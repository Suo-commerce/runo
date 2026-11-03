//! The four clocks, escalation and suspicion (technical spec §5). A pure state machine: feed it
//! observations and a monotonic `now`, read back clock states and escalations.

pub mod escalate;

use runo_core::time::Instant;
use runo_core::{About, Clock, Millis, Refrain, SingerId, TaskId, Verse};

/// One observation about a peer.
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
}

/// Suspicion level per clock.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
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

/// Clock state for every peer this voice listens to.
#[derive(Default)]
pub struct Chorus {
    _private: (),
}

impl Chorus {
    /// Record an observation at `now`. Implemented in milestone 3.
    pub fn observe(&mut self, _now: Instant, _from: &SingerId, _o: Observation<'_>) {
        todo!("milestone 3: runo-clock observe")
    }

    /// Advance to `now` and return the crossings since the last tick. Implemented in milestone 3.
    pub fn tick(&mut self, _now: Instant) -> Vec<Escalation> {
        todo!("milestone 3: runo-clock tick")
    }
}
