//! `MemStore` against the store conformance suite (technical spec §7).

use runo_core::fence::Durability;
use runo_store::mem::MemStore;

runo_testkit::store_conformance!(mem, MemStore::new(), Durability::Process);

#[test]
fn mem_refuses_host_durability_at_open() {
    let refused = MemStore::open(runo_core::ResourceId("song".into()), Durability::Host);
    assert!(matches!(
        refused,
        Err(runo_store::StoreError::Unsupported(Durability::Host))
    ));
}
