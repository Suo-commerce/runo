//! The wire types of the Dwarven Song, version 3 (technical spec §3).
//!
//! Plain data with `serde` derives only. A Later feature adds variants at the end of an enum
//! and fields only under the version rule; it never removes or retypes.

use serde::{Deserialize, Serialize};

/// Protocol version carried in every verse header.
pub const PROTOCOL_VERSION: u8 = 3;

/// A 32-byte blake3 digest.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default, Serialize, Deserialize,
)]
pub struct Hash(pub [u8; 32]);

/// A verse's identity: the blake3 of its canonical bytes (`sum`).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct VerseId(pub Hash);

/// Stable identity of a voice, bound to a key in the roster.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct SingerId(pub String);

/// The issue or task a chain belongs to; a Goliath machine uses its boot session.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct TaskId(pub String);

/// Incarnation within a task; granted by the fence allocator, rises on each relaunch.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct Epoch(pub u64);

/// A roster version; the one a verse names is the one whose key signs it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct RosterVersion(pub u32);

/// A protected resource the allocator can install a fence on.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct ResourceId(pub String);

/// Advisory wall-clock time, RFC 3339 on the wire. Never used for ordering.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Timestamp(pub String);

/// A reference to state kept outside the song: `scheme://path` plus its content hash.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Ref {
    /// `taso`, `file`, `git`, …
    pub scheme: String,
    /// Backend-specific path.
    pub path: String,
    /// Content hash of what the reference points at.
    pub hash: Hash,
}

/// A duration in whole milliseconds on the wire.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct Millis(pub u64);

/// The singer's last durable commit.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    /// Monotonic height of durable work.
    pub height: u64,
    /// Head of that work (for a lane, the git SHA as a hash).
    pub head: Option<Hash>,
}

/// Absolute escalation durations, computed at claim time (spec §5).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Cadence {
    /// Call after this long.
    pub call: Millis,
    /// Call again after this long.
    pub again: Millis,
    /// Sing `suspect` after this long.
    pub suspect: Millis,
}

/// What a singer promises when it claims work.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Promise {
    /// Unit type, e.g. `siege_tank`.
    pub unit: String,
    /// The cadence peers may hold the singer to.
    pub cadence: Cadence,
    /// Whether intermediate checkpoints are expected; `false` disables progress escalation.
    pub checkpoints: bool,
    /// The cadence source and version that produced the promise.
    pub source: String,
}

/// What a task delivers, for the usefulness level.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TaskSpec {
    /// One line on what the task delivers.
    pub deliver: String,
    /// The area or paths it works in.
    pub scope: Vec<String>,
    /// `product` or `process`.
    pub kind: String,
}

/// The full identity of the verse or voice a peer verse concerns.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct About {
    /// The singer concerned.
    pub singer: SingerId,
    /// The task concerned, when one is meant.
    pub task: Option<TaskId>,
    /// The epoch concerned, when one is meant.
    pub epoch: Option<Epoch>,
    /// The verse concerned, when one is meant.
    pub hash: Option<VerseId>,
}

/// The four clocks (spec §5). Integrity is not a clock.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Clock {
    /// Can we hear it?
    Reachability,
    /// Is it moving?
    Progress,
    /// Is it overdue against its promise?
    Task,
    /// Is its dependency stale?
    Waiting,
}

/// How a peer confirmed another is reachable.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum VouchHow {
    /// A direct probe reply bound to a challenge.
    Direct,
    /// Reached through another voice.
    Indirect {
        /// The voice that reached it.
        via: SingerId,
    },
    /// Observed a fresh refrain.
    Refrain,
}

/// A judgment of work.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Verdict {
    /// The Smith admits the cart.
    Merge,
    /// The Smith refuses it.
    Reject,
    /// The Loremaster agrees with a ruling.
    Concur,
    /// The Loremaster disagrees with a ruling.
    Dissent,
    /// The owner accepts a slice.
    Accept,
    /// A ruling is withdrawn by appending.
    Retract,
    /// A waiting lane may proceed.
    Permit,
    /// A waiting lane may not.
    Deny,
}

