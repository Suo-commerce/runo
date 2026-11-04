//! The fleet view (`runo view`): one line per machine, what it last declared, when it was last
//! heard, and whether that silence is expected. Problems first. The listener principle (Song spec,
//! principle 14): the line should need no explanation beyond the command.
//!
//! The view reads the store like a voice does, through the [`Listener`], so a forged verse is
//! counted as an incident and never moves a machine's line. It judges silence with each machine's
//! own profile: reachability is expected within the profile's `reachability` cadence (or twice its
//! refrain interval), or within the cadence of a declared state it does not refrain in (`sleep`).
//!
//! **Times are the singers' own** (`sung_at`, `sent_at`): the view has no arrival times of its
//! own, unlike a node's clocks. A machine whose clock runs ahead looks fresher than it is; a time
//! in the future counts as now.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use runo_clock::Level;
use runo_clock::escalate::{level_at, threshold_of};
use runo_core::time::Instant;
use runo_core::time::wall;
use runo_core::{Cadence, Millis, Payload, SingerId, TaskId};
use runo_profile::Profile;
use runo_store::{RefrainSlot, StoreError, VerseReader};

use crate::listener::{Incident, Listener};
use crate::voice::{VoiceError, clock_config};

/// What one machine's line says.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// Silent past its expectation, at this level.
    Quiet {
        /// How far up the ladder the silence is.
        level: Level,
        /// The expectation it is past.
        expected: Millis,
    },
    /// In the roster but never heard.
    NeverSung,
    /// Up, and waiting on something it named.
    Waiting(String),
    /// Heard within its expectation.
    Up,
    /// In a declared silent state (`sleep`), within that state's cadence.
    Resting {
        /// How long the state may last before it is called.
        expected: Millis,
    },
    /// It declared a terminal state (`going-offline`).
    Offline,
}

impl Verdict {
    fn rank(&self) -> (u8, u8) {
        match self {
            Self::Quiet { level, .. } => (0, 9 - *level as u8),
            Self::NeverSung => (1, 0),
            Self::Waiting(_) => (2, 0),
            Self::Up => (3, 0),
            Self::Resting { .. } => (4, 0),
            Self::Offline => (5, 0),
        }
    }
}

/// One machine's line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Line {
    /// The machine.
    pub singer: SingerId,
    /// Its role.
    pub role: String,
    /// Its current boot session.
    pub session: Option<TaskId>,
    /// What it last declared.
    pub state: Option<String>,
    /// When it declared that (its own clock).
    pub since: Option<Instant>,
    /// When it was last heard, by verse or refrain (its own clock).
    pub heard: Option<Instant>,
    /// The judgement.
    pub verdict: Verdict,
}

#[derive(Default)]
struct Machine {
    session: Option<TaskId>,
    state: Option<String>,
    blocked_on: Option<String>,
    since: Option<Instant>,
    heard: Option<Instant>,
}

impl Machine {
    fn heard_at(&mut self, at: Option<Instant>) {
        if at > self.heard {
            self.heard = at;
        }
    }
}

/// The fleet, kept current by [`Fleet::update`].
pub struct Fleet {
    listener: Listener,
    profiles: BTreeMap<SingerId, Profile>,
    machines: BTreeMap<SingerId, Machine>,
    incidents: Vec<Incident>,
}

impl Fleet {
    /// A view of the roster's voices, each judged by its own profile. Voices without a cadence
    /// (the owner) are left out.
    ///
    /// # Errors
    /// A profile's cadence is unusable.
    pub fn new(
        listener: Listener,
        profiles: BTreeMap<SingerId, Profile>,
    ) -> Result<Self, VoiceError> {
        for p in profiles.values() {
            clock_config(p, listener.roster())?;
        }
        Ok(Self {
            listener,
            profiles,
            machines: BTreeMap::new(),
            incidents: Vec::new(),
        })
    }

    /// Read what is new in the store.
    ///
    /// # Errors
    /// The store fails.
    pub async fn update<S: VerseReader + RefrainSlot + ?Sized>(
        &mut self,
        store: &S,
    ) -> Result<(), StoreError> {
        let admitted = self.listener.read(store).await?;
        self.incidents.extend(admitted.incidents);
        for v in &admitted.current {
            let at = wall::parse(&v.header.sung_at.0);
            let m = self.machines.entry(v.header.singer.clone()).or_default();
            if m.session.as_ref() != Some(&v.header.task) {
                if !matches!(v.payload, Payload::Claimed { .. }) {
                    continue; // a late verse from an older session
                }
                *m = Machine {
                    session: Some(v.header.task.clone()),
                    ..Machine::default()
                };
            }
            m.heard_at(at);
            if let Payload::Boundary {
                event, blocked_on, ..
            } = &v.payload
            {
                m.state = Some(event.clone());
                m.blocked_on.clone_from(blocked_on);
                m.since = at;
            }
        }
        let (refrains, incidents) = self.listener.refrains(store, |_| false).await?;
        self.incidents.extend(incidents);
        for r in refrains {
            if let Some(m) = self.machines.get_mut(&r.singer)
                && m.session.as_ref() == Some(&r.task)
            {
                m.heard_at(wall::parse(&r.sent_at.0));
            }
        }
        Ok(())
    }

