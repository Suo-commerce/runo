//! Grants, activations, the resource guard, task lifecycle, the allocator's journal and
//! operation keys (technical spec §8). The types a fence is spoken in come from `runo-core`.
//!
//! - [`Allocator`] runs the relaunch sequence and closure.
//! - [`journal`] holds its durable state: [`journal::MemJournal`], [`journal::JsonlJournal`].
//! - [`StoreResource`] makes the song store a [`FencedResource`].
//! - [`FenceGuard`] is the check any other resource runs before it accepts a write.

pub mod allocator;
pub mod journal;

pub use allocator::Allocator;

use async_trait::async_trait;
use runo_core::fence::{Fence, FenceAck};
use runo_core::ingress::TaskState;
use runo_core::{Epoch, Hash, Payload, ResourceId, SingerId, TaskId, VerseId};
use runo_store::{ResourceFenceStore, VerseReader};

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
    /// The journal could not be read or written.
    #[error("allocator journal: {0}")]
    Journal(String),
    /// The authorizing verse is not what the install asked for.
    #[error("not an authority for this fence: {0}")]
    Authority(String),
    /// A guarded write was refused.
    #[error("refused: {0}")]
    Refused(String),
    /// The allocator is misconfigured or could not sign.
    #[error("{0}")]
    Setup(String),
}

/// A reserved epoch, with the `Grant` already sung for it if a previous attempt got that far.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Reservation {
    /// The epoch.
    pub epoch: Epoch,
    /// The grant, on a retry.
    pub grant: Option<VerseId>,
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
/// [`ResourceFenceStore::install_fence`], which takes the authorizing verse. It fetches that verse
/// by id and checks it is a `Grant` for `(singer, task)` at the requested epoch, or the task's
/// `Closing` for `Closed`, before it delegates.
pub struct StoreResource<'a> {
    /// Where the authorizing verse is read.
    pub verses: &'a dyn VerseReader,
    /// Where the fence is installed.
    pub fences: &'a dyn ResourceFenceStore,
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
        singer: &SingerId,
        task: &TaskId,
        fence: Fence,
        grant: VerseId,
    ) -> Result<FenceAck, FenceError> {
        install_on_store(
            self.verses,
            self.fences,
            &self.id,
            singer,
            task,
            fence,
            grant,
        )
        .await
    }
}

/// [`StoreResource`] that owns its handle to the store: for an allocator or a simulator that
/// keeps resources in a list.
pub struct SharedStoreResource {
    /// The store.
    pub store: std::sync::Arc<dyn runo_store::SongStore>,
    /// How the store is named in grants.
    pub id: ResourceId,
}

#[async_trait]
impl FencedResource for SharedStoreResource {
    fn id(&self) -> ResourceId {
        self.id.clone()
    }

    async fn install(
        &self,
        singer: &SingerId,
        task: &TaskId,
        fence: Fence,
        grant: VerseId,
    ) -> Result<FenceAck, FenceError> {
        install_on_store(
            &*self.store,
            &*self.store,
            &self.id,
            singer,
            task,
            fence,
            grant,
        )
        .await
    }
}

async fn install_on_store(
    verses: &dyn VerseReader,
    fences: &dyn ResourceFenceStore,
    id: &ResourceId,
    singer: &SingerId,
    task: &TaskId,
    fence: Fence,
    grant: VerseId,
) -> Result<FenceAck, FenceError> {
    let Some(record) = verses.get(&grant).await? else {
        return Err(FenceError::Authority(
            "the authorizing verse is not in the store".into(),
        ));
    };
    let v = &record.verse;
    let ok = match (&v.payload, fence) {
        (
            Payload::Grant {
                holder,
                task: t,
                epoch,
                resources,
            },
            Fence::Epoch(e),
        ) => holder == singer && t == task && *epoch == e && resources.contains(id),
        (Payload::Closing { task: t }, Fence::Closed) => t == task,
        _ => false,
    };
    if !ok {
        return Err(FenceError::Authority(format!(
            "{} does not authorize {fence:?} for {} on {} at {}",
            runo_core::hash::display(&grant.0),
            singer.0,
            task.0,
            id.0
        )));
    }
    Ok(fences.install_fence(v).await?)
}