/// A resource's acknowledgement that a fence is installed and durable.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ResourceAck {
    /// Which resource.
    pub resource: ResourceId,
    /// The epoch it now enforces.
    pub epoch: Epoch,
    /// Durable at `Host` or better before this ack.
    pub durable: bool,
}

/// The header every verse carries.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Header {
    /// Protocol version; readers accept `v <= PROTOCOL_VERSION` and reject unknown fields.
    pub v: u8,
    /// Who sings.
    pub singer: SingerId,
    /// Which roster version signs.
    pub roster: RosterVersion,
    /// Which task's chain.
    pub task: TaskId,
    /// Which incarnation.
    pub epoch: Epoch,
    /// The `Grant` verse for this epoch; `None` only on unfenced chains.
    pub grant: Option<VerseId>,
    /// Strictly increasing within an epoch.
    pub seq: u64,
    /// This singer's previous verse in the chain; `None` only at seq 0.
    pub prev: Option<VerseId>,
    /// The verse this one answers or results from.
    pub cause: Option<VerseId>,
    /// Last durable commit.
    pub progress: Progress,
    /// Advisory time.
    pub sung_at: Timestamp,
    /// One human-readable line.
    pub note: Option<String>,
}

/// The kinds of verse (spec §4). Later kinds are reserved, not present.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Payload {
    // self
    /// Work is claimed, with a promise.
    Claimed {
        /// What the task delivers.
        task: TaskSpec,
        /// The cadence promised.
        promise: Promise,
    },
    /// A state boundary is crossed.
    Boundary {
        /// The event name from the profile's state machine.
        event: String,
        /// Saved state, if any.
        state: Option<Ref>,
        /// What a waiting singer needs; required when `event == "waiting"`.
        blocked_on: Option<String>,
    },
    /// A unit of work starts, advances or ends.
    Work {
        /// The step name.
        step: String,
        /// What this step changed.
        changed: Vec<Ref>,
        /// The new head, if any.
        head: Option<Hash>,
    },
    // peer
    /// A verse that asked to be heard was heard.
    Ack {
        /// The verse heard, by full identity.
        about: About,
    },
    /// A peer is reachable.
    Vouch {
        /// Who.
        about: About,
        /// How it was confirmed.
        how: VouchHow,
    },
    /// A peer's clock ran past its threshold.
    Suspect {
        /// Who.
        about: About,
        /// Which clock.
        clock: Clock,
        /// By how much.
        overdue_by: Millis,
    },
    /// A judgment of work.
    Ruling {
        /// The work's verse.
        about: About,
        /// The verdict.
        verdict: Verdict,
        /// Why, in one line.
        reason: Option<String>,
    },
    // record: allocator only
    /// The allocator reserves an epoch for a singer and task.
    Grant {
        /// Who may work.
        holder: SingerId,
        /// On which task.
        task: TaskId,
        /// In which epoch.
        epoch: Epoch,
        /// Resources that must install the fence before activation.
        resources: Vec<ResourceId>,
    },
    /// Every required resource acknowledged the grant.
    Activation {
        /// The grant activated.
        grant: VerseId,
        /// The acks, covering every listed resource.
        acks: Vec<ResourceAck>,
    },
    /// The allocator closes a task.
    Close {
        /// The task closed.
        task: TaskId,
        /// The immutable closure snapshot blob.
        snapshot: Ref,
    },
    /// A closed task's chains are compacted.
    Tome {
        /// The `Close` verse.
        close: VerseId,
        /// The manifest blob.
        manifest: Ref,
        /// Merkle root over the included verses.
        root: Hash,
    },
    /// A signed verse arrived after its task's closure.
    Addendum {
        /// The tome it belongs with.
        tome: VerseId,
        /// The late verse, unchanged.
        verse: VerseId,
    },
    // record: owner only
    /// The roster changes at a run boundary.
    Roster {
        /// New version.
        version: RosterVersion,
        /// The roster document blob.
        roster: Ref,
    },
    // Later, reserved: Offer, Claim, Handoff — ingress rejects them until enabled.
}

