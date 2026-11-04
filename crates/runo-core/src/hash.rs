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

/// Read a hash as [`display`] prints it, `b3:<64 hex>` (the prefix is optional).
#[must_use]
pub fn parse(s: &str) -> Option<Hash> {
    let hex = s.trim().strip_prefix("b3:").unwrap_or(s.trim());
    if hex.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(hex.get(2 * i..2 * i + 2)?, 16).ok()?;
    }
    Some(Hash(out))
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

#[cfg(test)]
mod tests {
    #[test]
    fn parse_reads_what_display_prints() {
        let h = super::digest(b"x");
        assert_eq!(super::parse(&super::display(&h)), Some(h));
        assert_eq!(super::parse("b3:zz"), None);
    }
}
