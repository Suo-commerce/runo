//! `runo watch`'s loop body (technical spec §15, Watch): pages in, messages out.
//!
//! Each [`Watch::step`] takes one fresh page of issues and runs and:
//!
//! 1. turns them into new board events ([`Feed`]), noting any gap;
//! 2. replays those events through the herald in board-time order, ticking the clocks up to each
//!    event, then to `now`, so a stall is timed by when things happened on the board, not by
//!    when the herald happened to look;
//! 3. records every raised and cleared signal in the episode ledger, keyed by the stall's start,
//!    so a restart that replays the same history finds the episodes it already has;
//! 4. on the first step only, settles history: episodes that resolved before the herald started
//!    are recorded but never sent (Jarmo, 4 Nov 2026); anything still stuck now is owed;
//! 5. saves the ledger, then returns the messages owed. The caller delivers each and calls
//!    [`Watch::delivered`], which saves again, so a crash between the two resends the same
//!    message under the same key, never a new one.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use runo_clock::{Cleared, Level};
use runo_core::time::Instant;
use runo_core::{Clock, Timestamp, VerseId};
use runo_node::episodes::{Episode, EpisodeError, Episodes, Notice};

use crate::board::{Agent, BLOCKER, Issue, Run, blocker_of};
use crate::herald::{FAILED_RUN, Herald, HeraldConfig, Signal};
use crate::poll::{Feed, Gap, List};
use crate::time;

/// Resolved episodes kept in the ledger beyond those still owed a notice: enough history for the
/// view and the measures, small enough for a nearly full disk (HODD Bob was at 95% on 3 Nov).
pub const KEEP_RESOLVED: usize = 500;

/// Rows the view lists before folding the rest into a count: the view fits one screen
/// (technical spec §15).
pub const VIEW_ROWS: usize = 25;

/// One row of the view: a lane, or the lanes one blocker holds.
enum Row<'a> {
    One(&'a Episode),
    Blocks(&'a str, Vec<&'a Episode>),
}

impl<'a> Row<'a> {
    fn lanes(&self) -> Vec<&'a Episode> {
        match self {
            Row::One(e) => vec![e],
            Row::Blocks(_, lanes) => lanes.clone(),
        }
    }
}

/// The view's rows, by usefulness: failed runs; then waits grouped by the issue that blocks them,
/// the blocker holding the most lanes first (unblock it and they all move); then every other lane,
/// worst level and longest overdue first.
fn rows<'a>(open: &[&'a Episode]) -> Vec<Row<'a>> {
    let by_age = |a: &&Episode, b: &&Episode| {
        b.level
            .cmp(&a.level)
            .then(a.opened_at.0.cmp(&b.opened_at.0))
    };
    let mut failed: Vec<&Episode> = open
        .iter()
        .copied()
        .filter(|e| e.reason.starts_with(FAILED_RUN))
        .collect();
    failed.sort_by(by_age);
    let mut groups: BTreeMap<&str, Vec<&Episode>> = BTreeMap::new();
    let mut singles = Vec::new();
    for e in open
        .iter()
        .copied()
        .filter(|e| !e.reason.starts_with(FAILED_RUN))
    {
        match blocker_of(&e.reason).filter(|_| e.id.clock == Clock::Waiting) {
            Some(b) => groups.entry(b).or_default().push(e),
            None => singles.push(e),
        }
    }
    let mut blocks = Vec::new();
    for (b, mut lanes) in groups {
        if lanes.len() == 1 {
            singles.extend(lanes);
        } else {
            lanes.sort_by(|a, b| a.opened_at.0.cmp(&b.opened_at.0));
            blocks.push((b, lanes));
        }
    }
    blocks.sort_by(|(_, a), (_, b)| {
        b.len()
            .cmp(&a.len())
            .then(a[0].opened_at.0.cmp(&b[0].opened_at.0))
    });
    singles.sort_by(by_age);
    failed
        .into_iter()
        .map(Row::One)
        .chain(blocks.into_iter().map(|(b, l)| Row::Blocks(b, l)))
        .chain(singles.into_iter().map(Row::One))
        .collect()
}

/// A message owed to a person.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Outgoing {
    /// The notice, for the channel's idempotency key and for [`Watch::delivered`].
    pub notice: Notice,
    /// The words.
    pub text: String,
}

/// The herald, its feed and its episode ledger, persisted in one directory.
pub struct Watch {
    herald: Herald,
    feed: Feed,
    episodes: Episodes,
    path: PathBuf,
    base: String,
    prefix: Option<String>,
    names: BTreeMap<String, String>,
    started: bool,
    settled: usize,
    gaps: Vec<Gap>,
}

