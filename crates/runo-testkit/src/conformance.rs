//! Conformance suites: the same tests against every implementation of a trait. A new
//! implementation is not done until its suite passes (CLAUDE.md, design rules).

/// Run the `SongStore` conformance suite against a store constructor.
///
/// Checks, once milestone 2 lands: durability levels honoured or refused at open; `append`
/// refuses fenced chains; `Stored` on byte-identical re-append; `archive` only below the fence;
/// `record` only for allocator kinds on a closed task; `head` scoped per task.
#[macro_export]
macro_rules! store_conformance {
    ($name:ident, $make:expr) => {
        #[tokio::test]
        async fn $name() {
            let _store = $make;
            // milestone 2: the suite body lives in runo_testkit::conformance::store
        }
    };
}

/// Run the `FencedResource` conformance suite: persisted, monotonic, never lowered, restart-safe.
#[macro_export]
macro_rules! fenced_resource_conformance {
    ($name:ident, $make:expr) => {
        #[tokio::test]
        async fn $name() {
            let _resource = $make;
            // milestone 5
        }
    };
}

/// Run the `CadenceSource` conformance suite: a promise per state, version carried, fallback below ten.
#[macro_export]
macro_rules! cadence_source_conformance {
    ($name:ident, $make:expr) => {
        #[test]
        fn $name() {
            let _source = $make;
            // milestone 4
        }
    };
}
