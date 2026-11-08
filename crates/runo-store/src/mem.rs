//! `MemStore`: the shared [`State`] behind a mutex, applied at once. The simulator's and the unit
//! tests' store. `Process` durability only, and says so: asking for more is refused at open.
//!
//! Records are kept in arrival order with their [`RecordClass`](crate::RecordClass), so
//! `read_since` and `get` return [`Record`](crate::Record)s with the class each route persisted.
//! It also implements [`IngressView`] directly, so ingress can be driven against it in tests.

use std::sync::{Mutex, MutexGuard};

use async_trait::async_trait;
use runo_core::fence::{Durability, Fence, FenceAck};
use runo_core::ingress::{RecordClass, TaskState};
use runo_core::{ChainKey, Epoch, Hash, Refrain, ResourceId, SingerId, TaskId, Verse, VerseId};

use crate::state::{Prepared, State};
use crate::{
    Cursor, FenceError, Head, Receipt, Record, RefrainSlot, ResourceFenceStore, ScribeRead,
    StoreError, VerseReader, VerseWriter,
};

/// The in-memory store.
pub struct MemStore {
    state: Mutex<State>,
    resource: ResourceId,
    ceiling: Durability,
}

impl MemStore {
    /// Open an empty store that names itself `resource` in fence acks.
    ///
    /// # Errors
    /// `StoreError::Unsupported` for anything above `Process`.
    pub fn open(resource: ResourceId, level: Durability) -> Result<Self, StoreError> {
        if level > Durability::Process {
            return Err(StoreError::Unsupported(level));
        }
        Ok(Self {
            state: Mutex::new(State::default()),
            resource,
            ceiling: Durability::Process,
        })
    }

    /// A store for the simulator, where this memory stands for a disk that survives the
    /// simulated crash of every process using it, and so may honestly claim `ceiling` (`Host`).
    /// Never use it where a real crash can happen: the memory is lost with the process.
    #[must_use]
    pub fn simulated(resource: ResourceId, ceiling: Durability) -> Self {
        Self {
            state: Mutex::new(State::default()),
            resource,
            ceiling,
        }
    }

    /// An empty store named `song`, at `Process` durability.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Mutex::new(State::default()),
            resource: ResourceId("song".into()),
            ceiling: Durability::Process,
        }
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // A panic while holding the lock leaves the index consistent: every change is applied
        // in one step after it is checked.
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn check(&self, level: Durability) -> Result<(), StoreError> {
        if level > self.ceiling {
            return Err(StoreError::Unsupported(level));
        }
        Ok(())
    }

    fn write(&self, prepared: Prepared<()>, id: VerseId) -> Receipt {
        if let Prepared::Change(change, ()) = prepared {
            self.lock().apply(change);
        }
        Receipt {
            id,
            level: self.ceiling,
        }
    }
}

impl Default for MemStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl VerseReader for MemStore {
    async fn read_since(&self, cursor: Cursor) -> Result<(Vec<Record>, Cursor), StoreError> {
        Ok(self.lock().read_since(cursor))
    }
    async fn get(&self, id: &VerseId) -> Result<Option<Record>, StoreError> {
        Ok(self.lock().get(id))
    }
    async fn head(&self, singer: &SingerId, task: &TaskId) -> Result<Option<Head>, StoreError> {
        Ok(self.lock().head(singer, task))
    }
    async fn get_blob(&self, h: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.lock().blob(h))
    }
}

#[async_trait]
impl VerseWriter for MemStore {
    async fn append(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError> {
        self.check(level)?;
        let mut s = self.lock();
        let p = s.prepare_append(v)?;
        if let Prepared::Change(c, ()) = p {
            s.apply(c);
        }
        Ok(Receipt {
            id: v.seal.sum,
            level: self.ceiling,
        })
    }
    async fn append_fenced(&self, v: &Verse, level: Durability) -> Result<Receipt, FenceError> {
        self.check(level)?;
        // Check and apply under one lock: the atomic step of technical spec §7.
        let mut s = self.lock();
        let p = s.prepare_append_fenced(v)?;
        if let Prepared::Change(c, ()) = p {
            s.apply(c);
        }
        Ok(Receipt {
            id: v.seal.sum,
            level: self.ceiling,
        })
    }
    async fn archive(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError> {
        self.check(level)?;
        let p = self.lock().prepare_archive(v)?;
        Ok(self.write(p, v.seal.sum))
    }
    async fn record(&self, v: &Verse, level: Durability) -> Result<Receipt, StoreError> {
        self.check(level)?;
        let p = self.lock().prepare_record(v)?;
        Ok(self.write(p, v.seal.sum))
    }
    async fn put_blob(&self, bytes: &[u8]) -> Result<Hash, StoreError> {
        let mut s = self.lock();
        let (hash, change) = s.prepare_blob(bytes);
        if let Some(c) = change {
            s.apply(c);
        }
        Ok(hash)
    }
}

#[async_trait]
impl ResourceFenceStore for MemStore {
    async fn install_fence(&self, grant: &Verse) -> Result<FenceAck, StoreError> {
        let mut s = self.lock();
        match s.prepare_install(grant, &self.resource, self.ceiling)? {
            Prepared::Change(c, ack) => {
                s.apply(c);
                Ok(ack)
            }
            Prepared::Stored(_) => unreachable!("install never dedups"),
        }
    }
    async fn installed_fence(
        &self,
        singer: &SingerId,
        task: &TaskId,
    ) -> Result<Option<Fence>, StoreError> {
        Ok(self.lock().installed_fence(singer, task))
    }
    async fn task_state(&self, task: &TaskId) -> Result<TaskState, StoreError> {
        Ok(self.lock().task_state(task))
    }
}

#[async_trait]
impl RefrainSlot for MemStore {
    async fn refrain(&self, r: &Refrain) -> Result<(), StoreError> {
        let mut s = self.lock();
        if let Some(c) = s.prepare_refrain(r) {
            s.apply(c);
        }
        Ok(())
    }
    async fn latest_refrains(&self) -> Result<Vec<Refrain>, StoreError> {
        Ok(self.lock().latest_refrains())
    }
}

#[async_trait]
impl ScribeRead for MemStore {
    async fn snapshot(&self, close: &VerseId) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.lock().snapshot(close))
    }
    async fn blob(&self, h: &Hash) -> Result<Option<Vec<u8>>, StoreError> {
        Ok(self.lock().blob(h))
    }
    async fn task_records(&self, task: &TaskId) -> Result<Vec<Record>, StoreError> {
        Ok(self.lock().task_records(task))
    }
}

crate::state::ingress_view_via_state!(MemStore, lock);
