//! A fixture chorus: an owner, an allocator and N lanes with fixed keys, a roster, and a `sing`
//! that keeps each chain's `prev` and `seq` so tests write what they mean and nothing else.

use std::collections::BTreeMap;

use ed25519_dalek::SigningKey;
use runo_core::fence::Durability;
use runo_core::ingress::{Decision, RosterHistory};
use runo_core::sign::seal;
use runo_core::{
    About, Cadence, ChainKey, Domains, Epoch, Header, Millis, PROTOCOL_VERSION, Payload, Progress,
    Promise, Ref, ResourceId, RosterDoc, RosterVersion, SingerId, TaskId, TaskSpec, Timestamp,
    Verse, VerseId, VoiceEntry,
};
use runo_store::{FenceError, SongStore, StoreError};

/// The owner's singer id.
pub const OWNER: &str = "owner";
/// The allocator's singer id.
pub const ALLOCATOR: &str = "allocator";

/// A chorus with fixed keys and a roster history.
pub struct Chorus {
    keys: BTreeMap<SingerId, SigningKey>,
    rosters: BTreeMap<RosterVersion, RosterDoc>,
    current: RosterVersion,
    heads: BTreeMap<ChainKey, (u64, VerseId)>,
}

impl Chorus {
    /// An owner, an allocator and `lanes` lanes named `lane-0`, `lane-1`, …, under roster v1.
    #[must_use]
    pub fn new(lanes: u8) -> Self {
        let mut keys = BTreeMap::new();
        keys.insert(SingerId(OWNER.into()), crate::fixture_key(1));
        keys.insert(SingerId(ALLOCATOR.into()), crate::fixture_key(2));
        for i in 0..lanes {
            keys.insert(lane(i), crate::fixture_key(10 + i));
        }
        let voices = keys
            .iter()
            .map(|(s, k)| VoiceEntry {
                singer: s.clone(),
                key: k.verifying_key().to_bytes(),
                role: if s.0.starts_with("lane") {
                    "lane".into()
                } else {
                    s.0.clone()
                },
                domains: Domains::default(),
                fenced: vec![ResourceId("song".into())],
            })
            .collect();
        let doc = RosterDoc {
            version: RosterVersion(1),
            voices,
            allocator: SingerId(ALLOCATOR.into()),
            owner: SingerId(OWNER.into()),
            run: "fixture".into(),
        };
        let mut rosters = BTreeMap::new();
        rosters.insert(RosterVersion(1), doc);
        Self {
            keys,
            rosters,
            current: RosterVersion(1),
            heads: BTreeMap::new(),
        }
    }

    /// Start a new run under a new roster version with the same voices.
    pub fn next_roster(&mut self) -> RosterVersion {
        let mut doc = self.rosters[&self.current].clone();
        doc.version = RosterVersion(self.current.0 + 1);
        self.current = doc.version;
        self.rosters.insert(doc.version, doc);
        self.current
    }

    /// The signing key of a voice.
    ///
    /// # Panics
    /// If the singer is not in the fixture.
    #[must_use]
    pub fn key(&self, s: &SingerId) -> &SigningKey {
        &self.keys[s]
    }

    /// Sing the next verse of `(singer, task, epoch)`: `seq` and `prev` follow the chain.
    ///
    /// # Panics
    /// If sealing fails, which it cannot for fixture values.
    pub fn sing(
        &mut self,
        singer: &SingerId,
        task: &TaskId,
        epoch: Epoch,
        grant: Option<VerseId>,
        cause: Option<VerseId>,
        payload: Payload,
    ) -> Verse {
        let key = ChainKey {
            singer: singer.clone(),
            task: task.clone(),
            epoch,
        };
        let (seq, prev) = match self.heads.get(&key) {
            Some((seq, id)) => (seq + 1, Some(*id)),
            None => (0, None),
        };
        let header = Header {
            v: PROTOCOL_VERSION,
            singer: singer.clone(),
            roster: self.current,
            task: task.clone(),
            epoch,
            grant,
            seq,
            prev,
            cause,
            progress: Progress {
                height: seq,
                head: None,
            },
            sung_at: Timestamp(format!("2026-11-03T12:00:{:02}Z", seq % 60)),
            note: None,
        };
        let verse = seal(&self.keys[singer], header, payload).expect("fixture seals");
        self.heads.insert(key, (seq, verse.seal.sum));
        verse
    }

    /// A lane claims `task` at `epoch`.
    pub fn claim(
        &mut self,
        s: &SingerId,
        task: &TaskId,
        epoch: Epoch,
        grant: Option<VerseId>,
    ) -> Verse {
        self.sing(s, task, epoch, grant, None, claimed())
    }

    /// A lane's work step.
    pub fn work(
        &mut self,
        s: &SingerId,
        task: &TaskId,
        epoch: Epoch,
        grant: Option<VerseId>,
    ) -> Verse {
        let payload = Payload::Work {
            step: "build".into(),
            changed: vec![],
            head: None,
        };
        self.sing(s, task, epoch, grant, None, payload)
    }

