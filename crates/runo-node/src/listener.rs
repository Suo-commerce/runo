//! Listening to the store: every record is admitted against a local mirror and the roster before
//! anything believes it (ingress, technical spec §4). A node and the fleet view share this, so a
//! forged verse is refused the same way whether a voice or a person is looking.

use std::collections::BTreeMap;

use runo_core::fence::Durability;
use runo_core::ingress::{Decision, RecordClass, RejectReason, RosterHistory, admit};
use runo_core::sign::verify_refrain;
use runo_core::{Refrain, RosterDoc, RosterVersion, SingerId, TaskId, Verse};
use runo_store::mem::MemStore;
use runo_store::{Cursor, RefrainSlot, StoreError, VerseReader, VerseWriter};

/// A roster history of one version: Core's runs are fixed for their duration.
struct Rosters(RosterDoc);

impl RosterHistory for Rosters {
    fn at(&self, v: RosterVersion) -> Option<&RosterDoc> {
        (v == self.0.version).then_some(&self.0)
    }
    fn current(&self) -> RosterVersion {
        self.0.version
    }
}

/// A record that failed ingress: who it claimed to be from, and why it was refused.
pub type Incident = (SingerId, RejectReason);

/// What one read admitted.
#[derive(Debug, Default)]
pub struct Admitted {
    /// Current records, in store order: the only ones that are live evidence.
    pub current: Vec<Verse>,
    /// Records refused at ingress.
    pub incidents: Vec<Incident>,
}

/// A cursor on the store, a mirror of what it admitted, and the roster it admits against.
pub struct Listener {
    rosters: Rosters,
    mirror: MemStore,
    cursor: Cursor,
    pending: Vec<Verse>,
    verified: BTreeMap<(SingerId, TaskId), u64>,
}

impl Listener {
    /// A listener that admits against `roster`, starting at the beginning of the store.
    #[must_use]
    pub fn new(roster: RosterDoc) -> Self {
        Self {
            rosters: Rosters(roster),
            mirror: MemStore::new(),
            cursor: Cursor(0),
            pending: Vec::new(),
            verified: BTreeMap::new(),
        }
    }

    /// The roster in force.
    #[must_use]
    pub fn roster(&self) -> &RosterDoc {
        &self.rosters.0
    }

    /// What ingress would decide for `verse` now.
    #[must_use]
    pub fn check(&self, verse: &Verse) -> Decision {
        admit(verse, &self.rosters, &self.mirror)
    }

    /// Record a verse this process appended itself (already checked).
    ///
    /// # Errors
    /// The mirror refuses it, which means it was not checked.
    pub async fn mirror_own(&mut self, verse: &Verse) -> Result<(), StoreError> {
        self.mirror
            .append(verse, Durability::Process)
            .await
            .map(|_| ())
    }

    /// Admit everything new in the store, including records that were waiting for an earlier
    /// one (pending).
    ///
    /// # Errors
    /// The store fails.
    pub async fn read<S: VerseReader + ?Sized>(
        &mut self,
        store: &S,
    ) -> Result<Admitted, StoreError> {
        let mut out = Admitted::default();
        loop {
            let (records, next) = store.read_since(self.cursor).await?;
            if records.is_empty() {
                break;
            }
            self.cursor = next;
            let mut batch = std::mem::take(&mut self.pending);
            batch.extend(records.into_iter().map(|r| r.verse));
            for verse in batch {
                self.take(verse, &mut out).await?;
            }
        }
        Ok(out)
    }

    async fn take(&mut self, verse: Verse, out: &mut Admitted) -> Result<(), StoreError> {
        let class = match self.check(&verse) {
            Decision::Stored { .. } => return Ok(()),
            Decision::Pending { .. } => {
                self.pending.push(verse);
                return Ok(());
            }
            Decision::Reject(why) => {
                out.incidents.push((verse.header.singer.clone(), why));
                return Ok(());
            }
            Decision::Append { .. } => RecordClass::Current,
            Decision::History => RecordClass::History,
            Decision::Control => RecordClass::Control,
        };
        match class {
            RecordClass::Current => self.mirror.append(&verse, Durability::Process).await?,
            RecordClass::History => self.mirror.archive(&verse, Durability::Process).await?,
            RecordClass::Control => self.mirror.record(&verse, Durability::Process).await?,
        };
        if class == RecordClass::Current {
            out.current.push(verse);
        }
        Ok(())
    }

    /// The latest refrains that are new since the last call and verify against the roster key
    /// of the singer they name. `skip` drops slots the caller does not care about before the
    /// signature is checked.
    ///
    /// # Errors
    /// The store fails.
    pub async fn refrains<S: RefrainSlot + ?Sized>(
        &mut self,
        store: &S,
        skip: impl Fn(&SingerId) -> bool,
    ) -> Result<(Vec<Refrain>, Vec<Incident>), StoreError> {
        let mut fresh = Vec::new();
        let mut incidents = Vec::new();
        for r in store.latest_refrains().await? {
            let slot = (r.singer.clone(), r.task.clone());
            if skip(&r.singer) || self.verified.get(&slot).is_some_and(|&n| r.n <= n) {
                continue;
            }
            let Some(v) = self.rosters.0.voices.iter().find(|v| v.singer == r.singer) else {
                incidents.push((r.singer.clone(), RejectReason::UnknownSinger));
                continue;
            };
            if verify_refrain(&r, &v.key).is_err() {
                incidents.push((r.singer.clone(), RejectReason::BadSig));
                continue;
            }
            self.verified.insert(slot, r.n);
            fresh.push(r);
        }
        Ok((fresh, incidents))
    }
}
