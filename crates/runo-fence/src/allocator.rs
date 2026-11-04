//! The allocator (technical spec §8): the one voice that grants epochs and closes tasks. It holds
//! the allocator key, its journal, the song store and the other fenced resources.
//!
//! **Relaunch**, steps as numbered in §8: (1) one request at a time; (2) reserve the epoch in the
//! journal first; (3) sing `Grant`; (4) install the fence at every resource the holder's roster
//! entry lists; (5) any missing ack: stop, the grant stays unactivated, a retry resumes with the
//! same grant; (6) sing `Activation` with every ack exactly as given. Steps 7–8 belong to the
//! replacement and the resources.
//!
//! **Closure** (decided 4 Nov 2026: a `Closing` verse authorizes the closed fences): (1) journal
//! `Closing` with the frozen grant set, then sing `Closing`; (2) abandon reservations that never
//! became grants; (3) install `Closed` at every resource the grants name, and the song; (4) build
//! the snapshot at that boundary and persist its hash in the journal before anything else, reusing
//! it on every retry; (5) sing `Close` naming it, journal `Closed`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use ed25519_dalek::SigningKey;
use runo_core::codec::canonical;
use runo_core::fence::{Durability, Fence, FenceAck};
use runo_core::ingress::{RecordClass, TaskState};
use runo_core::sign::seal;
use runo_core::{
    ChainEnd, ChainKey, ClosureSnapshot, Epoch, Header, Payload, Progress, Ref, ResourceId,
    RosterDoc, SingerId, TaskId, Timestamp, Verse, VerseId,
};
use runo_store::{SongStore, StoreError};
use tokio::sync::Mutex;

use crate::{AllocatorJournal, FenceError, FencedResource, Outcome, StoreResource};

/// The allocator.
pub struct Allocator<J> {
    key: SigningKey,
    roster: RosterDoc,
    store: Arc<dyn SongStore>,
    song: ResourceId,
    journal: J,
    others: BTreeMap<ResourceId, Arc<dyn FencedResource>>,
    wall: Box<dyn Fn() -> Timestamp + Send + Sync>,
    one_at_a_time: Mutex<()>,
}

impl<J: AllocatorJournal> Allocator<J> {
    /// An allocator signing with `key` (the roster's allocator), writing to `store` (named `song`
    /// in grants) and journaling to `journal`. `wall` gives `sung_at`.
    ///
    /// # Errors
    /// `key` is not the roster allocator's key.
    pub fn new(
        key: SigningKey,
        roster: RosterDoc,
        store: Arc<dyn SongStore>,
        journal: J,
        wall: Box<dyn Fn() -> Timestamp + Send + Sync>,
    ) -> Result<Self, FenceError> {
        let mine = roster
            .voices
            .iter()
            .find(|v| v.singer == roster.allocator)
            .ok_or_else(|| FenceError::Setup("the roster's allocator is not a voice".into()))?;
        if mine.key != key.verifying_key().to_bytes() {
            return Err(FenceError::Setup(
                "this key is not the roster allocator's".into(),
            ));
        }
        Ok(Self {
            key,
            roster,
            store,
            song: ResourceId("song".into()),
            journal,
            others: BTreeMap::new(),
            wall,
            one_at_a_time: Mutex::new(()),
        })
    }

    /// Another resource that must install fences before a grant naming it activates.
    #[must_use]
    pub fn with_resource(mut self, r: Arc<dyn FencedResource>) -> Self {
        self.others.insert(r.id(), r);
        self
    }

    /// The journal, for inspection.
    pub fn journal(&self) -> &J {
        &self.journal
    }

