//! blake3 everywhere: verse identity, Merkle leaves, credential fingerprints.

use crate::types::Hash;

/// Hash arbitrary bytes.
#[must_use]
pub fn digest(bytes: &[u8]) -> Hash {
    Hash(*blake3::hash(bytes).as_bytes())
}

/// Keyed hash, for credential-reference fingerprints under a chorus salt.
#[must_use]
pub fn fingerprint(salt: &[u8; 32], reference: &str) -> Hash {
    Hash(*blake3::keyed_hash(salt, reference.as_bytes()).as_bytes())
}

/// Render a hash the way the specs print it: `b3:<hex>`.
#[must_use]
pub fn display(h: &Hash) -> String {
    let mut s = String::with_capacity(3 + 64);
    s.push_str("b3:");
    for b in h.0 {
        use std::fmt::Write as _;
        let _ = write!(s, "{b:02x}");
    }
    s
}
