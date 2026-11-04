//! The four clocks, escalation and suspicion (technical spec §5). A pure state machine: feed it
//! observations and a monotonic `now`, read back clock states and escalations. It never reads
//! the wall clock, which is what lets the simulator drive it.
//!
//! State is kept per `(peer, task)`. Each of the four clocks has its own suspicion level; a
//! level rises only in [`Chorus::tick`] and falls only when an observation clears the clock
//! (new progress, a cleared block, a fresh refrain, completion, or an owner's ruling), each
//! fall reported once as a [`Resolution`].
//!
//! Choices where the spec leaves room, each named where it is made:
//! - **Unknown reachability** (Watch mode, technical spec §15): configure no reachability
//!   cadence and the clock reports `Unknown` and never escalates. It is shown, not switched off.
//! - **Waiting relaxes progress**: while blocked, progress does not escalate, and clearing the
//!   block restarts the progress clock.
//! - **A suspect heard from another voice** is counted as corroboration on the row; it does not
//!   change this voice's own levels (Song spec §5: each voice judges for itself).

pub mod escalate;

use std::collections::{BTreeMap, BTreeSet};

use runo_core::time::Instant;
use runo_core::{
    About, Cadence, Clock, Domains, Epoch, Hash, Millis, Payload, Promise, Refrain, SingerId,
    TaskId, Verse,
};

/// One observation about a peer.
#[derive(Clone, Copy)]
pub enum Observation<'a> {
    /// A refrain; reachability only, if `n` advanced.
    Refrain(&'a Refrain),
    /// A verse; reachability, progress if height/head advanced, task on `Claimed`.
    Verse(&'a Verse),
    /// A vouch from another voice; lowers reachability suspicion only, once.
    Vouch {
        /// Who is vouched for.
        about: &'a About,
        /// Who vouches.
        from: &'a SingerId,
    },
    /// A suspect heard from another voice.
    SuspectHeard {
        /// Who is suspected.
        about: &'a About,
        /// Which clock.
        clock: Clock,
        /// Who suspects.
        from: &'a SingerId,
    },
    /// Evidence about a lane inferred from a board by a herald (Watch mode, technical spec §15).
    /// It drives the same clock rules as the lane's own verses, but it is never a verse in the
    /// lane's name and never proves reachability.
    Inferred {
        /// The lane's singer.
        singer: &'a SingerId,
        /// The lane's task.
        task: &'a TaskId,
        /// What the board showed.
        what: Evidence<'a>,
    },
}

/// What a board can show about a lane, in clock terms.
#[derive(Clone, Copy, Debug)]
pub enum Evidence<'a> {
    /// Work started under this (advisory) promise; like `Claimed`. Watch has no epochs.
    Started(&'a Promise),
    /// New work appeared; `mark` must grow for it to count, like a verse's height.
    Progressed(u64),
    /// The lane waits on something, named.
    Blocked(&'a str),
    /// The wait ended.
    Unblocked,
    /// The work ended (done, cancelled, or taken off the board).
    Ended,
}

/// Suspicion level per clock.
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Debug,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Nothing overdue.
    #[default]
    Quiet,
    /// First threshold crossed; waiting one cadence for the lane's own next event.
    Crossed,
    /// Called.
    Called,
    /// Called again.
    CalledAgain,
    /// Suspected.
    Suspected,
}

impl Level {
    fn up(self) -> Level {
        match self {
            Level::Quiet => Level::Crossed,
            Level::Crossed => Level::Called,
            Level::Called => Level::CalledAgain,
            Level::CalledAgain | Level::Suspected => Level::Suspected,
        }
    }
    fn down(self) -> Level {
        match self {
            Level::Quiet | Level::Crossed => Level::Quiet,
            Level::Called => Level::Crossed,
            Level::CalledAgain => Level::Called,
            Level::Suspected => Level::CalledAgain,
        }
    }
}

/// A threshold crossing, emitted exactly once.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Escalation {
    /// Which peer.
    pub peer: SingerId,
    /// Which task, if any.
    pub task: Option<TaskId>,
    /// Which clock.
    pub clock: Clock,
    /// New level.
    pub level: Level,
    /// How far past the threshold, in milliseconds like every protocol duration.
    pub overdue_by: Millis,
}

/// Why a clock went back to `Quiet`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cleared {
    /// New progress (`Progress`), a fresh refrain or verse (`Reachability`), a cleared block
    /// (`Waiting`).
    Evidence,
    /// A vouch brought reachability suspicion back down.
    Vouched,
    /// A terminal boundary or `Close` ended the task.
    Completed,
    /// A newer epoch's claim replaced the task.
    Relaunched,
    /// The owner ruled on the lane.
    Ruled,
}

