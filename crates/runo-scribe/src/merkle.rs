//! Binary Merkle tree over `sum` values, blake3, RFC 6962 domain separation (`0x00` leaf,
//! `0x01` node), last node duplicated on odd levels. The one home for Merkle in the workspace.

use runo_core::Hash;

/// Root over leaves in the given order; `None` for no leaves.
#[must_use]
pub fn root(leaves: &[Hash]) -> Option<Hash> {
    if leaves.is_empty() {
        return None;
    }
    let mut level: Vec<Hash> = leaves.iter().map(leaf).collect();
    while level.len() > 1 {
        if level.len() % 2 == 1
            && let Some(&last) = level.last()
        {
            level.push(last);
        }
        level = level.chunks(2).map(|p| node(&p[0], &p[1])).collect();
    }
    level.pop()
}

fn leaf(h: &Hash) -> Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[0x00]);
    hasher.update(&h.0);
    Hash(*hasher.finalize().as_bytes())
}

fn node(a: &Hash, b: &Hash) -> Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(&[0x01]);
    hasher.update(&a.0);
    hasher.update(&b.0);
    Hash(*hasher.finalize().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_has_no_root() {
        assert!(root(&[]).is_none());
    }

    #[test]
    fn single_leaf_is_its_leaf_hash() {
        let h = Hash([7u8; 32]);
        assert_eq!(root(&[h]), Some(leaf(&h)));
    }

    #[test]
    fn order_matters() {
        let a = Hash([1u8; 32]);
        let b = Hash([2u8; 32]);
        assert_ne!(root(&[a, b]), root(&[b, a]));
    }
}
