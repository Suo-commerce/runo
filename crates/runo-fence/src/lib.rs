//! Grants, activations, the resource guard, task lifecycle and operation keys (technical spec §8).

use async_trait::async_trait;
use runo_core::{Epoch, ResourceId, SingerId, TaskId, Verse, VerseId};
use runo_store::{Fence, FenceAck};

/// A fencing failure.
#[derive(Debug, thiserror::Error)]
pub enum FenceError {
    /// A resource did not acknowledge; the grant stays unactivated.
    #[error("not activated; missing {0:?}")]
    NotActivated(Vec<ResourceId>),
    /// The task is closing or closed.
    #[error("task closing or closed")]
    Closing,
    /// Underlying store failure.
    #[error(transparent)]
    Store(#[from] runo_store::StoreError),
}

/// The outcome of a relaunch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Outcome {
    /// Activated: the replacement may start with this grant.
    Activated {
        /// The grant.
        grant: VerseId,
        /// The activation.
        activation: VerseId,
    },
    /// Not activated: some resources did not acknowledge.
    NotActivated {
        /// The grant, kept in the song.
        grant: VerseId,
        /// Resources still missing.
        missing: Vec<ResourceId>,
    },
}

/// Anything that accepts writes on behalf of a task.
#[async_trait]
pub trait FencedResource: Send + Sync {
    /// Which resource.
    fn id(&self) -> ResourceId;
    /// Persist `(singer, task) -> max(fence, existing)` with the grant id; never lowers.
    async fn install(
        &self,
        singer: &SingerId,
        task: &TaskId,
        fence: Fence,
        grant: VerseId,
    ) -> Result<FenceAck, FenceError>;
}

/// The atomic check every write path calls inside the same critical section as the write.
#[derive(Default)]
pub struct FenceGuard {
    _private: (),
}

impl FenceGuard {
    /// Admit a write if its grant is installed here and an activation for it is known.
    ///
    /// # Errors
    /// Returns `FenceError` on a stale epoch, unknown grant or closed task.
    pub fn admit(
        &self,
        _singer: &SingerId,
        _task: &TaskId,
        _epoch: Epoch,
        _grant: &VerseId,
        _activation: &VerseId,
    ) -> Result<(), FenceError> {
        todo!("milestone 5: runo-fence guard")
    }
}

/// A stable logical operation key for external effects, deduplicated across epochs.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct OpKey {
    /// The task.
    pub task: TaskId,
    /// The operation, chosen when first attempted.
    pub operation_id: String,
}

/// Consumer-side deduplication of an external effect.
pub trait Dedup: Send + Sync {
    /// Has this operation already taken effect?
    fn seen(&self, key: &OpKey) -> bool;
    /// Record that it has.
    fn mark(&self, key: &OpKey);
}

/// Marker for the allocator's side; `relaunch` and `close` are implemented in milestone 5.
pub struct Allocator;

impl Allocator {
    /// The relaunch sequence (technical spec §8, steps 1–8).
    ///
    /// # Errors
    /// Returns `FenceError` if the task is closing or storage fails.
    pub fn relaunch(&self, _singer: &SingerId, _task: &TaskId) -> Result<Outcome, FenceError> {
        todo!("milestone 5: runo-fence relaunch")
    }

    /// Closure: `Open -> Closing -> Closed`, fixed grant set, snapshot, `Close` verse.
    ///
    /// # Errors
    /// Returns `FenceError` if storage fails.
    pub fn close(&self, _task: &TaskId) -> Result<Verse, FenceError> {
        todo!("milestone 5: runo-fence close")
    }
}
