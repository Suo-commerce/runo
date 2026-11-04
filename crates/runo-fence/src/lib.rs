//! Grants, activations, the resource guard, task lifecycle, the allocator's journal and
//! operation keys (technical spec §8). The types a fence is spoken in come from `runo-core`.

use async_trait::async_trait;
use runo_core::fence::{Fence, FenceAck};
use runo_core::ingress::TaskState;
use runo_core::{Epoch, Hash, ResourceId, SingerId, TaskId, Verse, VerseId};
use runo_store::ResourceFenceStore;

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
    /// Persist `(singer, task) -> max(fence, existing)` with the id of the verse that authorizes
    /// it (a `Grant` for an epoch fence, the `Close` verse for `Closed`); never lowers. The ack
    /// names this resource, that verse, the fence now enforced and the durability reached.
    async fn install(
        &self,
        singer: &SingerId,
        task: &TaskId,
        fence: Fence,
        grant: VerseId,
    ) -> Result<FenceAck, FenceError>;
}

/// The song store as a fenced resource (technical decisions 3): the one adapter between
/// [`FencedResource::install`], which every protected resource implements, and the store's own
/// [`ResourceFenceStore::install_fence`], which takes the authorizing verse. The adapter fetches
/// that verse by id, checks it is a `Grant` for `(singer, task)` at the requested epoch (or the
/// task's `Close` for `Closed`), and only then delegates. Implemented in milestone 5.
pub struct StoreResource<'a> {
    /// The store, as the role traits the adapter needs.
    pub store: &'a (dyn ResourceFenceStore + Sync),
    /// How the store is named in grants.
    pub id: ResourceId,
}

#[async_trait]
impl FencedResource for StoreResource<'_> {
    fn id(&self) -> ResourceId {
        self.id.clone()
    }

    async fn install(
        &self,
        _singer: &SingerId,
        _task: &TaskId,
        _fence: Fence,
        _grant: VerseId,
    ) -> Result<FenceAck, FenceError> {
        todo!("milestone 5: resolve the authorizing verse, validate it, then install_fence")
    }
}

/// The allocator's own durable state (technical spec §8; technical decisions 5): epoch
/// reservations, pending grants, task lifecycle and the closure record with its snapshot hash.
/// A different contract from the store's `ResourceFenceStore`, even where they share a backend:
/// this is what lets a crashed allocator resume from the recorded step and never reuse an epoch.
#[async_trait]
pub trait AllocatorJournal: Send + Sync {
    /// Reserve the next epoch for `(singer, task)`, persisted before anything else is done with
    /// it; a pending reservation for the same key is returned instead of a new one.
    async fn reserve(&self, singer: &SingerId, task: &TaskId) -> Result<Epoch, FenceError>;
    /// Record the `Grant` verse that fulfilled a reservation.
    async fn granted(
        &self,
        singer: &SingerId,
        task: &TaskId,
        grant: VerseId,
    ) -> Result<(), FenceError>;
    /// Mark a reservation abandoned after the settle timeout; its epoch is never reused.
    async fn abandoned(
        &self,
        singer: &SingerId,
        task: &TaskId,
        epoch: Epoch,
    ) -> Result<(), FenceError>;
    /// The task's lifecycle state as the allocator knows it.
    async fn task_state(&self, task: &TaskId) -> Result<TaskState, FenceError>;
    /// Move the task to `Closing` with its frozen grant set, or to `Closed`.
    async fn set_task_state(
        &self,
        task: &TaskId,
        state: TaskState,
        grants: &[VerseId],
    ) -> Result<(), FenceError>;
    /// Persist the closure snapshot's hash before `Close` is sung; a second call for the same
    /// task returns the hash already recorded and ignores the new one.
    async fn closure_snapshot(&self, task: &TaskId, hash: Hash) -> Result<Hash, FenceError>;
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
