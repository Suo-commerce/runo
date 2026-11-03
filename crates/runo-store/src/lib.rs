//! `SongStore` and its role traits, the jsonl and in-memory backends, the resource fence store
//! and the store service (technical spec §7). One process owns a chorus's store; everyone else is
//! a client. The types a fence is spoken in (`Fence`, `FenceAck`, `Durability`, `RecordClass`)
//! live in `runo-core` and are re-exported here.

pub mod jsonl;
pub mod mem;
pub mod resource_fences;
pub mod service;
pub mod state;

use async_trait::async_trait;
pub use runo_core::fence::{Durability, Fence, FenceAck};
pub use runo_core::ingress::RecordClass;
use runo_core::ingress::TaskState;
use runo_core::{Epoch, Hash, Refrain, SingerId, TaskId, Verse, VerseId};

/// Proof an append happened at the requested level.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Receipt {
    /// The verse appended.
    pub id: VerseId,
    /// The level reached.
    pub level: Durability,
}

/// A read cursor over the store.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Cursor(pub u64);

/// What a read returns: the verse exactly as sung, plus what this store knows about it
/// (technical spec §7).
///
/// The class is persisted per route beside the unchanged verse bytes (in jsonl, a wrapper object
/// per line: `{"class": "history", "verse": …}`), so a reader's observer skips `History` for live
/// clocks and the scribe finds addenda.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Record {
    /// The verse, byte for byte as admitted.
    pub verse: Verse,
    /// How the store classified it on admission.
    pub class: RecordClass,
    /// Where this record sits in arrival order; the cursor to read from to see what follows it.
    pub cursor: Cursor,
}

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

/// Read-only access to verses. Every read returns a [`Record`], so the persisted class travels
/// with the verse and no reader has to decide one.
#[async_trait]
pub trait VerseReader: Send + Sync {
    /// Records after a cursor, in arrival order, with the next cursor.
    async fn read_since(&self, cursor: Cursor) -> Result<(Vec<Record>, Cursor), StoreError>;
    /// One record by verse id.
    async fn get(&self, id: &VerseId) -> Result<Option<Record>, StoreError>;
    /// Highest epoch's head for a chain.
    async fn head(&self, singer: &SingerId, task: &TaskId) -> Result<Option<Head>, StoreError>;
    /// A blob by hash.
    async fn get_blob(&self, h: &Hash) -> Result<Option<Vec<u8>>, StoreError>;
}

/// The three write routes (technical spec §7: one API, three routes). Each route persists its
/// [`RecordClass`] beside the verse: `append` and `append_fenced` as `Current`, `archive` as
/// `History`, `record` as `Control`. A byte-identical re-admission on any route keeps the class
/// it was first admitted with.
#[async_trait]
pub trait VerseWriter: Send + Sync {
    /// Current work on an unfenced chain. Refuses fenced chains. Persists `Current`.
    async fn append(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError>;
    /// Current work on a fenced chain; grant and fence checked atomically with the append.
    /// Persists `Current`.
    async fn append_fenced(&self, v: &Verse, level: Durability) -> Result<Receipt, FenceError>;
    /// Historical evidence: validated, kept unchanged, no effects. Persists `History`.
    async fn archive(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError>;
    /// Control records (`Close`, `Tome`, `Addendum`) on a closed task; allocator only.
    /// Persists `Control`.
    async fn record(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError>;
    /// Store a blob, returning its hash.
    async fn put_blob(&self, bytes: &[u8]) -> Result<Hash, StoreError>;
}

/// The store's own installed fences and task lifecycle, as the song's first fenced resource
/// (technical spec §7–§8; technical decisions 3 and 5). This is the resource-side contract; the
/// allocator's reservations and closure recovery state are a different one, `AllocatorJournal`
/// in `runo-fence`. The two may share a backend.
#[async_trait]
pub trait ResourceFenceStore: Send + Sync {
    /// Install a fence from a `Grant` verse (an epoch fence) or a `Close` verse (`Closed`);
    /// durable before the ack; never lowers. The ack names this store as the resource.
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

/// Reads for sealing and verification (technical spec §9; technical decisions 2): the closure
/// snapshot a `Close` verse names, blobs, and every record of a task including its history. A
/// different contract from `runo_core::ingress::IngressView`, which answers only the synchronous
/// validation facts ingress needs.
#[async_trait]
pub trait ScribeRead: Send + Sync {
    /// The closure snapshot blob a `Close` verse references, if the store holds it.
    async fn snapshot(&self, close: &VerseId) -> Result<Option<Vec<u8>>, StoreError>;
    /// A blob by hash.
    async fn blob(&self, h: &Hash) -> Result<Option<Vec<u8>>, StoreError>;
    /// Every record of a task, every class, in arrival order.
    async fn task_records(&self, task: &TaskId) -> Result<Vec<Record>, StoreError>;
}

/// The whole store: the sum of the role traits.
pub trait SongStore:
    VerseReader + VerseWriter + ResourceFenceStore + RefrainSlot + ScribeRead
{
}
impl<T: VerseReader + VerseWriter + ResourceFenceStore + RefrainSlot + ScribeRead> SongStore for T {}
