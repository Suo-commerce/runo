//! The jsonl backend (technical spec §7): `song.jsonl`, `fences.jsonl`, `refrains/`, `blobs/`,
//! `index/`. Single-process; a lock file refuses a second opener. Implemented in milestone 2.
//!
//! Each `song.jsonl` line is a wrapper object carrying the route's [`RecordClass`](crate::RecordClass)
//! beside the untouched verse: `{"class": "current" | "history" | "control", "verse": …}`. The
//! line's position is the record's [`Cursor`](crate::Cursor); `index/` is rebuilt from the file
//! on open and maps verse id to `(cursor, class)` so `get` returns a full [`Record`](crate::Record).
