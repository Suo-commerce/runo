//! Persisted resource fences for the store itself: `(singer, task) -> (fence, grant)`, loaded on
//! open, the highest fence wins. The backend of `ResourceFenceStore`: a jsonl file in Core, a
//! Taso record Later. The allocator's journal (reservations, pending grants, closure recovery) is
//! a separate contract in `runo-fence` even where it shares this file.