    /// Relaunch `singer` on `task` under a new epoch (steps 1–6).
    ///
    /// # Errors
    /// The task is closing or closed, the singer has no fenced resources, or storage fails.
    pub async fn relaunch(&self, singer: &SingerId, task: &TaskId) -> Result<Outcome, FenceError> {
        let _one = self.one_at_a_time.lock().await;
        if self.journal.task_state(task).await? != TaskState::Open {
            return Err(FenceError::Closing);
        }
        let resources = self.resources_of(singer)?;
        let r = self.journal.reserve(singer, task).await?;
        let grant = if let Some(g) = r.grant {
            g
        } else {
            let payload = Payload::Grant {
                holder: singer.clone(),
                task: task.clone(),
                epoch: r.epoch,
                resources: resources.clone(),
            };
            let g = self.sing(task, payload).await?.seal.sum;
            self.journal.granted(singer, task, g).await?;
            g
        };
        let mut acks = Vec::new();
        let mut missing = Vec::new();
        for res in &resources {
            match self
                .install(res, singer, task, Fence::Epoch(r.epoch), grant)
                .await
            {
                Ok(a) if acked(&a, grant, Fence::Epoch(r.epoch)) => acks.push(a),
                _ => missing.push(res.clone()),
            }
        }
        if !missing.is_empty() {
            return Ok(Outcome::NotActivated { grant, missing });
        }
        if self.journal.task_state(task).await? != TaskState::Open {
            return Err(FenceError::Closing);
        }
        let activation = self
            .sing(task, Payload::Activation { grant, acks })
            .await?
            .seal
            .sum;
        self.journal.activated(singer, task, grant).await?;
        Ok(Outcome::Activated { grant, activation })
    }

    /// Close `task` (steps 1–5). Safe to call again after a crash at any step: it resumes from
    /// the journal, and a snapshot already persisted is reused byte for byte.
    ///
    /// # Errors
    /// A resource refused the closed fence, or storage fails.
    pub async fn close(&self, task: &TaskId) -> Result<Verse, FenceError> {
        let _one = self.one_at_a_time.lock().await;
        if let Some(done) = self.find_close(task).await? {
            return Ok(done);
        }
        let state = self.journal.read()?;
        let life = state.lifecycle(task);
        // 1. Closing, journaled before it is sung, then its verse.
        let mut grants = life.grants.clone();
        if life.state != Some(TaskState::Closing) && life.state != Some(TaskState::Closed) {
            grants = state.grants(task).into_iter().map(|(_, _, g)| g).collect();
            self.journal
                .set_task_state(task, TaskState::Closing, &grants, None)
                .await?;
        }
        let closing = if let Some(c) = life.authority {
            c
        } else {
            let c = self
                .sing(task, Payload::Closing { task: task.clone() })
                .await?
                .seal
                .sum;
            self.journal
                .set_task_state(task, TaskState::Closing, &grants, Some(c))
                .await?;
            c
        };
        // 2. Reservations that never became grants are abandoned; their epochs stay used.
        for (singer, epoch) in self.journal.read()?.ungranted(task) {
            self.journal.abandoned(&singer, task, epoch).await?;
        }
        // 3. Closed at every resource any grant names, and always the song.
        let frozen: BTreeSet<VerseId> = grants.iter().copied().collect();
        let mut named: BTreeSet<ResourceId> = BTreeSet::from([self.song.clone()]);
        for (_, _, g) in self.journal.read()?.grants(task) {
            if frozen.contains(&g)
                && let Some(rec) = self.store.get(&g).await?
                && let Payload::Grant { resources, .. } = rec.verse.payload
            {
                named.extend(resources);
            }
        }
        let me = self.roster.allocator.clone();
        for res in &named {
            let ack = self.install(res, &me, task, Fence::Closed, closing).await?;
            if !acked(&ack, closing, Fence::Closed) {
                return Err(FenceError::NotActivated(vec![res.clone()]));
            }
        }
        // 4. The snapshot, once.
        let snapshot = if let Some(h) = self.journal.read()?.lifecycle(task).snapshot {
            h
        } else {
            let bytes = canonical(&self.snapshot(task, &grants).await?)
                .map_err(|e| FenceError::Setup(e.to_string()))?;
            let h = self.store.put_blob(&bytes).await?;
            self.journal.closure_snapshot(task, h).await?
        };
        // 5. Close, then Closed.
        let close = self
            .sing(
                task,
                Payload::Close {
                    task: task.clone(),
                    snapshot: Ref {
                        scheme: "blob".into(),
                        path: String::new(),
                        hash: snapshot,
                    },
                },
            )
            .await?;
        self.journal
            .set_task_state(task, TaskState::Closed, &[], None)
            .await?;
        Ok(close)
    }

