//! Ed25519 keys and signatures: verses (over `sum`, never raw bytes), refrains and rosters.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};

use crate::codec::{CodecError, canonical};
use crate::hash::digest;
use crate::types::{Header, Payload, Refrain, RosterDoc, Seal, Verse, VerseId};

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

/// A new Ed25519 signing key from the operating system's random source. The caller keeps it in
/// the voice's key home and never prints it.
#[must_use]
pub fn generate() -> SigningKey {
    SigningKey::generate(&mut rand_core::OsRng)
}

/// Sign a refrain: fills `sig` over the canonical bytes of its other fields, the counterpart of
/// [`verify_refrain`].
///
/// # Errors
/// Returns an error if canonical encoding fails.
pub fn sign_refrain(key: &SigningKey, mut refrain: Refrain) -> Result<Refrain, SignError> {
    refrain.sig = [0u8; 64];
    let bytes = canonical(&refrain)?;
    refrain.sig = key.sign(&bytes).to_bytes();
    Ok(refrain)
}

/// The owner's signature over a roster document's canonical bytes.
///
/// # Errors
/// Returns an error if canonical encoding fails.
pub fn sign_roster(key: &SigningKey, roster: &RosterDoc) -> Result<[u8; 64], SignError> {
    Ok(key.sign(&canonical(roster)?).to_bytes())
}

/// Check a roster document against its signature by the owner it names: the owner's key is the
/// one in the document's own voice list. That makes version 1 self-certifying; a reader pins the
/// owner's key it was given out of band before trusting it.
///
/// # Errors
/// `BadKey` if the roster names no owner voice; `BadSignature` if the check fails.
pub fn verify_roster(roster: &RosterDoc, sig: &[u8; 64]) -> Result<(), SignError> {
    let owner = roster
        .voices
        .iter()
        .find(|v| v.singer == roster.owner)
        .ok_or(SignError::BadKey)?;
    let vk = VerifyingKey::from_bytes(&owner.key).map_err(|_| SignError::BadKey)?;
    vk.verify(&canonical(roster)?, &Signature::from_bytes(sig))
        .map_err(|_| SignError::BadSignature)
}

#[cfg(test)]
mod signing_tests {
    use super::{generate, sign_refrain, sign_roster, verify_refrain, verify_roster};
    use crate::types::{
        Domains, Epoch, Hash, Progress, Refrain, RosterDoc, RosterVersion, SingerId, TaskId,
        Timestamp, VerseId, VoiceEntry,
    };

    #[test]
    fn a_signed_refrain_verifies_and_a_changed_one_does_not() {
        let k = generate();
        let r = Refrain {
            singer: SingerId("laptop".into()),
            task: TaskId("boot-1".into()),
            epoch: Epoch(0),
            n: 7,
            last: VerseId(Hash::default()),
            progress: Progress {
                height: 3,
                head: None,
            },
            sent_at: Timestamp("2026-11-04T12:00:00Z".into()),
            sig: [9; 64],
        };
        let signed = sign_refrain(&k, r).unwrap();
        let key = k.verifying_key().to_bytes();
        assert!(verify_refrain(&signed, &key).is_ok());
        let mut later = signed;
        later.n = 8;
        assert!(verify_refrain(&later, &key).is_err());
    }

    #[test]
    fn a_roster_verifies_only_under_its_owner() {
        let owner = generate();
        let other = generate();
        let voice = |name: &str, k: &ed25519_dalek::SigningKey| VoiceEntry {
            singer: SingerId(name.into()),
            key: k.verifying_key().to_bytes(),
            role: name.into(),
            domains: Domains::default(),
            fenced: vec![],
        };
        let doc = RosterDoc {
            version: RosterVersion(1),
            voices: vec![voice("owner", &owner), voice("laptop", &other)],
            allocator: SingerId("owner".into()),
            owner: SingerId("owner".into()),
            run: "goliath-1".into(),
        };
        let sig = sign_roster(&owner, &doc).unwrap();
        assert!(verify_roster(&doc, &sig).is_ok());
        assert!(verify_roster(&doc, &sign_roster(&other, &doc).unwrap()).is_err());
        let mut changed = doc.clone();
        changed.run = "goliath-2".into();
        assert!(verify_roster(&changed, &sig).is_err());
    }
}