impl Watch {
    /// Open the ledger in `dir` (created if missing). `base` is the board's address, for links.
    ///
    /// # Errors
    /// The directory cannot be created, or the ledger exists and cannot be read.
    pub fn open(config: HeraldConfig, dir: &Path, base: &str) -> Result<Self, EpisodeError> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join("episodes.json");
        Ok(Self {
            herald: Herald::new(config, runo_core::SingerId("herald".into())),
            feed: Feed::default(),
            episodes: Episodes::load(&path)?,
            path,
            base: base.trim_end_matches('/').to_owned(),
            prefix: None,
            names: BTreeMap::new(),
            started: false,
            settled: 0,
            gaps: Vec::new(),
        })
    }

    /// The company's issue prefix (such as `HOD`), so links open the board's own page:
    /// `<board>/HOD/issues/HOD-3971`.
    pub fn set_prefix(&mut self, prefix: &str) {
        self.prefix = Some(prefix.trim_matches('/').to_owned()).filter(|p| !p.is_empty());
    }

    /// Agent names for messages and the view.
    pub fn set_agents(&mut self, agents: &[Agent]) {
        self.names = agents
            .iter()
            .map(|a| (a.id.clone(), a.name.clone()))
            .collect();
    }

    /// Take one fresh page of each list (fetched with page size `page`), plus every open issue,
    /// at wall time `now`, and
    /// return the messages owed, oldest stall first.
    ///
    /// # Errors
    /// The ledger cannot be saved; nothing is returned to send until it can be.
    pub fn step(
        &mut self,
        issues: &[Issue],
        open: &[Issue],
        runs: &[Run],
        page: usize,
        now: Instant,
    ) -> Result<Vec<Outgoing>, EpisodeError> {
        for i in issues.iter().chain(open) {
            self.herald
                .learn(&i.id, i.identifier.as_deref().unwrap_or(&i.id));
        }
        let (events, gaps) = self.feed.take(issues, open, runs, page);
        self.gaps.extend(gaps);
        for ev in &events {
            let ticked = self.herald.tick(ev.at());
            self.record(ticked);
            let said = self.herald.ingest(ev);
            self.record(said);
        }
        let ticked = self.herald.tick(now);
        self.record(ticked);
        if !self.started {
            self.started = true;
            self.settled = self.episodes.settle_resolved();
        }
        self.episodes.prune(KEEP_RESOLVED);
        self.episodes.save(&self.path)?;
        Ok(self
            .episodes
            .pending()
            .into_iter()
            .map(|notice| Outgoing {
                text: self.text(&notice),
                notice,
            })
            .collect())
    }

    /// A message was delivered.
    ///
    /// # Errors
    /// The ledger cannot be saved; the message will be offered again, under the same key.
    pub fn delivered(&mut self, n: &Notice) -> Result<(), EpisodeError> {
        self.episodes.delivered(n);
        self.episodes.save(&self.path)
    }

    fn record(&mut self, signals: Vec<Signal>) {
        for s in signals {
            match s {
                Signal::Raised {
                    lane,
                    clock,
                    level,
                    reason,
                    since,
                    at,
                    overdue_by,
                } => {
                    let id = format!("{}\n{}\n{clock:?}\n{}", lane.singer.0, lane.task.0, since.0);
                    let opening = VerseId(runo_core::hash::digest(id.as_bytes()));
                    // When it became overdue: deterministic on replay, and what the
                    // within-the-hour measure counts from.
                    let crossed =
                        Instant(at.0.saturating_sub(overdue_by.0.saturating_mul(1_000_000)));
                    let at = Timestamp(time::format(crossed));
                    self.episodes
                        .escalated(&lane, clock, level, opening, &at, &reason);
                }
                Signal::Cleared {
                    lane,
                    clock,
                    why,
                    at,
                } => {
                    self.episodes
                        .resolved(&lane, clock, why, &Timestamp(time::format(at)));
                }
            }
        }
    }

    fn agent<'a>(&'a self, id: &'a str) -> &'a str {
        self.names.get(id).map_or(id, String::as_str)
    }

    /// The one-message wording of a notice.
    fn text(&self, n: &Notice) -> String {
        let lane = &n.id.lane;
        let cleared = n.resolved.map_or(String::new(), |why| {
            format!(" (already cleared: {})", cleared_word(why))
        });
        format!(
            "{} · {} · {} {}: {}{cleared}\n  {}",
            lane.task.0,
            self.agent(&lane.singer.0),
            clock_word(n.id.clock),
            level_word(n.level),
            n.reason,
            self.link(&lane.task.0)
        )
    }

    fn link(&self, issue: &str) -> String {
        match &self.prefix {
            Some(p) => format!("{}/{p}/issues/{issue}", self.base),
            None => format!("{}/issues/{issue}", self.base),
        }
    }

    /// One row of the view.
    fn row_line(&self, row: &Row<'_>, now: Instant) -> String {
        let age = |e: &Episode| {
            time::parse(&e.opened_at.0).map_or(String::new(), |t| span(now.0.saturating_sub(t.0)))
        };
        match row {
            Row::One(e) => format!(
                "  {:<14} {:<12} {:<10} {:<16} {:<9} {}",
                level_word(e.level),
                clock_word(e.id.clock),
                e.id.lane.task.0,
                self.agent(&e.id.lane.singer.0),
                age(e),
                e.reason
            ),
            Row::Blocks(blocker, lanes) => {
                let worst = lanes.iter().map(|e| e.level).max().unwrap_or(Level::Quiet);
                let oldest = lanes.first().map(|e| age(e)).unwrap_or_default();
                let mut names: Vec<&str> =
                    lanes.iter().map(|e| e.id.lane.task.0.as_str()).collect();
                let more = names.len().saturating_sub(6);
                names.truncate(6);
                let more = if more > 0 {
                    format!(" +{more}")
                } else {
                    String::new()
                };
                let why = lanes[0].reason.split(BLOCKER).next().unwrap_or_default();
                format!(
                    "  {:<14} {:<12} {blocker} holds {} lanes ({}{more}), oldest {oldest}: {why}",
                    level_word(worst),
                    "waiting",
                    lanes.len(),
                    names.join(" ")
                )
            }
        }
    }

    /// The triage view at `now`, one screen: open episodes worst first, then what was settled
    /// as history, any gaps, and the one honest limitation of Watch mode.
    #[must_use]
    pub fn view(&self, now: Instant) -> String {
        let mut v = String::new();
        let lanes = self.herald.chorus().rows(now).len();
        let _ = writeln!(v, "runo watch · {} · {}", self.base, time::format(now));
        let _ = writeln!(
            v,
            "{lanes} lanes in progress · reachability unknown (the board does not say whether a worker responds)"
        );
        let open: Vec<&Episode> = self.episodes.open().collect();
        let rows = rows(&open);
        let _ = writeln!(
            v,
            "\nOpen lanes: {}, worst first; waits are grouped by the issue that blocks them, with how long each has been overdue:",
            open.len()
        );
        if open.is_empty() {
            let _ = writeln!(v, "  nothing overdue");
        }
        for row in rows.iter().take(VIEW_ROWS) {
            let _ = writeln!(v, "{}", self.row_line(row, now));
        }
        if let Some(rest) = rows.get(VIEW_ROWS..).filter(|r| !r.is_empty()) {
            let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
            let mut lanes = 0;
            for row in rest {
                for e in row.lanes() {
                    *kinds.entry(clock_word(e.id.clock)).or_default() += 1;
                    lanes += 1;
                }
            }
            let kinds: Vec<String> = kinds.iter().map(|(k, n)| format!("{n} {k}")).collect();
            let _ = writeln!(
                v,
                "  … and {lanes} more lanes in {} rows: {}",
                rest.len(),
                kinds.join(", ")
            );
        }
        let resolved = self.episodes.all().iter().filter(|e| !e.is_open()).count();
        let _ = writeln!(
            v,
            "\nResolved: {resolved} ({} before the herald started, kept as history and not sent)",
            self.settled
        );
        if self.gaps.is_empty() {
            let _ = writeln!(v, "Gaps: none");
        }
        for g in &self.gaps {
            let list = match g.list {
                List::Issues => "issues",
                List::Runs => "runs",
            };
            let _ = writeln!(
                v,
                "paperclip-gap: {list} changed between {} and {} were not all seen",
                time::format(g.after),
                time::format(g.before)
            );
        }
        let _ = writeln!(
            v,
            "\nWatch mode infers from inside the board it watches and is a single point of failure until a sentinel joins."
        );
        v
    }
}

fn clock_word(c: Clock) -> &'static str {
    match c {
        Clock::Reachability => "reachability",
        Clock::Progress => "no progress",
        Clock::Task => "task",
        Clock::Waiting => "waiting",
    }
}

fn level_word(l: Level) -> &'static str {
    match l {
        Level::Quiet => "quiet",
        Level::Crossed => "crossed",
        Level::Called => "overdue",
        Level::CalledAgain => "still overdue",
        Level::Suspected => "needs a person",
    }
}

fn cleared_word(c: Cleared) -> &'static str {
    match c {
        Cleared::Evidence => "it moved again",
        Cleared::Vouched => "vouched for",
        Cleared::Completed => "finished",
        Cleared::Relaunched => "relaunched",
        Cleared::Ruled => "the owner ruled",
    }
}

/// `2h05m`, `35m`, or `40s`.
fn span(nanos: u64) -> String {
    let s = nanos / 1_000_000_000;
    match (s / 3600, s % 3600 / 60) {
        (0, 0) => format!("{s}s"),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h{m:02}m"),
    }
}
