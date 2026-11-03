//! The herald (technical spec §2): reads Paperclip's signals and sings peer-family verses about
//! lanes — a `blocked` run becomes `Suspect { clock: Waiting }`, a `failed` run
//! `Suspect { clock: Task }`, a recovery action a `Vouch` or `Suspect` with its reason. Never
//! `Boundary` or `Work` on a lane's behalf. Later, the allocator side. Implemented in milestone 3.