/// A clock that was above `Quiet` and is `Quiet` again: what resolves an episode (technical
/// spec §2, "Episodes").
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Resolution {
    /// Which peer.
    pub peer: SingerId,
    /// Which task.
    pub task: TaskId,
    /// Which clock.
    pub clock: Clock,
    /// Why.
    pub why: Cleared,
}

/// What this voice knows before it starts listening.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// The reachability cadence, or `None` where there is no direct evidence of reachability
    /// (Watch mode): the clock is then shown as unknown and never escalates.
    pub reachability: Option<Cadence>,
    /// The waiting cadence from the profile.
    pub waiting: Option<Cadence>,
    /// Boundary events that end a task (the profile's terminal states).
    pub terminal: BTreeSet<String>,
    /// The owner, whose `Ruling` about a lane resolves its clocks.
    pub owner: Option<SingerId>,
    /// Declared failure domains, for weighting vouches.
    pub domains: BTreeMap<SingerId, Domains>,
    /// The reachability cadence while a peer's last declared state is one of these, such as a
    /// machine's `sleep`: declared silence changes the expectation, it never switches the clock
    /// off (technical spec §14, milestone 4).
    pub declared: BTreeMap<String, Cadence>,
}

/// One clock as the view shows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClockState {
    /// Not running: no task, not blocked, or a promise without checkpoints.
    Idle,
    /// No direct evidence (Watch mode reachability).
    Unknown,
    /// Running.
    Running {
        /// Time on this clock.
        elapsed: Millis,
        /// Its suspicion level.
        level: Level,
    },
}

/// One `(peer, task)` row for the view.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Row {
    /// The peer.
    pub peer: SingerId,
    /// The task.
    pub task: TaskId,
    /// Its epoch.
    pub epoch: Epoch,
    /// Reachability, progress, task, waiting, in [`Clock`] order.
    pub clocks: [ClockState; 4],
    /// What it is blocked on, if anything.
    pub blocked_on: Option<String>,
    /// Its last declared state (the event of its latest boundary), if any.
    pub state: Option<String>,
    /// Voices that sang `Suspect` about it.
    pub suspected_by: BTreeSet<SingerId>,
}

const CLOCKS: [Clock; 4] = [
    Clock::Reachability,
    Clock::Progress,
    Clock::Task,
    Clock::Waiting,
];

fn idx(c: Clock) -> usize {
    match c {
        Clock::Reachability => 0,
        Clock::Progress => 1,
        Clock::Task => 2,
        Clock::Waiting => 3,
    }
}

fn ns(m: Millis) -> u64 {
    m.0.saturating_mul(1_000_000)
}

fn ms(nanos: u64) -> Millis {
    Millis(nanos / 1_000_000)
}

struct TaskClock {
    claimed_at: Instant,
    cadence: Cadence,
    checkpoints: bool,
    completed_waits: u64,
}

#[derive(Default)]
struct PeerClocks {
    epoch: Option<Epoch>,
    reach_since: Option<Instant>,
    last_refrain_n: Option<u64>,
    progress_since: Option<Instant>,
    last_progress: Option<(u64, Option<runo_core::Hash>)>,
    task: Option<TaskClock>,
    waiting_since: Option<Instant>,
    blocked_on: Option<String>,
    state: Option<String>,
    level: [Level; 4],
    vouch_weight: u8,
    vouches: Vec<(SingerId, Instant)>,
    suspected_by: BTreeSet<SingerId>,
}

/// Clock state for every peer this voice listens to.
#[derive(Default)]
pub struct Chorus {
    config: Config,
    peers: BTreeMap<(SingerId, TaskId), PeerClocks>,
    resolutions: Vec<Resolution>,
    replay: bool,
}

impl Chorus {
    /// A chorus with this voice's configuration.
    #[must_use]
    pub fn new(config: Config) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// Startup replay (technical spec §4): while on, verses rebuild chain state and waits, but
    /// never prove current reachability.
    pub fn set_replay(&mut self, on: bool) {
        self.replay = on;
    }

