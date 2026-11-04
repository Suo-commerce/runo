//! The call ladder for a chorus node (technical spec §6): crossings become durable episodes, and a
//! person hears about each episode once per level, never once per tick.
//!
//! - `Crossed` is the wait-one-cadence step: the peer gets one more cadence, nobody is told.
//! - `Called`, `CalledAgain`: one message each.
//! - `Suspected`: one message, and the node signs a `Suspect` verse about the peer
//!   ([`Voice::suspect`]). It does nothing else; relaunch is the recovery authority's call.
//! - When a delivered episode clears (the peer is heard again, its boot session was replaced),
//!   one more message says so.
//!
//! An episode is named by the peer's last verse before the silence, so the same silence after a
//! restart is the same episode and is not announced again. The ledger is the same
//! [`Episodes`](crate::episodes::Episodes) Watch mode uses.

use std::path::PathBuf;

use runo_clock::{Cleared, ClockState, Escalation, Level};
use runo_core::{Clock, Timestamp};

use crate::episodes::{EpisodeError, Episodes, Lane, Notice};
use crate::fleet::span;
use crate::voice::{Heard, Voice};
use runo_core::time::Instant;

/// One message owed to a person.
#[derive(Clone, Debug)]
pub struct Outgoing {
    /// What it is about; the channel's idempotency key comes from it.
    pub notice: Notice,
    /// The message, in one line.
    pub text: String,
}

/// The ledger and the messages it owes.
pub struct Ladder {
    episodes: Episodes,
    path: Option<PathBuf>,
    cleared: Vec<Outgoing>,
}

/// The first words of a message: who, and how it stands.
fn headline(peer: &str, clock: Clock, level: Level, past: bool) -> String {
    let (is, still) = if past {
        ("was", "was still")
    } else {
        ("is", "is still")
    };
    match (clock, level) {
        (Clock::Reachability, Level::Suspected) => {
            format!("{peer} may be down (a Suspect verse was signed)")
        }
        (Clock::Reachability, Level::CalledAgain) => format!("{peer} {still} quiet"),
        (Clock::Reachability, _) => format!("{peer} {is} quiet"),
        (Clock::Waiting, _) => format!("{peer} {is} waiting too long"),
        (Clock::Progress, _) => format!("{peer} {is} not moving"),
        (Clock::Task, _) => format!("{peer} {is} past its promise"),
    }
}

fn cleared_word(c: Cleared) -> &'static str {
    match c {
        Cleared::Evidence => "heard again",
        Cleared::Vouched => "vouched for by another voice",
        Cleared::Completed => "its session ended (a new boot, or it said goodbye)",
        Cleared::Relaunched => "relaunched",
        Cleared::Ruled => "the owner ruled",
    }
}

impl Ladder {
    /// The ledger at `path` (created on first save), or in memory only.
    ///
    /// # Errors
    /// The file exists and cannot be read.
    pub fn open(path: Option<PathBuf>) -> Result<Self, EpisodeError> {
        let episodes = match &path {
            Some(p) => {
                if let Some(dir) = p.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                Episodes::load(p)?
            }
            None => Episodes::default(),
        };
        Ok(Self {
            episodes,
            path,
            cleared: Vec::new(),
        })
    }

    /// Take what a probe heard into the ledger. Returns the crossings that newly reached
    /// `Suspected`, for which the caller sings `Suspect`.
    pub fn record<
        S: runo_store::VerseReader + runo_store::VerseWriter + runo_store::RefrainSlot + ?Sized,
    >(
        &mut self,
        voice: &Voice<S>,
        heard: &Heard,
        now: Instant,
        wall: &Timestamp,
    ) -> Vec<Escalation> {
        let rows = voice.rows(now);
        let mut suspect = Vec::new();
        for e in &heard.escalations {
            let Some(task) = &e.task else { continue };
            if e.level < Level::Called {
                continue;
            }
            let Some(opening) = voice.head(&e.peer, task) else {
                continue; // never heard in this run: nothing to name the silence by
            };
            let row = rows.iter().find(|r| r.peer == e.peer && r.task == *task);
            let elapsed = row.and_then(|r| match r.clocks[clock_index(e.clock)] {
                ClockState::Running { elapsed, .. } => Some(elapsed),
                _ => None,
            });
            let state = row.and_then(|r| r.state.clone()).unwrap_or_default();
            let reason = match (e.clock, elapsed) {
                (Clock::Reachability, Some(t)) => {
                    format!("no word for {}; it last said {state}", span(t.0))
                }
                (Clock::Waiting, Some(t)) => format!(
                    "waiting on {} for {}",
                    row.and_then(|r| r.blocked_on.clone()).unwrap_or_default(),
                    span(t.0)
                ),
                _ => format!("overdue by {}", span(e.overdue_by.0)),
            };
            let lane = Lane {
                singer: e.peer.clone(),
                task: task.clone(),
            };
            let change = self
                .episodes
                .escalated(&lane, e.clock, e.level, opening, wall, &reason);
            if e.level == Level::Suspected && change != crate::episodes::Change::Unchanged {
                suspect.push(e.clone());
            }
        }
        for r in &heard.resolutions {
            let lane = Lane {
                singer: r.peer.clone(),
                task: r.task.clone(),
            };
            let told = self
                .episodes
                .open()
                .find(|e| e.id.lane == lane && e.id.clock == r.clock)
                .and_then(|e| {
                    e.delivered
                        .map(|level| (e.id.clone(), level, e.reason.clone()))
                });
            if self.episodes.resolved(&lane, r.clock, r.why, wall)
                && let Some((id, level, reason)) = told
            {
                let notice = Notice {
                    id,
                    level,
                    resolved: Some(r.why),
                    reason,
                    cleared: true,
                };
                let text = format!("{} is fine: {}", lane.singer.0, cleared_word(r.why));
                self.cleared.push(Outgoing { notice, text });
            }
        }
        suspect
    }

    /// Messages owed, oldest episode first: raised episodes, then clearances.
    #[must_use]
    pub fn owed(&self) -> Vec<Outgoing> {
        let mut out: Vec<Outgoing> = self
            .episodes
            .pending()
            .into_iter()
            .map(|n| {
                let peer = &n.id.lane.singer.0;
                let text = match n.resolved {
                    Some(why) => format!(
                        "{}: {} (since then: {})",
                        headline(peer, n.id.clock, n.level, true),
                        n.reason,
                        cleared_word(why)
                    ),
                    None => format!(
                        "{}: {}",
                        headline(peer, n.id.clock, n.level, false),
                        n.reason
                    ),
                };
                Outgoing { notice: n, text }
            })
            .collect();
        out.extend(self.cleared.iter().cloned());
        out
    }

    /// A message was delivered.
    ///
    /// # Errors
    /// The ledger cannot be saved; the message will be offered again under the same key.
    pub fn delivered(&mut self, o: &Outgoing) -> Result<(), EpisodeError> {
        if o.notice.cleared {
            self.cleared.retain(|c| c.notice.key() != o.notice.key());
        } else {
            self.episodes.delivered(&o.notice);
        }
        self.episodes.prune(KEEP_RESOLVED);
        match &self.path {
            Some(p) => self.episodes.save(p),
            None => Ok(()),
        }
    }
}

/// Resolved episodes kept in the ledger, newest first.
const KEEP_RESOLVED: usize = 500;

fn clock_index(c: Clock) -> usize {
    match c {
        Clock::Reachability => 0,
        Clock::Progress => 1,
        Clock::Task => 2,
        Clock::Waiting => 3,
    }
}
