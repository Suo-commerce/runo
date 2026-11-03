//! `SongStore` and its role traits, the jsonl and in-memory backends, the fence log and the
//! store service (technical spec §7). One process owns a chorus's store; everyone else is a client.

pub mod fence_log;
pub mod jsonl;
pub mod mem;
pub mod service;

use async_trait::async_trait;
use runo_core::ingress::TaskState;
use runo_core::{Epoch, Hash, Refrain, SingerId, TaskId, Verse, VerseId};

/// How durable an append must be before it returns.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Durability {
    /// Survives the singer's process crashing.
    Process,
    /// Survives power loss on the host: fsync has returned.
    Host,
    /// Survives loss of fewer than k replicas.
    Replicated(u8),
}

/// A fence as a resource stores it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fence {
    /// Writes at this epoch or above are admitted.
    Epoch(Epoch),
    /// Above every epoch: nothing writes after it.
    Closed,
}

/// Proof an append happened at the requested level.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Receipt {
    /// The verse appended.
    pub id: VerseId,
    /// The level reached.
    pub level: Durability,
}

/// A resource's acknowledgement of an installed fence.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FenceAck {
    /// The fence now enforced.
    pub fence: Fence,
    /// Durable before this ack.
    pub durable: bool,
}

/// A read cursor over the store.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Cursor(pub u64);

/// Head of one chain.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Head {
    /// Epoch.
    pub epoch: Epoch,
    /// Seq.
    pub seq: u64,
    /// Hash.
    pub hash: VerseId,
}

/// Storage failure.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The backend cannot provide this level; reported at open, not append.
    #[error("unsupported durability {0:?}")]
    Unsupported(Durability),
    /// I/O.
    #[error("io: {0}")]
    Io(String),
    /// The route refused the verse (wrong kind for this path).
    #[error("route: {0}")]
    Route(String),
}

/// A fenced write was refused.
#[derive(Debug, thiserror::Error)]
pub enum FenceError {
    /// The chain has an installed fence; use `append_fenced`.
    #[error("chain is fenced")]
    Fenced,
    /// Epoch below the installed fence.
    #[error("stale epoch")]
    Stale,
    /// The task is closed.
    #[error("task closed")]
    Closed,
    /// Grant unknown or not activated here.
    #[error("bad grant")]
    BadGrant,
    /// Underlying storage failure.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Read-only access to verses.
#[async_trait]
pub trait VerseReader: Send + Sync {
    /// Verses after a cursor, in arrival order, with the next cursor.
    async fn read_since(&self, cursor: Cursor) -> Result<(Vec<Verse>, Cursor), StoreError>;
    /// One verse by id.
    async fn get(&self, id: &VerseId) -> Result<Option<Verse>, StoreError>;
    /// Highest epoch's head for a chain.
    async fn head(&self, singer: &SingerId, task: &TaskId) -> Result<Option<Head>, StoreError>;
    /// A blob by hash.
    async fn get_blob(&self, h: &Hash) -> Result<Option<Vec<u8>>, StoreError>;
}

/// The three write routes (technical spec §7: one API, three routes).
#[async_trait]
pub trait VerseWriter: Send + Sync {
    /// Current work on an unfenced chain. Refuses fenced chains.
    async fn append(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError>;
    /// Current work on a fenced chain; grant and fence checked atomically with the append.
    async fn append_fenced(&self, v: &Verse, level: Durability) -> Result<Receipt, FenceError>;
    /// Historical evidence: validated, kept unchanged, no effects.
    async fn archive(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError>;
    /// Control records (`Close`, `Tome`, `Addendum`) on a closed task; allocator only.
    async fn record(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError>;
    /// Store a blob, returning its hash.
    async fn put_blob(&self, bytes: &[u8]) -> Result<Hash, StoreError>;
}

/// Fences and task lifecycle.
#[async_trait]
pub trait FenceLog: Send + Sync {
    /// Install a fence from a `Grant` verse; durable before the ack; never lowers.
    async fn install_fence(&self, grant: &Verse) -> Result<FenceAck, StoreError>;
    /// The installed fence for a chain owner, if any.
    async fn installed_fence(
        &self,
        singer: &SingerId,
        task: &TaskId,
    ) -> Result<Option<Fence>, StoreError>;
    /// The task's lifecycle state.
    async fn task_state(&self, task: &TaskId) -> Result<TaskState, StoreError>;
}

/// The latest refrain per chain.
#[async_trait]
pub trait RefrainSlot: Send + Sync {
    /// Replace the latest refrain for its chain.
    async fn refrain(&self, r: &Refrain) -> Result<(), StoreError>;
    /// All latest refrains.
    async fn latest_refrains(&self) -> Result<Vec<Refrain>, StoreError>;
}

/// The whole store: the sum of the role traits.
pub trait SongStore: VerseReader + VerseWriter + FenceLog + RefrainSlot {}
impl<T: VerseReader + VerseWriter + FenceLog + RefrainSlot> SongStore for T {}