    /// Record an observation at `now`. The peer it is about is read from the observation itself;
    /// the voice it came through is kept in the signature for the transport's bookkeeping.
    pub fn observe(&mut self, now: Instant, _from: &SingerId, o: Observation<'_>) {
        match o {
            Observation::Refrain(r) => self.refrain(now, r),
            Observation::Verse(v) => self.verse(now, v),
            Observation::Inferred { singer, task, what } => match what {
                Evidence::Started(promise) => {
                    self.claimed(now, singer, task, Epoch(0), promise, (0, None));
                }
                Evidence::Progressed(mark) => self.progressed(now, singer, task, (mark, None)),
                Evidence::Blocked(on) => self.waits(now, singer, task, Some(on)),
                Evidence::Unblocked => self.waits(now, singer, task, None),
                Evidence::Ended => self.complete(singer, task, Cleared::Completed),
            },
            Observation::Vouch {
                about,
                from: voucher,
            } => self.vouch(now, about, voucher),
            Observation::SuspectHeard {
                about,
                from: suspecter,
                ..
            } => {
                for p in self.matching(about) {
                    p.suspected_by.insert(suspecter.clone());
                }
            }
        }
    }

    /// Advance to `now` and return the crossings since the last tick, one per level crossed.
    pub fn tick(&mut self, now: Instant) -> Vec<Escalation> {
        let mut out = Vec::new();
        let config = &self.config;
        for ((peer, task), p) in &mut self.peers {
            if apply_vouches(peer, p, now, config) {
                self.resolutions.push(Resolution {
                    peer: peer.clone(),
                    task: task.clone(),
                    clock: Clock::Reachability,
                    why: Cleared::Vouched,
                });
            }
            for clock in CLOCKS {
                let Some((elapsed, cadence)) = running(p, clock, now, config) else {
                    continue;
                };
                let (target, _) = escalate::level_at(&cadence, elapsed);
                while p.level[idx(clock)] < target {
                    let level = p.level[idx(clock)].up();
                    p.level[idx(clock)] = level;
                    let threshold = escalate::threshold_of(&cadence, level);
                    out.push(Escalation {
                        peer: peer.clone(),
                        task: Some(task.clone()),
                        clock,
                        level,
                        overdue_by: Millis(elapsed.0.saturating_sub(threshold.0)),
                    });
                }
            }
        }
        out
    }

    /// Clocks that went back to `Quiet` since the last call, in order.
    pub fn take_resolutions(&mut self) -> Vec<Resolution> {
        std::mem::take(&mut self.resolutions)
    }

    /// Every `(peer, task)` row with a task or an open suspicion at `now`, for the view.
    #[must_use]
    pub fn rows(&self, now: Instant) -> Vec<Row> {
        self.peers
            .iter()
            .filter(|(_, p)| p.task.is_some() || p.level.iter().any(|l| *l > Level::Quiet))
            .map(|((peer, task), p)| Row {
                peer: peer.clone(),
                task: task.clone(),
                epoch: p.epoch.unwrap_or(Epoch(0)),
                clocks: CLOCKS.map(|c| {
                    if c == Clock::Reachability && self.config.reachability.is_none() {
                        return ClockState::Unknown;
                    }
                    running(p, c, now, &self.config).map_or(ClockState::Idle, |(elapsed, _)| {
                        ClockState::Running {
                            elapsed,
                            level: p.level[idx(c)],
                        }
                    })
                }),
                blocked_on: p.blocked_on.clone(),
                state: p.state.clone(),
                suspected_by: p.suspected_by.clone(),
            })
            .collect()
    }

    // ---- observations ------------------------------------------------------------------------

    fn entry(&mut self, peer: &SingerId, task: &TaskId) -> &mut PeerClocks {
        self.peers.entry((peer.clone(), task.clone())).or_default()
    }

    fn matching(&mut self, about: &About) -> Vec<&mut PeerClocks> {
        self.peers
            .iter_mut()
            .filter(|((s, t), _)| *s == about.singer && about.task.as_ref().is_none_or(|a| a == t))
            .map(|(_, p)| p)
            .collect()
    }

    fn clear(&mut self, peer: &SingerId, task: &TaskId, clock: Clock, why: Cleared) {
        if let Some(p) = self.peers.get_mut(&(peer.clone(), task.clone()))
            && p.level[idx(clock)] > Level::Quiet
        {
            p.level[idx(clock)] = Level::Quiet;
            self.resolutions.push(Resolution {
                peer: peer.clone(),
                task: task.clone(),
                clock,
                why,
            });
        }
    }

    fn heard(&mut self, now: Instant, peer: &SingerId, task: &TaskId) {
        if self.replay {
            return;
        }
        let p = self.entry(peer, task);
        p.reach_since = Some(now);
        p.vouch_weight = 0;
        p.vouches.clear();
        self.clear(peer, task, Clock::Reachability, Cleared::Evidence);
    }

