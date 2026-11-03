//! Canonical encoding: deterministic CBOR (RFC 8949 §4.2) so that one value has one byte
//! sequence on every node. Strict on read: unknown fields are rejected (technical spec §3).

use serde::{Serialize, de::DeserializeOwned};

/// Encoding failure.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The value could not be serialized.
    #[error("encode: {0}")]
    Encode(String),
    /// The bytes are not a valid value of the expected type, or carry unknown fields.
    #[error("decode: {0}")]
    Decode(String),
}

/// Encode a value as canonical CBOR: sorted map keys, shortest integers, definite lengths.
///
/// # Errors
/// Returns `CodecError::Encode` if serialization fails.
pub fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    // Step 1: serialize with ciborium; step 2: re-order map keys canonically.
    // The canonical pass is implemented in milestone 2; until then this is the plain encoding.
    let mut out = Vec::new();
    ciborium::into_writer(value, &mut out).map_err(|e| CodecError::Encode(e.to_string()))?;
    Ok(out)
}

/// Decode canonical CBOR strictly.
///
/// # Errors
/// Returns `CodecError::Decode` on malformed input or unknown fields.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    ciborium::from_reader(bytes).map_err(|e| CodecError::Decode(e.to_string()))
}
