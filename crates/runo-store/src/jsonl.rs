//! The jsonl backend (technical spec §7): `song.jsonl`, `fences.jsonl`, `refrains/`, `blobs/`,
//! `index/`. Single-process; a lock file refuses a second opener. Implemented in milestone 2.
