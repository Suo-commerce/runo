//! The store's rules, once: the in-memory index every backend keeps and the checks every write
//! route makes (technical spec §7). A mutation is first *prepared* — checked against the current
//! state, without changing it — into a [`Change`]; a backend persists the change at its
//! durability and only then *applies* it. `MemStore` applies at once; the jsonl backend writes
//! and fsyncs first. Both therefore enforce exactly the same routes.

use std::collections::{BTreeMap, BTreeSet};

use runo_core::fence::{Durability, Fence, FenceAck};
use runo_core::hash::digest;
use runo_core::ingress::{IngressView, RecordClass, TaskState};
use runo_core::{
    ChainKey, Epoch, Hash, Payload, Refrain, ResourceId, SingerId, TaskId, Verse, VerseId,
};
use serde::{Deserialize, Serialize};

use crate::{Cursor, FenceError, Head, Record, StoreError};

/// A prepared mutation: what a backend persists, then applies.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum Change {
    /// A verse admitted under a class.
    Record {
        /// The class persisted beside it.
        class: RecordClass,
        /// The verse, unchanged.
        verse: Box<Verse>,
    },
    /// An epoch fence raised for a chain owner.
    Fence {
        /// The holder.
        singer: SingerId,
        /// The task.
        task: TaskId,
        /// The epoch now enforced.
        epoch: Epoch,
        /// The `Grant` that authorized it.
        grant: VerseId,
    },
    /// A task closed: every fence on it becomes `Closed`.
    Close {
        /// The task.
        task: TaskId,
        /// The `Close` verse that authorized it.
        close: VerseId,
    },
    /// A blob stored by hash.
    Blob {
        /// Its blake3.
        hash: Hash,
        /// Its bytes.
        bytes: Vec<u8>,
    },
    /// The latest refrain for its chain.
    Refrain {
        /// The refrain.
        refrain: Box<Refrain>,
    },
}

/// What a write route returns once prepared: either nothing to do (already stored, under the
/// class it was first admitted with) or a change to persist.
pub enum Prepared<T> {
    /// Byte-identical verse already stored.
    Stored(VerseId),
    /// Persist, then apply.
    Change(Change, T),
}

/// The in-memory index of one store.
#[derive(Default)]
pub struct State {
    records: Vec<(Verse, RecordClass)>,
    by_id: BTreeMap<VerseId, usize>,
    chains: BTreeMap<ChainKey, BTreeMap<u64, Vec<VerseId>>>,
    fences: BTreeMap<(SingerId, TaskId), (Fence, VerseId)>,
    closed: BTreeMap<TaskId, VerseId>,
    activated: BTreeSet<VerseId>,
    blobs: BTreeMap<Hash, Vec<u8>>,
    refrains: BTreeMap<ChainKey, Refrain>,
}

impl State {
    // ---- writes: prepare --------------------------------------------------------------------

    fn dedup(&self, v: &Verse) -> Result<Option<VerseId>, StoreError> {
        let id = v.seal.sum;
        match self.by_id.get(&id) {
            Some(&i) if self.records[i].0 == *v => Ok(Some(id)),
            Some(_) => Err(StoreError::Route(
                "a different verse carries this sum".into(),
            )),
            None => Ok(None),
        }
    }

    fn fence_of(&self, v: &Verse) -> Option<Fence> {
        self.fences
            .get(&(v.header.singer.clone(), v.header.task.clone()))
            .map(|(f, _)| *f)
    }

    /// `append`: current work on an unfenced chain of an open task.
    pub fn prepare_append(&self, v: &Verse) -> Result<Prepared<()>, StoreError> {
        if let Some(id) = self.dedup(v)? {
            return Ok(Prepared::Stored(id));
        }
        if self.closed.contains_key(&v.header.task) {
            return Err(StoreError::Route(
                "task is closed; use archive or record".into(),
            ));
        }
        if self.fence_of(v).is_some() || v.header.grant.is_some() {
            return Err(StoreError::Route(
                "chain is fenced; use append_fenced".into(),
            ));
        }
        Ok(Prepared::Change(
            Change::Record {
                class: RecordClass::Current,
                verse: Box::new(v.clone()),
            },
            (),
        ))
    }

