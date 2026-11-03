//! Wire types, canonical encoding, hashing, signing, ingress decisions and the time trait.
//!
//! This crate depends on nothing else in the workspace. Everything in it is plain data or a
//! pure function; no I/O happens here. See the Runo Core Technical Specification §3–§4.

pub mod codec;
pub mod hash;
pub mod ingress;
pub mod sign;
pub mod time;
pub mod types;

pub use types::*;
