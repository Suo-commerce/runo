//! `MemStore`: `BTreeMap`s with an injectable `FailurePlan`. The simulator's and the unit tests'
//! store. `Process` durability only, and says so. Implemented in milestone 2.
//!
//! Records are kept in arrival order as `(Verse, RecordClass)` under a `u64` cursor, with a
//! verse-id index into it, so `read_since` and `get` return [`Record`](crate::Record)s with the
//! class each route persisted.
