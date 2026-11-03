//! The one pipeline every verse passes (technical spec §4). Pure: takes the verse, the roster
//! history and a view of the store, and returns a decision. The node applies it.

use serde::{Deserialize, Serialize};

use crate::types::{ChainKey, RosterDoc, RosterVersion, SingerId, TaskId, Verse, VerseId};

/// How the store classified a record on admission (technical spec §7).
///
/// Persisted beside the verse's unchanged bytes; the signed bytes themselves never carry local
/// state. The class follows the route the verse was first admitted on, and storage deduplication
/// never erases it: a byte-identical re-admission returns [`Decision::Stored`] with this class.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordClass {
    /// Current work on the chain (`append` or `append_fenced`): drives live clocks.
    Current,
    /// Historical evidence (`archive`): kept unchanged, no effects, skipped by live clocks.
    History,
    /// Allocator record on a closed task (`record`): `Close`, `Tome`, `Addendum`.
    Control,
}

/// Why a verse was rejected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RejectReason {
    /// Unsupported version, unknown field, or a kind not enabled in this build.
    Unknown,
    /// Roster version or singer not found.
    UnknownSinger,
    /// Recomputed `sum` differs from the carried one.
    BadSum,
    /// Signature does not verify.
    BadSig,
    /// Kind not allowed for the singer's role.
    Privilege,
    /// Roster version is not the one in force for this run.
    StaleRoster,
    /// `prev` is not in the same chain or `seq` is not continuous.
    BadChain,
    /// Activation is unsigned by the allocator, names no grant, or acks are incomplete.
    BadActivation,
    /// Grant is not an activated allocator grant for this chain.
    BadGrant,
}

/// What the node does with a verse.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Decision {
    /// Append to the chain; `fork_of` names an existing variant under the same key.
    Append {
        /// The chain.
        key: ChainKey,
        /// The other variant, if this is a fork.
        fork_of: Option<VerseId>,
    },
    /// Byte-identical verse already in storage, under the class it was first admitted with.
    /// Observable once per node, never appended twice; a `History` class is observed as history,
    /// never as live.
    Stored {
        /// The class the store persisted on first admission.
        class: RecordClass,
    },
    /// Held until the named verses are known.
    Pending {
        /// Missing `prev`, `cause`, `grant` or activation.
        waiting_for: Vec<VerseId>,
    },
    /// Structurally valid but below the installed fence: archive only.
    History,
    /// Allocator record on a closed task: record, no effects.
    Control,
    /// Not appended; recorded locally as an incident.
    Reject(RejectReason),
}

/// Task lifecycle as the store knows it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TaskState {
    /// Grants may be issued.
    Open,
    /// No new reservations or activations; closure in progress.
    Closing,
    /// Closed; only control records land.
    Closed,
}

/// The synchronous validation facts ingress needs from the store, without I/O (technical
/// decisions 2). Reads for sealing and verification are a different contract,
/// `runo_store::ScribeRead`.
pub trait IngressView {
    /// The persisted class of this verse if it is already stored, byte for byte.
    ///
    /// Ingress reads the class rather than deciding one (step 2), so a re-admitted record keeps
    /// whatever it was first admitted as.
    fn stored_class(&self, id: &VerseId) -> Option<RecordClass>;
    /// Fetch a verse by id, if known.
    fn get(&self, id: &VerseId) -> Option<Verse>;
    /// Other variants stored under the same chain key and seq.
    fn variants(&self, key: &ChainKey, seq: u64) -> Vec<VerseId>;
    /// The installed fence for a singer and task, as an epoch, if any.
    fn installed_fence(&self, singer: &SingerId, task: &TaskId) -> Option<crate::types::Epoch>;
    /// The task's lifecycle state.
    fn task_state(&self, task: &TaskId) -> TaskState;
    /// Is there a known activation naming this grant?
    fn has_activation(&self, grant: &VerseId) -> bool;
}

/// Roster versions over time.
pub trait RosterHistory {
    /// The roster at a version, if it exists.
    fn at(&self, version: RosterVersion) -> Option<&RosterDoc>;
    /// The version in force for the current run.
    fn current(&self) -> RosterVersion;
}

/// Decide what to do with a verse (technical spec §4, steps 1–14).
///
/// Structural checks complete before the fence decisions, so a `History` verse is always
/// well-formed. Implemented in milestone 2.
#[must_use]
pub fn admit(_verse: &Verse, _rosters: &dyn RosterHistory, _view: &dyn IngressView) -> Decision {
    todo!("milestone 2: runo-core ingress")
}

#[cfg(test)]
mod tests {
    use super::RecordClass;

    /// The class is persisted beside the verse as a lowercase name (`{"class": "history", …}`),
    /// in jsonl and in the canonical encoding alike.
    #[test]
    fn record_class_is_a_lowercase_name_on_the_wire() {
        for (class, name) in [
            (RecordClass::Current, "current"),
            (RecordClass::History, "history"),
            (RecordClass::Control, "control"),
        ] {
            let mut bytes = Vec::new();
            ciborium::into_writer(&class, &mut bytes).expect("encode");
            let decoded: String = ciborium::from_reader(bytes.as_slice()).expect("a text string");
            assert_eq!(decoded, name);
            let back: RecordClass = ciborium::from_reader(bytes.as_slice()).expect("decode");
            assert_eq!(back, class);
        }
    }
}