    /// Carry a sealed tome into the song (technical spec §9: the scribe cannot sign). `cause` is
    /// the `Close` verse. Idempotent: a tome already sung for this `Close` is returned.
    ///
    /// # Errors
    /// `close` is not this task's `Close`, or storage fails.
    pub async fn tome(
        &self,
        task: &TaskId,
        close: VerseId,
        manifest: runo_core::Hash,
        root: runo_core::Hash,
    ) -> Result<Verse, FenceError> {
        let _one = self.one_at_a_time.lock().await;
        let records = self.store.task_records(task).await?;
        if !records
            .iter()
            .any(|r| r.verse.seal.sum == close && matches!(r.verse.payload, Payload::Close { .. }))
        {
            return Err(FenceError::Setup("that is not this task's Close".into()));
        }
        if let Some(done) = records
            .into_iter()
            .map(|r| r.verse)
            .find(|v| matches!(&v.payload, Payload::Tome { close: c, .. } if *c == close))
        {
            return Ok(done);
        }
        let payload = Payload::Tome {
            close,
            manifest: Ref {
                scheme: "blob".into(),
                path: String::new(),
                hash: manifest,
            },
            root,
        };
        self.sing(task, payload).await
    }

    /// Record that `verse` arrived after `tome` was sealed (technical spec §9, addenda). The late
    /// verse is stored as it arrived; the tome is unchanged. Idempotent.
    ///
    /// # Errors
    /// Storage fails.
    pub async fn addendum(
        &self,
        task: &TaskId,
        tome: VerseId,
        verse: VerseId,
    ) -> Result<Verse, FenceError> {
        let _one = self.one_at_a_time.lock().await;
        if let Some(done) = self.store.task_records(task).await?.into_iter().map(|r| r.verse).find(|v| {
            matches!(&v.payload, Payload::Addendum { tome: t, verse: l } if *t == tome && *l == verse)
        }) {
            return Ok(done);
        }
        self.sing(task, Payload::Addendum { tome, verse }).await
    }

    /// The closure snapshot at this boundary: every grant, every chain's end among the task's
    /// current records (granted chains with no verses included as empty), and every fork.
    async fn snapshot(
        &self,
        task: &TaskId,
        grants: &[VerseId],
    ) -> Result<ClosureSnapshot, FenceError> {
        let records = self.store.task_records(task).await?;
        let mut ends: BTreeMap<(SingerId, Epoch), ChainEnd> = BTreeMap::new();
        let mut variants: BTreeMap<(ChainKey, u64), BTreeSet<VerseId>> = BTreeMap::new();
        for r in records.iter().filter(|r| r.class == RecordClass::Current) {
            let h = &r.verse.header;
            let key = ChainKey {
                singer: h.singer.clone(),
                task: h.task.clone(),
                epoch: h.epoch,
            };
            variants
                .entry((key, h.seq))
                .or_default()
                .insert(r.verse.seal.sum);
            let end = ends.entry((h.singer.clone(), h.epoch)).or_insert(ChainEnd {
                singer: h.singer.clone(),
                epoch: h.epoch,
                last_seq: None,
                last_hash: None,
            });
            if end.last_seq.is_none_or(|s| h.seq > s) {
                end.last_seq = Some(h.seq);
                end.last_hash = Some(r.verse.seal.sum);
            }
        }
        for (holder, epoch, _) in self.journal.read()?.grants(task) {
            ends.entry((holder.clone(), epoch)).or_insert(ChainEnd {
                singer: holder,
                epoch,
                last_seq: None,
                last_hash: None,
            });
        }
        let forks = variants
            .into_iter()
            .filter(|(_, ids)| ids.len() > 1)
            .map(|((key, _), ids)| (key, ids.into_iter().collect()))
            .collect();
        Ok(ClosureSnapshot {
            task: task.clone(),
            grants: grants.to_vec(),
            chains: ends.into_values().collect(),
            forks,
        })
    }

