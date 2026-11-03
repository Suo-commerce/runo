//! The one pipeline every verse passes (technical spec §4). Pure: takes the verse, the roster
//! history and a view of the store, and returns a decision. The node applies it.

use serde::{Deserialize, Serialize};

use crate::sign;
use crate::types::{
    ChainKey, Epoch, PROTOCOL_VERSION, Payload, RosterDoc, RosterVersion, SingerId, TaskId, Verse,
    VerseId,
};

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
    fn installed_fence(&self, singer: &SingerId, task: &TaskId) -> Option<Epoch>;
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
/// well-formed. Where the table leaves a detail open, this is the reading taken:
///
/// - Step 2 dedups on the carried `sum` only when the stored verse is identical; a verse that
///   merely reuses a stored `sum` falls through and fails step 4.
/// - Step 7 applies to effects only: a verse that steps 12–13 would archive is admitted under
///   the roster version it names, so delayed history still verifies.
/// - Step 8 also waits for the `Grant` an `Activation` names, so arrival order never rejects an
///   activation; a missing activation is reported under the grant id it would name.
/// - Step 11: a verse at epoch above 0 must carry a grant; epoch 0 is the unfenced original
///   claim and the allocator's and peers' own chains.
#[must_use]
pub fn admit(verse: &Verse, rosters: &dyn RosterHistory, view: &dyn IngressView) -> Decision {
    let h = &verse.header;
    let id = verse.seal.sum;

    // 1. Version. Unknown fields and reserved kinds never decode, so they cannot reach here.
    if h.v == 0 || h.v > PROTOCOL_VERSION {
        return Decision::Reject(RejectReason::Unknown);
    }
    // 2. Byte-identical verse already stored.
    if let Some(class) = view.stored_class(&id)
        && view.get(&id).as_ref() == Some(verse)
    {
        return Decision::Stored { class };
    }
    // 3. Roster version, singer, key.
    let Some(roster) = rosters.at(h.roster) else {
        return Decision::Reject(RejectReason::UnknownSinger);
    };
    let Some(voice) = roster.voices.iter().find(|v| v.singer == h.singer) else {
        return Decision::Reject(RejectReason::UnknownSinger);
    };
    // 4–5. Sum, then signature.
    match sign::sum(h, &verse.payload) {
        Ok(sum) if sum == id => {}
        _ => return Decision::Reject(RejectReason::BadSum),
    }
    if sign::verify(verse, &voice.key).is_err() {
        return Decision::Reject(RejectReason::BadSig);
    }
    // 6. Kind against role.
    let owner_only = matches!(verse.payload, Payload::Roster { .. });
    let allocator_only = matches!(
        verse.payload,
        Payload::Grant { .. }
            | Payload::Activation { .. }
            | Payload::Close { .. }
            | Payload::Tome { .. }
            | Payload::Addendum { .. }
    );
    if (owner_only && h.singer != roster.owner) || (allocator_only && h.singer != roster.allocator)
    {
        return Decision::Reject(RejectReason::Privilege);
    }
    let control = matches!(
        verse.payload,
        Payload::Close { .. } | Payload::Tome { .. } | Payload::Addendum { .. }
    ) && h.singer == roster.allocator;
    let closed = view.task_state(&h.task) == TaskState::Closed;
    let below_fence = view
        .installed_fence(&h.singer, &h.task)
        .is_some_and(|fence| h.epoch < fence);
    let archival = !(closed && control) && (closed || below_fence);
    // 7. Roster in force, for effects.
    if !archival && h.roster != rosters.current() {
        return Decision::Reject(RejectReason::StaleRoster);
    }
    // 8. Everything referenced is known.
    let waiting_for = missing(verse, view);
    if !waiting_for.is_empty() {
        return Decision::Pending { waiting_for };
    }
    // 9. Chain continuity.
    if !chained(verse, view) {
        return Decision::Reject(RejectReason::BadChain);
    }
    // 10. An activation names an existing allocator grant and covers every listed resource.
    if !activation_ok(verse, roster, view) {
        return Decision::Reject(RejectReason::BadActivation);
    }
    // 11. A fenced chain carries an activated allocator grant for exactly this chain.
    if !grant_ok(verse, roster, view) {
        return Decision::Reject(RejectReason::BadGrant);
    }
    // 12–13. Fence decisions.
    if closed && control {
        return Decision::Control;
    }
    if archival {
        return Decision::History;
    }
    // 14. Fork or plain append.
    let key = ChainKey {
        singer: h.singer.clone(),
        task: h.task.clone(),
        epoch: h.epoch,
    };
    let fork_of = view.variants(&key, h.seq).into_iter().find(|v| *v != id);
    Decision::Append { key, fork_of }
}

/// Step 8: `prev`, `cause`, `grant`, the grant's activation, and an activation's grant.
fn missing(verse: &Verse, view: &dyn IngressView) -> Vec<VerseId> {
    let h = &verse.header;
    let mut out: Vec<VerseId> = [h.prev, h.cause, h.grant]
        .into_iter()
        .flatten()
        .filter(|r| view.get(r).is_none())
        .collect();
    if let Some(g) = h.grant
        && view.get(&g).is_some()
        && !view.has_activation(&g)
    {
        out.push(g);
    }
    if let Payload::Activation { grant, .. } = &verse.payload
        && view.get(grant).is_none()
    {
        out.push(*grant);
    }
    out.sort();
    out.dedup();
    out
}

/// Step 9: `prev` is this chain's previous seq, or this is seq 0 with no `prev`.
fn chained(verse: &Verse, view: &dyn IngressView) -> bool {
    let h = &verse.header;
    match (h.seq, h.prev) {
        (0, None) => true,
        (seq, Some(prev)) if seq > 0 => view.get(&prev).is_some_and(|p| {
            p.header.singer == h.singer
                && p.header.task == h.task
                && p.header.epoch == h.epoch
                && p.header.seq + 1 == seq
        }),
        _ => false,
    }
}

/// Step 10: true for every verse that is not an `Activation`.
fn activation_ok(verse: &Verse, roster: &RosterDoc, view: &dyn IngressView) -> bool {
    let Payload::Activation { grant, acks } = &verse.payload else {
        return true;
    };
    view.get(grant).is_some_and(|g| match &g.payload {
        Payload::Grant {
            resources, epoch, ..
        } => {
            g.header.singer == roster.allocator
                && resources.iter().all(|r| {
                    acks.iter()
                        .any(|a| &a.resource == r && a.epoch == *epoch && a.durable)
                })
        }
        _ => false,
    })
}

/// Step 11: a verse with a grant names an activated allocator grant for exactly its chain; a
/// verse above epoch 0 must have one.
fn grant_ok(verse: &Verse, roster: &RosterDoc, view: &dyn IngressView) -> bool {
    let h = &verse.header;
    match h.grant {
        Some(g) => {
            view.get(&g).is_some_and(|gv| match &gv.payload {
                Payload::Grant {
                    holder,
                    task,
                    epoch,
                    ..
                } => {
                    gv.header.singer == roster.allocator
                        && *holder == h.singer
                        && *task == h.task
                        && *epoch == h.epoch
                }
                _ => false,
            }) && view.has_activation(&g)
        }
        None => h.epoch == Epoch(0),
    }
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