    /// `append_fenced`: the grant must be the one installed here for exactly this epoch, and
    /// activated; checked and applied inside the same critical section by the backend.
    pub fn prepare_append_fenced(&self, v: &Verse) -> Result<Prepared<()>, FenceError> {
        if let Some(id) = self.dedup(v)? {
            return Ok(Prepared::Stored(id));
        }
        if self.closed.contains_key(&v.header.task) {
            return Err(FenceError::Closed);
        }
        let key = (v.header.singer.clone(), v.header.task.clone());
        let Some((fence, installed)) = self.fences.get(&key) else {
            return Err(FenceError::BadGrant);
        };
        match *fence {
            Fence::Closed => return Err(FenceError::Closed),
            Fence::Epoch(e) if v.header.epoch < e => return Err(FenceError::Stale),
            Fence::Epoch(e) if v.header.epoch > e => return Err(FenceError::BadGrant),
            Fence::Epoch(_) => {}
        }
        if v.header.grant != Some(*installed) || !self.activated.contains(installed) {
            return Err(FenceError::BadGrant);
        }
        Ok(Prepared::Change(
            Change::Record {
                class: RecordClass::Current,
                verse: Box::new(v.clone()),
            },
            (),
        ))
    }

    /// `archive`: only what is below the installed fence or on a closed task.
    pub fn prepare_archive(&self, v: &Verse) -> Result<Prepared<()>, StoreError> {
        if let Some(id) = self.dedup(v)? {
            return Ok(Prepared::Stored(id));
        }
        let archival = self.closed.contains_key(&v.header.task)
            || match self.fence_of(v) {
                Some(Fence::Closed) => true,
                Some(Fence::Epoch(e)) => v.header.epoch < e,
                None => false,
            };
        if !archival {
            return Err(StoreError::Route(
                "not below the fence nor on a closed task".into(),
            ));
        }
        Ok(Prepared::Change(
            Change::Record {
                class: RecordClass::History,
                verse: Box::new(v.clone()),
            },
            (),
        ))
    }

    /// `record`: `Close`, `Tome` or `Addendum` on a closed task.
    pub fn prepare_record(&self, v: &Verse) -> Result<Prepared<()>, StoreError> {
        if let Some(id) = self.dedup(v)? {
            return Ok(Prepared::Stored(id));
        }
        let control = matches!(
            v.payload,
            Payload::Close { .. } | Payload::Tome { .. } | Payload::Addendum { .. }
        );
        if !control {
            return Err(StoreError::Route(
                "record takes Close, Tome or Addendum only".into(),
            ));
        }
        if !self.closed.contains_key(&v.header.task) {
            return Err(StoreError::Route("record needs a closed task".into()));
        }
        Ok(Prepared::Change(
            Change::Record {
                class: RecordClass::Control,
                verse: Box::new(v.clone()),
            },
            (),
        ))
    }

    /// `install_fence`: from a `Grant` (an epoch fence) or a `Close` (`Closed`); never lowers.
    /// Returns the ack the store will give once the change is durable.
    pub fn prepare_install(
        &self,
        authority: &Verse,
        resource: &ResourceId,
        durability: Durability,
    ) -> Result<Prepared<FenceAck>, StoreError> {
        let id = authority.seal.sum;
        let (change, fence) = match &authority.payload {
            Payload::Grant {
                holder,
                task,
                epoch,
                ..
            } => {
                let current = self
                    .fences
                    .get(&(holder.clone(), task.clone()))
                    .map(|(f, _)| *f);
                let enforced =
                    current.map_or(Fence::Epoch(*epoch), |f| f.raised_to(Fence::Epoch(*epoch)));
                let change = Change::Fence {
                    singer: holder.clone(),
                    task: task.clone(),
                    epoch: *epoch,
                    grant: id,
                };
                (change, enforced)
            }
            Payload::Close { task, .. } => (
                Change::Close {
                    task: task.clone(),
                    close: id,
                },
                Fence::Closed,
            ),
            _ => {
                return Err(StoreError::Route(
                    "install_fence takes a Grant or a Close".into(),
                ));
            }
        };
        let ack = FenceAck {
            resource: resource.clone(),
            grant: id,
            fence,
            durability,
        };
        Ok(Prepared::Change(change, ack))
    }

