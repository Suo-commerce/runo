//! The simulated world (technical spec §12, protocol stage): the real ingress, store, allocator
//! and scribe over in-memory replicas, driven by a seeded event order. After every step the
//! invariants of the model are checked: S1–S4, `FenceMonotone` and `SnapshotFixed`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use async_trait::async_trait;
use runo_core::fence::{Durability, Fence, FenceAck};
use runo_core::ingress::{Decision, RecordClass, admit};
use runo_core::{Epoch, Hash, Payload, ResourceId, SingerId, TaskId, Timestamp, Verse, VerseId};
use runo_fence::journal::MemJournal;
use runo_fence::{Allocator, FenceError, FencedResource, Outcome, SharedStoreResource};
use runo_store::mem::MemStore;
use runo_store::{Cursor, ResourceFenceStore, ScribeRead, SongStore, VerseReader};
use runo_testkit::faults::Faulty;
use runo_testkit::fixture::{Chorus, route};

use crate::rng::Rng;

/// A replica: a store that may go dark.
pub type Replica = Arc<Faulty<MemStore>>;

/// The world one scenario runs in.
pub struct World {
    /// The fixture chorus: keys, roster, chain heads for lanes.
    pub c: Chorus,
    /// Replicas; index 0 is the allocator's own.
    pub replicas: Vec<Replica>,
    /// The allocator.
    pub alloc: Allocator<MemJournal>,
    /// The seeded order.
    pub rng: Rng,
    /// What happened, for a failing seed's report.
    pub trace: Vec<String>,
    /// The latest activated epoch per chain owner, as the allocator answered.
    pub activated: BTreeMap<(SingerId, TaskId), Epoch>,
    fences: BTreeMap<(usize, SingerId, TaskId), Fence>,
    snapshots: BTreeMap<TaskId, Hash>,
    current_at: BTreeMap<(usize, VerseId), Epoch>,
    tomes: Vec<(Verse, runo_scribe::Tome)>,
    /// Verses a replica held as pending (waiting for an earlier one), retried after each delivery.
    pending: Vec<(usize, Verse)>,
}

/// A replica other than the allocator's: before it installs, it receives the authorizing verse
/// (replication delivers the allocator's records to every replica it can reach).
struct ReplicaResource {
    primary: Replica,
    replica: Replica,
    chorus_roster: runo_core::RosterDoc,
    id: ResourceId,
}

#[async_trait]
impl FencedResource for ReplicaResource {
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
        copy_chain_to(&self.primary, &self.replica, &self.chorus_roster, &grant).await?;
        let shared = SharedStoreResource {
            store: self.replica.clone() as Arc<dyn SongStore>,
            id: self.id.clone(),
        };
        shared.install(singer, task, fence, grant).await
    }
}

struct OneRoster(runo_core::RosterDoc);
impl runo_core::ingress::RosterHistory for OneRoster {
    fn at(&self, v: runo_core::RosterVersion) -> Option<&runo_core::RosterDoc> {
        (v == self.0.version).then_some(&self.0)
    }
    fn current(&self) -> runo_core::RosterVersion {
        self.0.version
    }
}

/// Deliver `id` and the allocator verses before it on its chain to `to`, oldest first.
async fn copy_chain_to(
    from: &Replica,
    to: &Replica,
    roster: &runo_core::RosterDoc,
    id: &VerseId,
) -> Result<(), FenceError> {
    let mut chain = Vec::new();
    let mut next = Some(*id);
    while let Some(i) = next {
        if to.get(&i).await?.is_some() {
            break;
        }
        let Some(r) = from.get(&i).await? else { break };
        next = r.verse.header.prev;
        chain.push(r.verse);
    }
    let rosters = OneRoster(roster.clone());
    for v in chain.iter().rev() {
        let d = admit(v, &rosters, &**to.inner());
        route(&**to, v, &d)
            .await
            .map_err(|e| FenceError::Refused(format!("{e:?}")))?;
    }
    Ok(())
}

