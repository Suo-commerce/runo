//! The herald (technical spec §2): reads Paperclip's signals and sings peer-family verses about
//! lanes — a `blocked` run becomes `Suspect { clock: Waiting }` when the wait is overdue, never
//! on sight; a failed run of a kind that never clears becomes `Suspect { clock: Task }` at once.
//! Never `Boundary` or `Work` on a lane's behalf. Later, the allocator side.
//!
//! - [`board`]: the records read from the board, normalized to events.
//! - [`herald`]: board events to lane evidence and signals, pure.
//! - [`time`]: board timestamps to `Instant`.

pub mod board;
pub mod herald;
pub mod time;
