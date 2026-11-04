//! Canonical encoding: deterministic CBOR (RFC 8949 §4.2) so that one value has one byte
//! sequence on every node. Strict on read: unknown fields, trailing bytes and non-canonical
//! encodings are rejected (technical spec §3).

use ciborium::Value;
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
    /// The bytes decode, but are not the canonical encoding of what they decode to.
    #[error("not canonical CBOR")]
    NotCanonical,
}

/// Encode a value as canonical CBOR: map keys sorted by the bytewise order of their own
/// encodings, shortest integers, definite lengths (RFC 8949 §4.2.1).
///
/// `ciborium` already writes shortest-form integers and definite lengths; this adds the key
/// order, recursively, through an intermediate [`Value`].
///
/// # Errors
/// Returns `CodecError::Encode` if serialization fails.
pub fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>, CodecError> {
    let v = Value::serialized(value).map_err(|e| CodecError::Encode(e.to_string()))?;
    let v = canonicalize(v)?;
    write(&v)
}

/// Decode CBOR strictly: the whole input must be one value, with no unknown fields.
///
/// # Errors
/// Returns `CodecError::Decode` on malformed input, trailing bytes or unknown fields.
pub fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CodecError> {
    let mut rest = bytes;
    let v: Value =
        ciborium::from_reader(&mut rest).map_err(|e| CodecError::Decode(e.to_string()))?;
    if !rest.is_empty() {
        return Err(CodecError::Decode(format!("{} trailing bytes", rest.len())));
    }
    v.deserialized()
        .map_err(|e| CodecError::Decode(e.to_string()))
}

/// Decode, and refuse input that is not byte-for-byte the canonical encoding of the result.
/// For bytes received from elsewhere that claim to be canonical.
///
/// # Errors
/// `CodecError::Decode` as [`decode`], or `CodecError::NotCanonical`.
pub fn decode_canonical<T: DeserializeOwned + Serialize>(bytes: &[u8]) -> Result<T, CodecError> {
    let t: T = decode(bytes)?;
    if canonical(&t)? != bytes {
        return Err(CodecError::NotCanonical);
    }
    Ok(t)
}

fn write(v: &Value) -> Result<Vec<u8>, CodecError> {
    let mut out = Vec::new();
    ciborium::into_writer(v, &mut out).map_err(|e| CodecError::Encode(e.to_string()))?;
    Ok(out)
}

fn canonicalize(v: Value) -> Result<Value, CodecError> {
    Ok(match v {
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .map(canonicalize)
                .collect::<Result<_, _>>()?,
        ),
        Value::Map(entries) => {
            let mut keyed = entries
                .into_iter()
                .map(|(k, val)| {
                    let k = canonicalize(k)?;
                    let bytes = write(&k)?;
                    Ok((bytes, k, canonicalize(val)?))
                })
                .collect::<Result<Vec<_>, CodecError>>()?;
            keyed.sort_by(|a, b| a.0.cmp(&b.0));
            if keyed.windows(2).any(|w| w[0].0 == w[1].0) {
                return Err(CodecError::Encode("duplicate map key".into()));
            }
            Value::Map(keyed.into_iter().map(|(_, k, v)| (k, v)).collect())
        }
        Value::Tag(t, inner) => Value::Tag(t, Box::new(canonicalize(*inner)?)),
        other => other,
    })
}

#[cfg(test)]
mod tests {
    use super::{CodecError, canonical, decode, decode_canonical};
    use proptest::prelude::*;
    use serde::{Deserialize, Serialize};
    use std::collections::BTreeMap;

    #[derive(Serialize, Deserialize, PartialEq, Debug, Clone)]
    #[serde(deny_unknown_fields)]
    struct Small {
        zeta: u64,
        a: String,
        mid: Option<Vec<u8>>,
    }

    #[test]
    fn keys_are_ordered_by_their_encoding_not_declaration() {
        let bytes = canonical(&Small {
            zeta: 1,
            a: "x".into(),
            mid: None,
        })
        .unwrap();
        // map(3), then "a" (shortest key) first; "mid" before "zeta" (same length, bytewise).
        assert_eq!(bytes[0], 0xa3);
        assert_eq!(&bytes[1..3], &[0x61, b'a']);
        let mid = bytes
            .windows(4)
            .position(|w| w == [0x63, b'm', b'i', b'd'])
            .unwrap();
        let zeta = bytes
            .windows(5)
            .position(|w| w == [0x64, b'z', b'e', b't', b'a'])
            .unwrap();
        assert!(mid < zeta);
    }

    #[test]
    fn unknown_fields_and_trailing_bytes_are_refused() {
        let mut extra = BTreeMap::new();
        extra.insert("zeta", ciborium::Value::from(1u64));
        extra.insert("a", ciborium::Value::from("x"));
        extra.insert("mid", ciborium::Value::Null);
        extra.insert("sneaky", ciborium::Value::from(true));
        let bytes = canonical(&extra).unwrap();
        assert!(matches!(
            decode::<Small>(&bytes),
            Err(CodecError::Decode(_))
        ));

        let mut ok = canonical(&Small {
            zeta: 1,
            a: "x".into(),
            mid: None,
        })
        .unwrap();
        ok.push(0x00);
        assert!(matches!(decode::<Small>(&ok), Err(CodecError::Decode(_))));
    }

    #[test]
    fn a_non_canonical_encoding_is_refused_by_decode_canonical() {
        // Declaration order (zeta, a, mid) as plain ciborium writes it.
        let mut plain = Vec::new();
        ciborium::into_writer(
            &Small {
                zeta: 1,
                a: "x".into(),
                mid: None,
            },
            &mut plain,
        )
        .unwrap();
        assert!(decode::<Small>(&plain).is_ok());
        assert!(matches!(
            decode_canonical::<Small>(&plain),
            Err(CodecError::NotCanonical)
        ));
    }

    proptest! {
        #[test]
        fn round_trip_and_deterministic(
            zeta in any::<u64>(),
            a in ".{0,40}",
            mid in proptest::option::of(proptest::collection::vec(any::<u8>(), 0..40)),
            map in proptest::collection::btree_map(".{0,8}", any::<i64>(), 0..8),
        ) {
            let s = Small { zeta, a, mid };
            let bytes = canonical(&s).unwrap();
            prop_assert_eq!(&decode_canonical::<Small>(&bytes).unwrap(), &s);
            prop_assert_eq!(canonical(&s).unwrap(), bytes);

            // A map built in any insertion order encodes identically.
            let forward: Vec<(String, i64)> = map.clone().into_iter().collect();
            let mut backward = forward.clone();
            backward.reverse();
            let enc = |pairs: Vec<(String, i64)>| {
                let v = ciborium::Value::Map(
                    pairs.into_iter().map(|(k, v)| (k.into(), v.into())).collect(),
                );
                canonical(&v).unwrap()
            };
            prop_assert_eq!(enc(forward), enc(backward));
        }
    }
}