impl World {
    /// `lanes` lanes, `replicas` replicas (each lane fenced on all of them), seeded by `seed`.
    ///
    /// # Panics
    /// Never for the fixture's values.
    #[must_use]
    pub fn new(seed: u64, lanes: u8, replicas: usize) -> Self {
        let mut c = Chorus::new(lanes);
        let names: Vec<ResourceId> = (0..replicas)
            .map(|i| {
                ResourceId(if i == 0 {
                    "song".into()
                } else {
                    format!("replica-{i}")
                })
            })
            .collect();
        c.fence_lanes_on(&names);
        let stores: Vec<Replica> = names
            .iter()
            .map(|n| {
                Arc::new(Faulty::new(Arc::new(MemStore::simulated(
                    n.clone(),
                    Durability::Host,
                ))))
            })
            .collect();
        let key = c.key(&SingerId("allocator".into())).clone();
        let mut alloc = Allocator::new(
            key,
            c.roster().clone(),
            stores[0].clone() as Arc<dyn SongStore>,
            MemJournal::new(),
            Box::new(|| Timestamp("2026-11-03T12:30:00Z".into())),
        )
        .expect("the fixture's allocator key");
        for (i, n) in names.iter().enumerate().skip(1) {
            alloc = alloc.with_resource(Arc::new(ReplicaResource {
                primary: stores[0].clone(),
                replica: stores[i].clone(),
                chorus_roster: c.roster().clone(),
                id: n.clone(),
            }));
        }
        Self {
            c,
            replicas: stores,
            alloc,
            rng: Rng::new(seed),
            trace: Vec::new(),
            activated: BTreeMap::new(),
            fences: BTreeMap::new(),
            snapshots: BTreeMap::new(),
            current_at: BTreeMap::new(),
            tomes: Vec::new(),
            pending: Vec::new(),
        }
    }

    /// Note a step in the trace.
    pub fn note(&mut self, s: impl Into<String>) {
        self.trace.push(s.into());
    }

    /// Deliver a lane's verse to every reachable replica through ingress, as a node would: a
    /// verse waiting for an earlier one is held and retried after every later delivery. Returns
    /// each replica's first decision (`None` where it was dark).
    pub async fn deliver(&mut self, v: &Verse) -> Vec<Option<Decision>> {
        let mut out = Vec::new();
        for i in 0..self.replicas.len() {
            if self.replicas[i]
                .faults
                .dark
                .load(std::sync::atomic::Ordering::SeqCst)
            {
                out.push(None);
                continue;
            }
            out.push(Some(self.offer(i, v).await));
        }
        self.retry_pending().await;
        self.note(format!(
            "deliver {} {} e{} seq {} -> {:?}",
            v.header.singer.0,
            v.header.task.0,
            v.header.epoch.0,
            v.header.seq,
            out.iter()
                .map(|d| d.as_ref().map(short))
                .collect::<Vec<_>>()
        ));
        out
    }

    async fn offer(&mut self, i: usize, v: &Verse) -> Decision {
        let r = self.replicas[i].clone();
        let d = admit(v, &self.c, &**r.inner());
        if matches!(d, Decision::Pending { .. }) {
            if !self
                .pending
                .iter()
                .any(|(j, p)| *j == i && p.seal.sum == v.seal.sum)
            {
                self.pending.push((i, v.clone()));
            }
        } else if route(&*r, v, &d).await.is_ok() && matches!(d, Decision::Append { .. }) {
            let act = self
                .activated
                .get(&(v.header.singer.clone(), v.header.task.clone()))
                .copied()
                .unwrap_or(Epoch(0));
            self.current_at.insert((i, v.seal.sum), act);
        }
        d
    }

    async fn retry_pending(&mut self) {
        loop {
            let held = std::mem::take(&mut self.pending);
            let before = held.len();
            for (i, v) in held {
                if self.replicas[i]
                    .faults
                    .dark
                    .load(std::sync::atomic::Ordering::SeqCst)
                {
                    self.pending.push((i, v));
                    continue;
                }
                self.offer(i, &v).await;
            }
            if self.pending.len() >= before {
                break;
            }
        }
    }

    /// How replica `i` filed `id`, if it holds it.
    ///
    /// # Panics
    /// If the replica's memory fails, which it cannot.
    pub async fn class(&self, i: usize, id: &VerseId) -> Option<RecordClass> {
        self.replicas[i]
            .inner()
            .get(id)
            .await
            .expect("memory answers")
            .map(|r| r.class)
    }

