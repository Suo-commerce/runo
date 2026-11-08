//! The deterministic simulator, protocol stage (technical spec §12): the real state machines over
//! in-memory replicas and a seeded event order. Every scenario checks S1–S4, `FenceMonotone` and
//! `SnapshotFixed` after every step; a failing seed prints its trace and replays exactly.

pub mod rng;
pub mod scenarios;
pub mod world;