    fn refrain(&mut self, now: Instant, r: &Refrain) {
        let p = self.entry(&r.singer, &r.task);
        if Some(r.epoch) < p.epoch || p.last_refrain_n.is_some_and(|n| r.n <= n) {
            return;
        }
        p.last_refrain_n = Some(r.n);
        self.heard(now, &r.singer, &r.task);
    }

    fn verse(&mut self, now: Instant, v: &Verse) {
        let (peer, task) = (&v.header.singer, &v.header.task);
        if Some(v.header.epoch) < self.entry(peer, task).epoch {
            return; // an older incarnation: evidence, never a live observation
        }
        self.heard(now, peer, task);
        match &v.payload {
            Payload::Claimed { promise, .. } => {
                let mark = (v.header.progress.height, v.header.progress.head);
                self.claimed(now, peer, task, v.header.epoch, promise, mark);
            }
            Payload::Boundary {
                event, blocked_on, ..
            } => {
                self.progressed(
                    now,
                    peer,
                    task,
                    (v.header.progress.height, v.header.progress.head),
                );
                self.entry(peer, task).state = Some(event.clone());
                if self.config.terminal.contains(event) {
                    self.complete(peer, task, Cleared::Completed);
                } else {
                    self.waits(now, peer, task, blocked_on.as_deref());
                }
            }
            Payload::Work { head, .. } => {
                let head = head.or(v.header.progress.head);
                self.progressed(now, peer, task, (v.header.progress.height, head));
            }
            Payload::Close { task: closed, .. } => {
                let lanes: Vec<SingerId> = self
                    .peers
                    .keys()
                    .filter(|(_, t)| t == closed)
                    .map(|(s, _)| s.clone())
                    .collect();
                for lane in lanes {
                    self.complete(&lane, closed, Cleared::Completed);
                }
            }
            Payload::Ruling { about, .. } if Some(peer) == self.config.owner.as_ref() => {
                let tasks: Vec<TaskId> = self
                    .peers
                    .keys()
                    .filter(|(s, t)| {
                        *s == about.singer && about.task.as_ref().is_none_or(|a| a == t)
                    })
                    .map(|(_, t)| t.clone())
                    .collect();
                for t in tasks {
                    for c in CLOCKS {
                        self.clear(&about.singer, &t, c, Cleared::Ruled);
                    }
                }
            }
            // Peer and record kinds are reachability only; vouches and suspects arrive as their
            // own observations so the caller decides whose evidence they are.
            Payload::Ruling { .. }
            | Payload::Ack { .. }
            | Payload::Vouch { .. }
            | Payload::Suspect { .. }
            | Payload::Grant { .. }
            | Payload::Activation { .. }
            | Payload::Tome { .. }
            | Payload::Addendum { .. }
            | Payload::Roster { .. } => {}
        }
    }

    fn claimed(
        &mut self,
        now: Instant,
        peer: &SingerId,
        task: &TaskId,
        epoch: Epoch,
        promise: &Promise,
        mark: (u64, Option<Hash>),
    ) {
        let relaunch = {
            let p = self.entry(peer, task);
            let relaunch = p.task.is_some() && Some(epoch) > p.epoch;
            if p.task.is_some() && Some(epoch) <= p.epoch {
                return; // the task clock never resets within an epoch
            }
            relaunch
        };
        if relaunch {
            for c in [Clock::Progress, Clock::Task, Clock::Waiting] {
                self.clear(peer, task, c, Cleared::Relaunched);
            }
        }
        let p = self.entry(peer, task);
        p.epoch = Some(epoch);
        p.task = Some(TaskClock {
            claimed_at: now,
            cadence: promise.cadence,
            checkpoints: promise.checkpoints,
            completed_waits: 0,
        });
        p.waiting_since = None;
        p.blocked_on = None;
        p.progress_since = Some(now);
        p.last_progress = Some(mark);
    }

    fn progressed(
        &mut self,
        now: Instant,
        peer: &SingerId,
        task: &TaskId,
        new: (u64, Option<Hash>),
    ) {
        let p = self.entry(peer, task);
        let advanced = p
            .last_progress
            .is_none_or(|(h, hd)| new.0 > h || (new.1.is_some() && new.1 != hd));
        if advanced {
            p.last_progress = Some(new);
            p.progress_since = Some(now);
            self.clear(peer, task, Clock::Progress, Cleared::Evidence);
        }
    }