    /// Relaunch through the allocator; records the activation for S1.
    ///
    /// # Errors
    /// What the allocator refused.
    pub async fn relaunch(
        &mut self,
        singer: &SingerId,
        task: &TaskId,
        stale: Epoch,
    ) -> Result<Outcome, FenceError> {
        let out = self.alloc.relaunch_stale(singer, task, stale).await;
        self.after_relaunch(singer, task, &out);
        out
    }

    /// Record a relaunch's result.
    pub fn after_relaunch(
        &mut self,
        singer: &SingerId,
        task: &TaskId,
        out: &Result<Outcome, FenceError>,
    ) {
        if let Ok(Outcome::Activated { .. }) = out
            && let Ok(state) = runo_fence::AllocatorJournal::read(self.alloc.journal())
            && let Some((e, _)) = state.activated(singer, task)
        {
            self.activated.insert((singer.clone(), task.clone()), e);
        }
        self.note(format!(
            "relaunch {} {} -> {}",
            singer.0,
            task.0,
            outcome(out)
        ));
    }

    /// Close through the allocator, seal, and have the allocator sing the tome.
    ///
    /// # Errors
    /// What the allocator or scribe refused.
    pub async fn close_and_seal(
        &mut self,
        task: &TaskId,
    ) -> Result<(Verse, runo_scribe::Tome), String> {
        let close = self.alloc.close(task).await.map_err(|e| e.to_string())?;
        self.note(format!("close {} -> {}", task.0, short_id(&close.seal.sum)));
        let primary = self.replicas[0].clone();
        let sealed = runo_scribe::seal(&*primary, &close)
            .await
            .map_err(|e| e.to_string())?;
        runo_store::VerseWriter::put_blob(&*primary, &sealed.manifest)
            .await
            .map_err(|e| e.to_string())?;
        self.alloc
            .tome(task, close.seal.sum, sealed.tome.manifest, sealed.tome.root)
            .await
            .map_err(|e| e.to_string())?;
        self.tomes.push((close.clone(), sealed.tome.clone()));
        self.note(format!(
            "seal {} -> {} verses",
            task.0,
            sealed.included.len()
        ));
        Ok((close, sealed.tome))
    }

    /// Replicate the allocator's and every other record between reachable replicas (set union).
    pub async fn sync(&mut self) {
        let lit: Vec<usize> = (0..self.replicas.len())
            .filter(|i| {
                !self.replicas[*i]
                    .faults
                    .dark
                    .load(std::sync::atomic::Ordering::SeqCst)
            })
            .collect();
        for &from in &lit {
            let Ok((records, _)) = self.replicas[from].read_since(Cursor(0)).await else {
                continue;
            };
            for &to in &lit {
                if to == from {
                    continue;
                }
                for r in &records {
                    let dest = self.replicas[to].clone();
                    if dest.get(&r.verse.seal.sum).await.ok().flatten().is_some() {
                        continue;
                    }
                    let d = admit(&r.verse, &self.c, &**dest.inner());
                    if route(&*dest, &r.verse, &d).await.is_ok()
                        && matches!(d, Decision::Append { .. })
                    {
                        let act = self
                            .activated
                            .get(&(r.verse.header.singer.clone(), r.verse.header.task.clone()))
                            .copied()
                            .unwrap_or(Epoch(0));
                        self.current_at.insert((to, r.verse.seal.sum), act);
                    }
                }
            }
        }
        self.note("sync");
    }