    /// Records refused at ingress since the view started.
    #[must_use]
    pub fn incidents(&self) -> &[Incident] {
        &self.incidents
    }

    /// The lines at wall time `now`, problems first.
    #[must_use]
    pub fn lines(&self, now: Instant) -> Vec<Line> {
        let mut out: Vec<Line> = self
            .listener
            .roster()
            .voices
            .iter()
            .filter_map(|v| {
                let p = self.profiles.get(&v.singer)?;
                let config = clock_config(p, self.listener.roster()).ok()?;
                let reach = config.reachability?;
                let m = self.machines.get(&v.singer);
                let verdict = m.map_or(Verdict::NeverSung, |m| {
                    judge(m, p, &config.declared, reach, now)
                });
                Some(Line {
                    singer: v.singer.clone(),
                    role: v.role.clone(),
                    session: m.and_then(|m| m.session.clone()),
                    state: m.and_then(|m| m.state.clone()),
                    since: m.and_then(|m| m.since),
                    heard: m.and_then(|m| m.heard),
                    verdict,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            a.verdict
                .rank()
                .cmp(&b.verdict.rank())
                .then(a.singer.cmp(&b.singer))
        });
        out
    }
}

fn judge(
    m: &Machine,
    p: &Profile,
    declared: &BTreeMap<String, Cadence>,
    reach: Cadence,
    now: Instant,
) -> Verdict {
    let Some(state) = &m.state else {
        return Verdict::NeverSung;
    };
    if p.states.terminal.contains(state) {
        return Verdict::Offline;
    }
    let cadence = declared.get(state).copied().unwrap_or(reach);
    let elapsed = Millis(now.0.saturating_sub(m.heard.map_or(0, |h| h.0)) / 1_000_000);
    let (level, _) = level_at(&cadence, elapsed);
    if level > Level::Quiet {
        return Verdict::Quiet {
            level,
            expected: threshold_of(&cadence, Level::Crossed),
        };
    }
    match (declared.contains_key(state), &m.blocked_on) {
        (true, _) => Verdict::Resting {
            expected: cadence.call,
        },
        (false, Some(on)) => Verdict::Waiting(on.clone()),
        (false, None) => Verdict::Up,
    }
}

/// `12 min`, `3 h 05 min`, `2 d 4 h`.
#[must_use]
pub fn span(ms: u64) -> String {
    let m = ms / 60_000;
    match m {
        0 => "under a minute".into(),
        1..60 => format!("{m} min"),
        60..1440 => format!("{} h {:02} min", m / 60, m % 60),
        _ if m % 1440 < 60 => format!("{} d", m / 1440),
        _ => format!("{} d {} h", m / 1440, m % 1440 / 60),
    }
}

fn ago(now: Instant, at: Option<Instant>) -> String {
    at.map_or_else(
        || "never".into(),
        |t| format!("{} ago", span(now.0.saturating_sub(t.0) / 1_000_000)),
    )
}

fn clock(at: Option<Instant>) -> String {
    at.map_or_else(|| "?".into(), |t| wall::format(t)[11..16].to_owned())
}

/// The view as text: a header, one line per machine, and incidents if there were any.
#[must_use]
pub fn render(run: &str, lines: &[Line], incidents: &[Incident], now: Instant) -> String {
    let mut v = String::new();
    let _ = writeln!(
        v,
        "runo view · {run} · {} UTC",
        wall::format(now)[..16].replace('T', " ")
    );
    let width = lines.iter().map(|l| l.singer.0.len()).max().unwrap_or(0);
    for l in lines {
        let state = l.state.as_deref().unwrap_or("-");
        let what = match &l.verdict {
            Verdict::Quiet { level, expected } => format!(
                "QUIET {} · {:?} · expected within {} · last said {state} at {}",
                ago(now, l.heard),
                level,
                span(expected.0),
                clock(l.since)
            ),
            Verdict::NeverSung => "never heard since the roster was made".into(),
            Verdict::Waiting(on) => format!(
                "up, waiting on {on} since {} · heard {}",
                clock(l.since),
                ago(now, l.heard)
            ),
            Verdict::Up => format!(
                "up · {state} since {} · heard {}",
                clock(l.since),
                ago(now, l.heard)
            ),
            Verdict::Resting { expected } => format!(
                "{state} since {}, as it said · called if still silent after {}",
                clock(l.since),
                span(expected.0)
            ),
            Verdict::Offline => format!("offline since {}, as it said", clock(l.since)),
        };
        let _ = writeln!(v, "  {:<width$}  {what}", l.singer.0);
    }
    if incidents.is_empty() {
        let _ = writeln!(v, "incidents: none");
    } else {
        let _ = writeln!(
            v,
            "incidents: {} records refused (forged or broken), never believed:",
            incidents.len()
        );
        let mut by: BTreeMap<(&str, String), usize> = BTreeMap::new();
        for (s, why) in incidents {
            *by.entry((s.0.as_str(), format!("{why:?}"))).or_default() += 1;
        }
        for ((s, why), n) in by {
            let _ = writeln!(v, "  {n} × claiming to be {s}: {why}");
        }
    }
    v
}