    /// The allocator's `Grant` for `holder` on `task` at `epoch`, fencing the song store.
    /// The allocator's `Grant` naming exactly `resources`.
    pub fn grant_on(
        &mut self,
        holder: &SingerId,
        task: &TaskId,
        epoch: Epoch,
        resources: Vec<ResourceId>,
    ) -> Verse {
        let payload = Payload::Grant {
            holder: holder.clone(),
            task: task.clone(),
            epoch,
            resources,
        };
        self.sing(
            &SingerId(ALLOCATOR.into()),
            task,
            Epoch(0),
            None,
            None,
            payload,
        )
    }

    /// The allocator's `Grant` of `epoch` on the song.
    pub fn grant(&mut self, holder: &SingerId, task: &TaskId, epoch: Epoch) -> Verse {
        let payload = Payload::Grant {
            holder: holder.clone(),
            task: task.clone(),
            epoch,
            resources: vec![ResourceId("song".into())],
        };
        self.sing(
            &SingerId(ALLOCATOR.into()),
            task,
            Epoch(0),
            None,
            None,
            payload,
        )
    }

    /// The allocator's `Activation` of a grant, with one durable ack per listed resource.
    ///
    /// # Panics
    /// If `grant` is not a `Grant` verse.
    pub fn activate(&mut self, grant: &Verse) -> Verse {
        let Payload::Grant {
            task,
            epoch,
            resources,
            ..
        } = &grant.payload
        else {
            panic!("activate takes a Grant");
        };
        let acks = resources
            .iter()
            .map(|r| runo_core::fence::FenceAck {
                resource: r.clone(),
                grant: grant.seal.sum,
                fence: runo_core::fence::Fence::Epoch(*epoch),
                durability: runo_core::fence::Durability::Host,
            })
            .collect();
        let payload = Payload::Activation {
            grant: grant.seal.sum,
            acks,
        };
        let task = task.clone();
        self.sing(
            &SingerId(ALLOCATOR.into()),
            &task,
            Epoch(0),
            None,
            None,
            payload,
        )
    }

    /// The allocator's `Closing` of `task`: the verse that authorizes `Fence::Closed`.
    pub fn closing(&mut self, task: &TaskId) -> Verse {
        let payload = Payload::Closing { task: task.clone() };
        self.sing(
            &SingerId(ALLOCATOR.into()),
            task,
            Epoch(0),
            None,
            None,
            payload,
        )
    }

    /// The allocator's `Close` of `task`, naming a snapshot blob.
    pub fn close(&mut self, task: &TaskId, snapshot: runo_core::Hash) -> Verse {
        let payload = Payload::Close {
            task: task.clone(),
            snapshot: Ref {
                scheme: "blob".into(),
                path: String::new(),
                hash: snapshot,
            },
        };
        self.sing(
            &SingerId(ALLOCATOR.into()),
            task,
            Epoch(0),
            None,
            None,
            payload,
        )
    }

    /// A peer verse about another voice, on the peer's own unfenced chain.
    pub fn ack(&mut self, s: &SingerId, task: &TaskId, about: &Verse) -> Verse {
        let payload = Payload::Ack {
            about: About {
                singer: about.header.singer.clone(),
                task: Some(about.header.task.clone()),
                epoch: Some(about.header.epoch),
                hash: Some(about.seal.sum),
            },
        };
        self.sing(s, task, Epoch(0), None, Some(about.seal.sum), payload)
    }
}

impl RosterHistory for Chorus {
    fn at(&self, version: RosterVersion) -> Option<&RosterDoc> {
        self.rosters.get(&version)
    }
    fn current(&self) -> RosterVersion {
        self.current
    }
}

/// The singer id of lane `i`.
#[must_use]
pub fn lane(i: u8) -> SingerId {
    SingerId(format!("lane-{i}"))
}

fn claimed() -> Payload {
    Payload::Claimed {
        task: TaskSpec {
            deliver: "a fixture task".into(),
            scope: vec![],
            kind: "product".into(),
        },
        promise: Promise {
            unit: "fixture".into(),
            cadence: Cadence {
                call: Millis(1_000),
                again: Millis(1_500),
                suspect: Millis(2_000),
            },
            checkpoints: true,
            source: "fixed-v1".into(),
        },
    }
}

/// A route failure, from either error family.
#[derive(Debug)]
pub enum RouteError {
    /// From `append`, `archive` or `record`.
    Store(StoreError),
    /// From `append_fenced`.
    Fence(FenceError),
}

/// Apply an ingress decision the way the node will (technical spec §7, "one API, three
/// routes"): `Append` to `append` or `append_fenced` by whether the verse carries a grant,
/// `History` to `archive`, `Control` to `record`. Other decisions write nothing.
///
/// # Errors
/// The route's refusal.
pub async fn route(store: &dyn SongStore, v: &Verse, d: &Decision) -> Result<(), RouteError> {
    let level = Durability::Process;
    match d {
        Decision::Append { .. } if v.header.grant.is_some() => store
            .append_fenced(v, level)
            .await
            .map(|_| ())
            .map_err(RouteError::Fence),
        Decision::Append { .. } => store
            .append(v, level)
            .await
            .map(|_| ())
            .map_err(RouteError::Store),
        Decision::History => store
            .archive(v, level)
            .await
            .map(|_| ())
            .map_err(RouteError::Store),
        Decision::Control => store
            .record(v, level)
            .await
            .map(|_| ())
            .map_err(RouteError::Store),
        _ => Ok(()),
    }
}
