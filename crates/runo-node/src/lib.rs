//! The voice runtime (technical spec §6): sing, refrain, probe, call ladder, recovery handoff.
//! Everything outside the process goes through a trait, so a node runs unchanged under the simulator.

pub mod channel;
pub mod episodes;
pub mod identity;
pub mod run;
pub mod voice;

use async_trait::async_trait;
use runo_core::{Clock, Epoch, SingerId, TaskId};

/// A call from the ladder, in one line.
#[derive(Clone, Debug)]
pub struct Call {
    /// Which clock is overdue.
    pub clock: Clock,
    /// The promise, elapsed, and what has not moved, as one line.
    pub line: String,
}

/// A probe reply bound to a challenge.
#[derive(Clone, Debug)]
pub struct ProbeReply {
    /// The challenge echoed.
    pub challenge: [u8; 16],
    /// Who replied.
    pub singer: SingerId,
    /// Their current epoch.
    pub epoch: Epoch,
    /// Signature over challenge, singer and epoch.
    pub sig: [u8; 64],
}

/// Channel failure.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// The peer could not be reached.
    #[error("unreachable")]
    Unreachable,
    /// The channel itself failed.
    #[error("channel: {0}")]
    Channel(String),
}

/// The consumer's direct channel: never a verse.
#[async_trait]
pub trait CallChannel: Send + Sync {
    /// Deliver a call.
    async fn call(&self, to: &SingerId, msg: Call) -> Result<(), CallError>;
    /// Probe a peer directly with a fresh challenge.
    async fn probe(&self, to: &SingerId, challenge: [u8; 16]) -> Result<ProbeReply, CallError>;
    /// Ask another voice to probe a peer and vouch or suspect on our behalf.
    async fn ask_probe(&self, via: &SingerId, target: &SingerId) -> Result<(), CallError>;
}

/// A suspicion with its corroboration, handed to the recovery authority.
#[derive(Clone, Debug)]
pub struct SuspicionReport {
    /// Who.
    pub peer: SingerId,
    /// Which task.
    pub task: Option<TaskId>,
    /// Which clock.
    pub clock: Clock,
    /// Vouches and suspects held from others, by singer.
    pub corroboration: Vec<(SingerId, bool)>,
}

/// Authority failure.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The authority could not be reached; nothing is relaunched.
    #[error("authority unreachable")]
    Unreachable,
}

/// The fence allocator's door. Core: report only; it decides.
#[async_trait]
pub trait RecoveryAuthority: Send + Sync {
    /// Report a suspicion.
    async fn report(&self, s: SuspicionReport) -> Result<(), AuthError>;
}