    /// Check every invariant; the first violation, named.
    ///
    /// # Errors
    /// The violated invariant and where.
    pub async fn check(&mut self) -> Result<(), String> {
        let mut all: BTreeMap<VerseId, Verse> = BTreeMap::new();
        let mut classes: BTreeMap<VerseId, Vec<RecordClass>> = BTreeMap::new();
        for (i, r) in self.replicas.iter().enumerate() {
            let records = r
                .inner()
                .read_since(Cursor(0))
                .await
                .map_err(|e| e.to_string())?
                .0;
            let here: BTreeSet<VerseId> = records.iter().map(|x| x.verse.seal.sum).collect();
            for rec in &records {
                let v = &rec.verse;
                all.insert(v.seal.sum, v.clone());
                classes.entry(v.seal.sum).or_default().push(rec.class);
                // S4: a cause exists.
                if let Some(c) = v.header.cause
                    && !here.contains(&c)
                {
                    return Err(format!(
                        "S4: replica {i} holds {} whose cause is missing",
                        short_id(&v.seal.sum)
                    ));
                }
                // S1: a current write is never from an epoch below one already activated.
                if rec.class == RecordClass::Current
                    && let Some(act) = self.current_at.get(&(i, v.seal.sum))
                    && v.header.epoch < *act
                {
                    return Err(format!(
                        "S1: replica {i} admitted {} at epoch {} after epoch {} was activated",
                        short_id(&v.seal.sum),
                        v.header.epoch.0,
                        act.0
                    ));
                }
                // FenceMonotone.
                let key = (i, v.header.singer.clone(), v.header.task.clone());
                if let Some(f) = r
                    .inner()
                    .installed_fence(&key.1, &key.2)
                    .await
                    .map_err(|e| e.to_string())?
                {
                    if let Some(old) = self.fences.get(&key)
                        && f.raised_to(*old) != f
                    {
                        return Err(format!(
                            "FenceMonotone: replica {i} lowered {old:?} to {f:?}"
                        ));
                    }
                    self.fences.insert(key, f);
                }
            }
        }
        // S2: every current verse that names a grant names an activated one.
        let activated: BTreeSet<VerseId> = all
            .values()
            .filter_map(|v| match &v.payload {
                Payload::Activation { grant, .. } => Some(*grant),
                _ => None,
            })
            .collect();
        for (id, v) in &all {
            if let Some(g) = v.header.grant
                && classes[id].contains(&RecordClass::Current)
                && !activated.contains(&g)
            {
                return Err(format!(
                    "S2: {} is current under an unactivated grant",
                    short_id(id)
                ));
            }
        }
        self.check_tomes(&classes).await
    }

    /// S3 and `SnapshotFixed`.
    async fn check_tomes(
        &mut self,
        classes: &BTreeMap<VerseId, Vec<RecordClass>>,
    ) -> Result<(), String> {
        // Every tome is what its snapshot says, and a verse that is history at every
        // replica is never in it.
        let primary = self.replicas[0].clone();
        for (close, tome) in self.tomes.clone() {
            let Payload::Close { task, snapshot } = &close.payload else {
                continue;
            };
            if let Some(old) = self.snapshots.get(task)
                && old != &snapshot.hash
            {
                return Err(format!("SnapshotFixed: {} snapshot changed", task.0));
            }
            self.snapshots.insert(task.clone(), snapshot.hash);
            let again = runo_scribe::seal(&*primary, &close)
                .await
                .map_err(|e| e.to_string())?;
            if again.tome != tome {
                return Err(format!(
                    "S3: the tome for {} no longer matches its snapshot",
                    task.0
                ));
            }
            for id in &again.included {
                if classes
                    .get(id)
                    .is_some_and(|c| c.iter().all(|k| *k == RecordClass::History))
                {
                    return Err(format!(
                        "S3: {} is history everywhere but in the tome",
                        short_id(id)
                    ));
                }
            }
        }
        Ok(())
    }

    /// Every record of `task` at replica `i`.
    ///
    /// # Panics
    /// If the replica is dark.
    pub async fn records(&self, i: usize, task: &TaskId) -> Vec<runo_store::Record> {
        self.replicas[i]
            .inner()
            .task_records(task)
            .await
            .expect("an in-memory replica answers")
    }
}

/// A short form of a decision for the trace.
fn short(d: &Decision) -> &'static str {
    match d {
        Decision::Append { .. } => "append",
        Decision::Stored { .. } => "stored",
        Decision::Pending { .. } => "pending",
        Decision::History => "history",
        Decision::Control => "control",
        Decision::Reject(_) => "reject",
    }
}

/// `b3:` and eight hex digits.
#[must_use]
pub fn short_id(id: &VerseId) -> String {
    runo_core::hash::display(&id.0)[..11].to_owned()
}

fn outcome(out: &Result<Outcome, FenceError>) -> String {
    match out {
        Ok(Outcome::Activated { grant, .. }) => format!("activated {}", short_id(grant)),
        Ok(Outcome::NotActivated { missing, .. }) => format!("not activated, missing {missing:?}"),
        Err(e) => format!("refused: {e}"),
    }
}