/// Hash and signature over a verse.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Seal {
    /// blake3 of the canonical bytes of header and payload.
    pub sum: VerseId,
    /// Ed25519 signature over `sum`, 64 bytes.
    #[serde(with = "serde_bytes_64")]
    pub sig: [u8; 64],
}

/// One immutable, signed record.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Verse {
    /// The header.
    pub header: Header,
    /// The payload.
    pub payload: Payload,
    /// The seal.
    pub seal: Seal,
}

/// The refrain: repeated while a task is worked; reachability only, never progress.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Refrain {
    /// Who.
    pub singer: SingerId,
    /// Which task.
    pub task: TaskId,
    /// Which epoch.
    pub epoch: Epoch,
    /// Strictly increasing within the epoch; duplicates and regressions are ignored.
    pub n: u64,
    /// The singer's latest verse in that chain.
    pub last: VerseId,
    /// Same as that verse's progress.
    pub progress: Progress,
    /// Advisory time.
    pub sent_at: Timestamp,
    /// Signature over the canonical bytes of the other fields.
    #[serde(with = "serde_bytes_64")]
    pub sig: [u8; 64],
}

/// A voice in the roster.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct VoiceEntry {
    /// Its identity.
    pub singer: SingerId,
    /// Its Ed25519 verifying key.
    pub key: [u8; 32],
    /// Its role in the profile.
    pub role: String,
    /// Declared failure domains.
    pub domains: Domains,
    /// Resources it writes that must be fenced.
    pub fenced: Vec<ResourceId>,
}

/// Declared failure domains (spec §6: declared, not proven).
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct Domains {
    /// Host name.
    pub host: String,
    /// Salted fingerprint of the credential reference.
    pub credential: Hash,
    /// Model provider, or `none`.
    pub provider: String,
    /// Network path.
    pub network: String,
}

/// The roster document, stored as a blob.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RosterDoc {
    /// Version.
    pub version: RosterVersion,
    /// Voices.
    pub voices: Vec<VoiceEntry>,
    /// The fence allocator.
    pub allocator: SingerId,
    /// The owner, who signs rosters.
    pub owner: SingerId,
    /// The run this version authorizes.
    pub run: String,
}

/// Key identifying one chain.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
pub struct ChainKey {
    /// Singer.
    pub singer: SingerId,
    /// Task.
    pub task: TaskId,
    /// Epoch.
    pub epoch: Epoch,
}

/// Where a chain ended at closure.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ChainEnd {
    /// Singer.
    pub singer: SingerId,
    /// Epoch.
    pub epoch: Epoch,
    /// Last seq, or `None` for a granted chain with no verses.
    pub last_seq: Option<u64>,
    /// Last hash, or `None` likewise.
    pub last_hash: Option<VerseId>,
}

/// Written by the allocator at `Close`; immutable (technical spec §3, §9).
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ClosureSnapshot {
    /// The task.
    pub task: TaskId,
    /// Every `Grant` issued for the task.
    pub grants: Vec<VerseId>,
    /// One entry per grant.
    pub chains: Vec<ChainEnd>,
    /// Forks from the fork index.
    pub forks: Vec<(ChainKey, Vec<VerseId>)>,
}

mod serde_bytes_64 {
    use serde::de::{Error as _, Visitor};
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8; 64], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(v)
    }

    struct V;

    impl<'de> Visitor<'de> for V {
        type Value = [u8; 64];

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("64 bytes")
        }

        fn visit_bytes<E: serde::de::Error>(self, b: &[u8]) -> Result<[u8; 64], E> {
            b.try_into()
                .map_err(|_| E::custom("signature must be 64 bytes"))
        }

        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<[u8; 64], A::Error> {
            let mut out = [0u8; 64];
            for (i, slot) in out.iter_mut().enumerate() {
                *slot = seq
                    .next_element()?
                    .ok_or_else(|| A::Error::custom(format!("signature short at {i}")))?;
            }
            Ok(out)
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 64], D::Error> {
        d.deserialize_bytes(V)
    }
}