    /// A blob, unless already stored.
    #[must_use]
    pub fn prepare_blob(&self, bytes: &[u8]) -> (Hash, Option<Change>) {
        let hash = digest(bytes);
        let change = (!self.blobs.contains_key(&hash)).then(|| Change::Blob {
            hash,
            bytes: bytes.to_vec(),
        });
        (hash, change)
    }

    /// A refrain, if its `n` is higher than the latest kept for its chain; otherwise nothing.
    #[must_use]
    pub fn prepare_refrain(&self, r: &Refrain) -> Option<Change> {
        let key = ChainKey {
            singer: r.singer.clone(),
            task: r.task.clone(),
            epoch: r.epoch,
        };
        let newer = self.refrains.get(&key).is_none_or(|old| r.n > old.n);
        newer.then(|| Change::Refrain {
            refrain: Box::new(r.clone()),
        })
    }

    // ---- writes: apply ----------------------------------------------------------------------

    /// Apply a change that has reached the backend's durability. Idempotent, so a replay of a
    /// log on open reaches the same state.
    pub fn apply(&mut self, change: Change) {
        match change {
            Change::Record { class, verse } => self.apply_record(class, *verse),
            Change::Fence {
                singer,
                task,
                epoch,
                grant,
            } => {
                let key = (singer, task);
                let raised = match self.fences.get(&key) {
                    None => true,
                    Some((f, _)) => f.raised_to(Fence::Epoch(epoch)) != *f,
                };
                if raised {
                    self.fences.insert(key, (Fence::Epoch(epoch), grant));
                }
            }
            Change::Close { task, close } => {
                for ((_, t), (f, g)) in &mut self.fences {
                    if *t == task {
                        *f = Fence::Closed;
                        *g = close;
                    }
                }
                self.closed.entry(task).or_insert(close);
            }
            Change::Blob { hash, bytes } => {
                self.blobs.entry(hash).or_insert(bytes);
            }
            Change::Refrain { refrain } => {
                let key = ChainKey {
                    singer: refrain.singer.clone(),
                    task: refrain.task.clone(),
                    epoch: refrain.epoch,
                };
                if self.refrains.get(&key).is_none_or(|old| refrain.n > old.n) {
                    self.refrains.insert(key, *refrain);
                }
            }
        }
    }

    fn apply_record(&mut self, class: RecordClass, verse: Verse) {
        let id = verse.seal.sum;
        if self.by_id.contains_key(&id) {
            return;
        }
        let h = &verse.header;
        let key = ChainKey {
            singer: h.singer.clone(),
            task: h.task.clone(),
            epoch: h.epoch,
        };
        self.chains
            .entry(key)
            .or_default()
            .entry(h.seq)
            .or_default()
            .push(id);
        if class != RecordClass::History {
            if let Payload::Activation { grant, .. } = &verse.payload {
                self.activated.insert(*grant);
            }
        }
        self.by_id.insert(id, self.records.len());
        self.records.push((verse, class));
    }

    // ---- reads ------------------------------------------------------------------------------

    fn record_at(&self, i: usize) -> Record {
        let (verse, class) = &self.records[i];
        Record {
            verse: verse.clone(),
            class: *class,
            cursor: Cursor(i as u64 + 1),
        }
    }