/// The allocator's own durable state (technical spec §8; technical decisions 5): epoch
/// reservations, pending grants, task lifecycle and the closure record with its snapshot hash.
/// A different contract from the store's `ResourceFenceStore`, even where they share a backend:
/// this is what lets a crashed allocator resume from the recorded step and never reuse an epoch.
#[async_trait]
pub trait AllocatorJournal: Send + Sync {
    /// Reserve the next epoch for `(singer, task)`, persisted before anything else is done with
    /// it; a pending reservation for the same key (reserved, or granted but not activated) is
    /// returned instead of a new one, with its grant if it has one. Refused while closing.
    async fn reserve(&self, singer: &SingerId, task: &TaskId) -> Result<Reservation, FenceError>;
    /// Record the `Grant` verse that fulfilled a reservation.
    async fn granted(
        &self,
        singer: &SingerId,
        task: &TaskId,
        grant: VerseId,
    ) -> Result<(), FenceError>;
    /// Record that the grant was activated: the reservation is complete, and the next relaunch
    /// of `(singer, task)` reserves a new epoch.
    async fn activated(
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
    /// Move the task to `Closing` (with its frozen grant set, and then the `Closing` verse that
    /// authorizes the closed fences), or to `Closed`.
    async fn set_task_state(
        &self,
        task: &TaskId,
        state: TaskState,
        grants: &[VerseId],
        authority: Option<VerseId>,
    ) -> Result<(), FenceError>;
    /// Persist the closure snapshot's hash before `Close` is sung; a second call for the same
    /// task returns the hash already recorded and ignores the new one.
    async fn closure_snapshot(&self, task: &TaskId, hash: Hash) -> Result<Hash, FenceError>;
    /// Everything recorded so far, folded: what a restarted allocator resumes from.
    ///
    /// # Errors
    /// The journal cannot be read.
    fn read(&self) -> Result<journal::JournalState, FenceError>;
}

/// The check any fenced resource other than the song store runs before it accepts a write on
/// behalf of a task (the store runs the same rule inside `append_fenced`). The resource calls
/// [`FenceGuard::admit`] inside the same critical section as the write.
pub struct FenceGuard<'a> {
    /// The fences installed at this resource.
    pub fences: &'a dyn ResourceFenceStore,
    /// Where grants and activations are read.
    pub verses: &'a dyn VerseReader,
    /// The roster's allocator: only its grants and activations count.
    pub allocator: SingerId,
}

impl FenceGuard<'_> {
    /// Admit a write by `singer` on `task` at `epoch` under `grant`: the installed fence must be
    /// exactly this epoch (not lower, not closed), the grant must be the allocator's grant for
    /// this chain, and `activation` must be the allocator's activation of that grant.
    ///
    /// # Errors
    /// [`FenceError::Refused`] naming the rule, or a store failure.
    pub async fn admit(
        &self,
        singer: &SingerId,
        task: &TaskId,
        epoch: Epoch,
        grant: &VerseId,
        activation: &VerseId,
    ) -> Result<(), FenceError> {
        let refuse = |why: &str| Err(FenceError::Refused(why.to_owned()));
        if self.fences.task_state(task).await? == TaskState::Closed {
            return Err(FenceError::Closing);
        }
        match self.fences.installed_fence(singer, task).await? {
            Some(Fence::Epoch(e)) if e == epoch => {}
            Some(Fence::Closed) => return Err(FenceError::Closing),
            Some(Fence::Epoch(e)) if e > epoch => return refuse("stale epoch"),
            _ => return refuse("no fence installed for this epoch"),
        }
        let Some(g) = self.verses.get(grant).await? else {
            return refuse("unknown grant");
        };
        let grant_ok = g.verse.header.singer == self.allocator
            && matches!(&g.verse.payload, Payload::Grant { holder, task: t, epoch: e, .. }
                if holder == singer && t == task && *e == epoch);
        if !grant_ok {
            return refuse("not the allocator's grant for this chain");
        }
        let Some(a) = self.verses.get(activation).await? else {
            return refuse("unknown activation");
        };
        let activation_ok = a.verse.header.singer == self.allocator
            && matches!(&a.verse.payload, Payload::Activation { grant: ag, .. } if ag == grant);
        if !activation_ok {
            return refuse("not the allocator's activation of this grant");
        }
        Ok(())
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