    fn waits(&mut self, now: Instant, peer: &SingerId, task: &TaskId, blocked_on: Option<&str>) {
        let p = self.entry(peer, task);
        match blocked_on {
            Some(b) => {
                p.waiting_since.get_or_insert(now);
                p.blocked_on = Some(b.to_owned());
            }
            None => {
                if let Some(since) = p.waiting_since.take() {
                    if let Some(t) = p.task.as_mut() {
                        t.completed_waits = t
                            .completed_waits
                            .saturating_add(now.0.saturating_sub(since.0));
                    }
                    p.blocked_on = None;
                    p.progress_since = Some(now);
                    self.clear(peer, task, Clock::Waiting, Cleared::Evidence);
                    self.clear(peer, task, Clock::Progress, Cleared::Evidence);
                }
            }
        }
    }

    fn complete(&mut self, peer: &SingerId, task: &TaskId, why: Cleared) {
        for c in [Clock::Progress, Clock::Task, Clock::Waiting] {
            self.clear(peer, task, c, why);
        }
        let p = self.entry(peer, task);
        p.task = None;
        p.waiting_since = None;
        p.blocked_on = None;
    }

    fn vouch(&mut self, now: Instant, about: &About, voucher: &SingerId) {
        if *voucher == about.singer {
            return;
        }
        for p in self.matching(about) {
            p.vouches.push((voucher.clone(), now));
        }
    }
}

/// Elapsed time and cadence for a clock that is running, or `None`.
fn running(
    p: &PeerClocks,
    clock: Clock,
    now: Instant,
    config: &Config,
) -> Option<(Millis, Cadence)> {
    match clock {
        Clock::Reachability => {
            let cadence = p
                .state
                .as_ref()
                .and_then(|s| config.declared.get(s))
                .copied()
                .or(config.reachability)?;
            p.task.as_ref()?;
            Some((ms(now.0.saturating_sub(p.reach_since?.0)), cadence))
        }
        Clock::Progress => {
            let t = p.task.as_ref()?;
            if !t.checkpoints || p.waiting_since.is_some() {
                return None;
            }
            Some((ms(now.0.saturating_sub(p.progress_since?.0)), t.cadence))
        }
        Clock::Task => {
            let t = p.task.as_ref()?;
            let active = p.waiting_since.map_or(0, |w| now.0.saturating_sub(w.0));
            let elapsed = now
                .0
                .saturating_sub(t.claimed_at.0)
                .saturating_sub(t.completed_waits)
                .saturating_sub(active);
            Some((ms(elapsed), t.cadence))
        }
        Clock::Waiting => {
            let cadence = config.waiting?;
            Some((ms(now.0.saturating_sub(p.waiting_since?.0)), cadence))
        }
    }
}

/// How much a vouch counts, in halves: independent 2, sharing a declared domain 1, the same
/// domain set 0 (technical spec §5, "Independence weighting"). Undeclared counts as sharing.
fn weight(config: &Config, a: &SingerId, b: &SingerId) -> u8 {
    let (Some(x), Some(y)) = (config.domains.get(a), config.domains.get(b)) else {
        return 1;
    };
    let shared = [
        x.host == y.host,
        x.credential == y.credential,
        x.provider == y.provider,
        x.network == y.network,
    ];
    match shared.iter().filter(|s| **s).count() {
        0 => 2,
        4 => 0,
        _ => 1,
    }
}

/// Apply vouches newer than the last reachability evidence and younger than one cadence. Each
/// full weight (2 halves) lowers reachability suspicion one level, once; leftover weight carries
/// until the next direct evidence resets it. Lowering moves the evidence point so the clock reads
/// as the lowered level and climbs again from there: a vouch buys time, it does not stop the
/// clock. Returns whether it came down to `Quiet`.
fn apply_vouches(peer: &SingerId, p: &mut PeerClocks, now: Instant, config: &Config) -> bool {
    let Some(cadence) = config.reachability else {
        p.vouches.clear();
        return false;
    };
    let since = p.reach_since.map_or(0, |i| i.0);
    let fresh = now.0.saturating_sub(ns(cadence.call));
    let mut quieted = false;
    for (voucher, at) in std::mem::take(&mut p.vouches) {
        if at.0 < since || at.0 < fresh {
            continue;
        }
        p.vouch_weight = p
            .vouch_weight
            .saturating_add(weight(config, &voucher, peer));
        while p.vouch_weight >= 2 && p.level[0] > Level::Quiet {
            p.vouch_weight -= 2;
            p.level[0] = p.level[0].down();
            let back = ns(escalate::threshold_of(&cadence, p.level[0]));
            p.reach_since = Some(Instant(now.0.saturating_sub(back)));
            quieted = p.level[0] == Level::Quiet;
        }
    }
    quieted
}

#[cfg(test)]
mod tests;
