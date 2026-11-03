//! Ed25519 signing of verses and refrains. The signature is over `sum`, never over raw bytes.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::codec::{CodecError, canonical};
use crate::hash::digest;
use crate::types::{Header, Payload, Refrain, Seal, Verse, VerseId};

/// Signing failure.
#[derive(Debug, thiserror::Error)]
pub enum SignError {
    /// Encoding failed before signing.
    #[error(transparent)]
    Codec(#[from] CodecError),
    /// The signature does not verify.
    #[error("bad signature")]
    BadSignature,
    /// The key bytes are not a valid verifying key.
    #[error("bad key")]
    BadKey,
}

/// Compute `sum` for a header and payload.
///
/// # Errors
/// Returns an error if canonical encoding fails.
pub fn sum(header: &Header, payload: &Payload) -> Result<VerseId, SignError> {
    let bytes = canonical(&(header, payload))?;
    Ok(VerseId(digest(&bytes)))
}

/// Seal a verse: compute `sum`, sign it.
///
/// # Errors
/// Returns an error if canonical encoding fails.
pub fn seal(key: &SigningKey, header: Header, payload: Payload) -> Result<Verse, SignError> {
    let sum = sum(&header, &payload)?;
    let sig = key.sign(&(sum.0).0).to_bytes();
    Ok(Verse {
        header,
        payload,
        seal: Seal { sum, sig },
    })
}

/// Verify a verse's `sum` and `sig` against a verifying key.
///
/// # Errors
/// Returns `BadSignature` if either check fails.
pub fn verify(verse: &Verse, key: &[u8; 32]) -> Result<(), SignError> {
    let expected = sum(&verse.header, &verse.payload)?;
    if expected != verse.seal.sum {
        return Err(SignError::BadSignature);
    }
    let vk = VerifyingKey::from_bytes(key).map_err(|_| SignError::BadKey)?;
    let sig = Signature::from_bytes(&verse.seal.sig);
    vk.verify(&(verse.seal.sum.0).0, &sig)
        .map_err(|_| SignError::BadSignature)
}

/// Verify a refrain's signature over the canonical bytes of its other fields.
///
/// # Errors
/// Returns `BadSignature` if the check fails.
pub fn verify_refrain(refrain: &Refrain, key: &[u8; 32]) -> Result<(), SignError> {
    let mut unsigned = refrain.clone();
    unsigned.sig = [0u8; 64];
    let bytes = canonical(&unsigned)?;
    let vk = VerifyingKey::from_bytes(key).map_err(|_| SignError::BadKey)?;
    let sig = Signature::from_bytes(&refrain.sig);
    vk.verify(&bytes, &sig).map_err(|_| SignError::BadSignature)
}