    /// Records after a cursor, and the cursor to read from next.
    #[must_use]
    pub fn read_since(&self, cursor: Cursor) -> (Vec<Record>, Cursor) {
        let from = usize::try_from(cursor.0)
            .unwrap_or(usize::MAX)
            .min(self.records.len());
        let out = (from..self.records.len())
            .map(|i| self.record_at(i))
            .collect();
        (out, Cursor(self.records.len() as u64))
    }

    /// One record by verse id.
    #[must_use]
    pub fn get(&self, id: &VerseId) -> Option<Record> {
        self.by_id.get(id).map(|&i| self.record_at(i))
    }

    /// The highest epoch's head for a chain owner.
    #[must_use]
    pub fn head(&self, singer: &SingerId, task: &TaskId) -> Option<Head> {
        self.chains
            .iter()
            .filter(|(k, _)| &k.singer == singer && &k.task == task)
            .max_by_key(|(k, _)| k.epoch)
            .and_then(|(k, seqs)| {
                seqs.iter().next_back().map(|(seq, ids)| Head {
                    epoch: k.epoch,
                    seq: *seq,
                    hash: ids[0],
                })
            })
    }

    /// A blob by hash.
    #[must_use]
    pub fn blob(&self, h: &Hash) -> Option<Vec<u8>> {
        self.blobs.get(h).cloned()
    }

    /// The installed fence for a chain owner.
    #[must_use]
    pub fn installed_fence(&self, singer: &SingerId, task: &TaskId) -> Option<Fence> {
        self.fences
            .get(&(singer.clone(), task.clone()))
            .map(|(f, _)| *f)
    }

    /// Open or Closed; the store does not see the allocator's `Closing`.
    #[must_use]
    pub fn task_state(&self, task: &TaskId) -> TaskState {
        if self.closed.contains_key(task) {
            TaskState::Closed
        } else {
            TaskState::Open
        }
    }

    /// All latest refrains.
    #[must_use]
    pub fn latest_refrains(&self) -> Vec<Refrain> {
        self.refrains.values().cloned().collect()
    }

    /// The snapshot blob a `Close` verse references.
    #[must_use]
    pub fn snapshot(&self, close: &VerseId) -> Option<Vec<u8>> {
        let rec = self.by_id.get(close).map(|&i| &self.records[i].0)?;
        match &rec.payload {
            Payload::Close { snapshot, .. } => self.blob(&snapshot.hash),
            _ => None,
        }
    }

    /// Every record of a task, every class, in arrival order.
    #[must_use]
    pub fn task_records(&self, task: &TaskId) -> Vec<Record> {
        (0..self.records.len())
            .filter(|&i| &self.records[i].0.header.task == task)
            .map(|i| self.record_at(i))
            .collect()
    }
}

impl IngressView for State {
    fn stored_class(&self, id: &VerseId) -> Option<RecordClass> {
        self.by_id.get(id).map(|&i| self.records[i].1)
    }

    fn get(&self, id: &VerseId) -> Option<Verse> {
        self.by_id.get(id).map(|&i| self.records[i].0.clone())
    }

    fn variants(&self, key: &ChainKey, seq: u64) -> Vec<VerseId> {
        self.chains
            .get(key)
            .and_then(|s| s.get(&seq))
            .cloned()
            .unwrap_or_default()
    }

    /// A closed chain reports the highest possible epoch, so everything on it is below the
    /// fence; `task_state` says why.
    fn installed_fence(&self, singer: &SingerId, task: &TaskId) -> Option<Epoch> {
        self.installed_fence(singer, task).map(|f| match f {
            Fence::Epoch(e) => e,
            Fence::Closed => Epoch(u64::MAX),
        })
    }

    fn task_state(&self, task: &TaskId) -> TaskState {
        self.task_state(task)
    }

    fn has_activation(&self, grant: &VerseId) -> bool {
        self.activated.contains(grant)
    }
}
