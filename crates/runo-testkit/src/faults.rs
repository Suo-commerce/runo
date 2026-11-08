//! A store with failures on demand, for crash and partition tests and the simulator: wrap any
//! [`SongStore`] and switch faults on and off. Every switch is an atomic, so a test flips it from
//! outside while the code under test holds the store.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use async_trait::async_trait;
use runo_core::fence::{Durability, Fence, FenceAck};
use runo_core::ingress::TaskState;
use runo_core::{Hash, Refrain, SingerId, TaskId, Verse, VerseId};
use runo_store::{
    Cursor, FenceError, Head, Receipt, Record, RefrainSlot, ResourceFenceStore, ScribeRead,
    SongStore, StoreError, VerseReader, VerseWriter,
};

/// Which faults are on.
#[derive(Debug, Default)]
pub struct Faults {
    /// Every call fails: the store is unreachable (a partition, or its host is down).
    pub dark: AtomicBool,
    /// Fail the next N `record` calls (a crash just before `Close` is published).
    pub fail_records: AtomicU32,
    /// Fail the next N `install_fence` calls (a resource that does not acknowledge).
    pub fail_installs: AtomicU32,
    /// Fail the next N `append` calls.
    pub fail_appends: AtomicU32,
}

impl Faults {
    fn take(n: &AtomicU32) -> bool {
        n.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |v| v.checked_sub(1))
            .is_ok()
    }

    fn check(&self) -> Result<(), StoreError> {
        if self.dark.load(Ordering::SeqCst) {
            return Err(StoreError::Io("injected: the store is unreachable".into()));
        }
        Ok(())
    }
}

/// A store that fails when told to.
pub struct Faulty<S: ?Sized> {
    /// The faults; flip them from the test.
    pub faults: Arc<Faults>,
    inner: Arc<S>,
}

impl<S: SongStore + ?Sized> Faulty<S> {
    /// Wrap `inner`, with every fault off.
    pub fn new(inner: Arc<S>) -> Self {
        Self {
            faults: Arc::new(Faults::default()),
            inner,
        }
    }

    /// The store underneath, which never fails.
    pub fn inner(&self) -> &Arc<S> {
        &self.inner
    }
}

fn injected(what: &str) -> StoreError {
    StoreError::Io(format!("injected: {what} failed"))
}

#[async_trait]
impl<S: SongStore + ?Sized> VerseReader for Faulty<S> {
    async fn read_since(&self, cursor: Cursor) -> Result<(Vec<Record>, Cursor), StoreError> {
        self.faults.check()?;
        self.inner.read_since(cursor).await
    }
    async fn get(&self, id: &VerseId) -> Result<Option<Record>, StoreError> {
        self.faults.check()?;
        self.inner.get(id).await
    }
    async fn head(&self, singer: &SingerId, task: &TaskId) -> Result<Option<Head>, StoreError> {
        self.faults.check()?;
        self.inner.head(singer, task).await
    }
    async fn get_blob(&self, h: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
        self.faults.check()?;
        self.inner.get_blob(h).await
    }
}

#[async_trait]
impl<S: SongStore + ?Sized> VerseWriter for Faulty<S> {
    async fn append(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError> {
        self.faults.check()?;
        if Faults::take(&self.faults.fail_appends) {
            return Err(injected("append"));
        }
        self.inner.append(v, level).await
    }
    async fn append_fenced(&self, v: &Verse, level: Durability) -> Result<Receipt, FenceError> {
        self.faults.check()?;
        self.inner.append_fenced(v, level).await
    }
    async fn archive(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError> {
        self.faults.check()?;
        self.inner.archive(v, level).await
    }
    async fn record(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError> {
        self.faults.check()?;
        if Faults::take(&self.faults.fail_records) {
            return Err(injected("record"));
        }
        self.inner.record(v, level).await
    }
    async fn put_blob(&self, bytes: &[u8]) -> Result<Hash, StoreError> {
        self.faults.check()?;
        self.inner.put_blob(bytes).await
    }
}

#[async_trait]
impl<S: SongStore + ?Sized> ResourceFenceStore for Faulty<S> {
    async fn install_fence(&self, grant: &Verse) -> Result<FenceAck, StoreError> {
        self.faults.check()?;
        if Faults::take(&self.faults.fail_installs) {
            return Err(injected("install_fence"));
        }
        self.inner.install_fence(grant).await
    }
    async fn installed_fence(
        &self,
        singer: &SingerId,
        task: &TaskId,
    ) -> Result<Option<Fence>, StoreError> {
        self.faults.check()?;
        self.inner.installed_fence(singer, task).await
    }
    async fn task_state(&self, task: &TaskId) -> Result<TaskState, StoreError> {
        self.faults.check()?;
        self.inner.task_state(task).await
    }
}

#[async_trait]
impl<S: SongStore + ?Sized> RefrainSlot for Faulty<S> {
    async fn refrain(&self, r: &Refrain) -> Result<(), StoreError> {
        self.faults.check()?;
        self.inner.refrain(r).await
    }
    async fn latest_refrains(&self) -> Result<Vec<Refrain>, StoreError> {
        self.faults.check()?;
        self.inner.latest_refrains().await
    }
}

#[async_trait]
impl<S: SongStore + ?Sized> ScribeRead for Faulty<S> {
    async fn snapshot(&self, close: &VerseId) -> Result<Option<Vec<u8>>, StoreError> {
        self.faults.check()?;
        self.inner.snapshot(close).await
    }
    async fn blob(&self, h: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
        self.faults.check()?;
        self.inner.blob(h).await
    }
    async fn task_records(&self, task: &TaskId) -> Result<Vec<Record>, StoreError> {
        self.faults.check()?;
        self.inner.task_records(task).await
    }
}