    async fn find_close(&self, task: &TaskId) -> Result<Option<Verse>, FenceError> {
        Ok(self
            .store
            .task_records(task)
            .await?
            .into_iter()
            .map(|r| r.verse)
            .find(|v| {
                v.header.singer == self.roster.allocator
                    && matches!(v.payload, Payload::Close { .. })
            }))
    }

    fn resources_of(&self, singer: &SingerId) -> Result<Vec<ResourceId>, FenceError> {
        let v = self
            .roster
            .voices
            .iter()
            .find(|v| v.singer == *singer)
            .ok_or_else(|| FenceError::Setup(format!("{} is not in the roster", singer.0)))?;
        if v.fenced.is_empty() {
            return Err(FenceError::Setup(format!(
                "{} has no fenced resources: nothing would stop the old incarnation",
                singer.0
            )));
        }
        Ok(v.fenced.clone())
    }

    async fn install(
        &self,
        res: &ResourceId,
        singer: &SingerId,
        task: &TaskId,
        fence: Fence,
        grant: VerseId,
    ) -> Result<FenceAck, FenceError> {
        if *res == self.song {
            let song = StoreResource {
                verses: &*self.store,
                fences: &*self.store,
                id: res.clone(),
            };
            return song.install(singer, task, fence, grant).await;
        }
        match self.others.get(res) {
            Some(r) => r.install(singer, task, fence, grant).await,
            None => Err(FenceError::Setup(format!(
                "no installer for resource {}: the allocator cannot fence it",
                res.0
            ))),
        }
    }

    /// Sign the next verse on the allocator's chain for `task` and store it: appended while the
    /// task is open, recorded as control once it is closed.
    async fn sing(&self, task: &TaskId, payload: Payload) -> Result<Verse, FenceError> {
        let me = &self.roster.allocator;
        let head = self
            .store
            .task_records(task)
            .await?
            .into_iter()
            .filter(|r| r.verse.header.singer == *me && r.verse.header.epoch == Epoch(0))
            .map(|r| (r.verse.header.seq, r.verse.seal.sum))
            .max_by_key(|(seq, _)| *seq);
        let (seq, prev) = head.map_or((0, None), |(s, id)| (s + 1, Some(id)));
        let header = Header {
            v: 3,
            singer: me.clone(),
            roster: self.roster.version,
            task: task.clone(),
            epoch: Epoch(0),
            grant: None,
            seq,
            prev,
            cause: None,
            progress: Progress {
                height: seq,
                head: None,
            },
            sung_at: (self.wall)(),
            note: None,
        };
        let verse =
            seal(&self.key, header, payload).map_err(|e| FenceError::Setup(e.to_string()))?;
        let closed = matches!(
            verse.payload,
            Payload::Close { .. } | Payload::Tome { .. } | Payload::Addendum { .. }
        );
        let stored = if closed {
            self.store.record(&verse, Durability::Host).await
        } else {
            self.store.append(&verse, Durability::Host).await
        };
        match stored {
            Ok(_) => Ok(verse),
            Err(StoreError::Unsupported(_)) => Err(FenceError::Setup(
                "the store cannot keep the allocator's records at Host durability".into(),
            )),
            Err(e) => Err(e.into()),
        }
    }
}

/// An ack counts only if it answers this authority with exactly this fence, at `Host` or better.
fn acked(a: &FenceAck, authority: VerseId, fence: Fence) -> bool {
    a.grant == authority && a.fence == fence && a.durability >= Durability::Host
}
