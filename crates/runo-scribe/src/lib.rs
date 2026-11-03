//! Closure snapshots, tomes, Merkle roots and addenda (technical spec §9). Not a voice: holds no
//! key, sings nothing, and its output is reproducible by anyone with the store.

pub mod merkle;

use runo_core::ingress::StoreView;
use runo_core::{Hash, Verse, VerseId};

/// A sealed tome.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Tome {
    /// The `Close` verse.
    pub close: VerseId,
    /// Hash of the manifest blob.
    pub manifest: Hash,
    /// Merkle root over the included verses.
    pub root: Hash,
}

/// Sealing or verification failure.
#[derive(Debug, thiserror::Error)]
pub enum ScribeError {
    /// The closure snapshot could not be loaded.
    #[error("snapshot missing")]
    SnapshotMissing,
    /// A chain in the snapshot is incomplete in the store.
    #[error("chain {0} incomplete")]
    ChainIncomplete(String),
    /// Verification found a mismatch; names the first differing chain or fork.
    #[error("mismatch at {0}")]
    Mismatch(String),
}

/// Deterministic: same closure verse + same store contents => byte-identical manifest and root.
/// Reads the closure snapshot, never live heads. Implemented in milestone 5.
///
/// # Errors
/// Returns `ScribeError` if the snapshot or a chain is missing.
pub fn seal(_store: &dyn StoreView, _closure: &Verse) -> Result<Tome, ScribeError> {
    todo!("milestone 5: runo-scribe seal")
}

/// Recompute and compare. Implemented in milestone 5.
///
/// # Errors
/// Returns `ScribeError::Mismatch` naming the first difference.
pub fn verify(_store: &dyn StoreView, _tome: &Tome) -> Result<(), ScribeError> {
    todo!("milestone 5: runo-scribe verify")
}
