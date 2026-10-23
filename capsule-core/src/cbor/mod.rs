//! Canonical CBOR (RFC 8949 §4.2 deterministic encoding).
//!
//! This is the **shared, load-bearing** encoder for every byte string a signature or
//! content hash commits to: the [sidecar](crate::sidecar), the encrypted
//! [metadata blob](crate::crypto::encryption) plaintext, and the signed
//! [manifests](crate::crypto::provenance). Two correct implementations MUST produce
//! byte-identical output for the same logical document, or signatures fail to verify
//! across peers — so this module is a **blocking cross-language conformance gate**, not
//! advisory. Conformance is pinned by the golden hex vectors in the tests below.
//!
//! SSoT for the ruleset: [Metadata — Canonical CBOR Encoding].
//!
//! [Metadata — Canonical CBOR Encoding]: https://docs/design/metadata/#canonical-cbor-encoding

mod encode;
mod open_map;

use ciborium::value::Value;
pub use open_map::OpenMap;
use serde::Serialize;
use serde::de::DeserializeOwned;
use thiserror::Error;

/// Errors from (de)serializing through the canonical codec.
#[derive(Debug, Error)]
pub enum CanonicalError {
    /// The value could not be modeled as CBOR before canonicalization.
    #[error("CBOR serialize failed: {0}")]
    Serialize(String),
    /// The input bytes were not valid CBOR.
    #[error("CBOR deserialize failed: {0}")]
    Deserialize(String),
}

/// Canonically encode an already-built CBOR [`Value`]. Infallible.
pub fn value_to_canonical_vec(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode::encode_value(value, &mut out);
    out
}

/// Serialize a `T` and return its canonical CBOR bytes.
pub fn to_canonical_vec<T: Serialize>(value: &T) -> Result<Vec<u8>, CanonicalError> {
    Ok(value_to_canonical_vec(&to_value(value)?))
}

/// Decode CBOR bytes into a `T`. Decoding is tolerant of non-canonical input; callers
/// that require canonical bytes re-encode via [`canonicalize`] or [`to_canonical_vec`].
pub fn from_slice<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, CanonicalError> {
    ciborium::de::from_reader(bytes).map_err(|e| CanonicalError::Deserialize(e.to_string()))
}

/// Re-encode arbitrary CBOR bytes into their canonical form. Used on the receive path:
/// decode what arrived, then verify a signature against the canonical re-encoding.
///
/// The result is a fixed point: canonicalizing it again returns the same bytes. Input
/// holding a negative bignum (tag 3) below `-2^127` is refused with
/// [`CanonicalError::Deserialize`], because its canonical form is one the decoder cannot
/// read back; the decoder already refuses that value written as a definite-length bignum,
/// and this refuses it however else it was framed.
pub fn canonicalize(bytes: &[u8]) -> Result<Vec<u8>, CanonicalError> {
    let value: Value =
        ciborium::de::from_reader(bytes).map_err(|e| CanonicalError::Deserialize(e.to_string()))?;
    if has_unreadable_bignum(&value) {
        return Err(CanonicalError::Deserialize(
            "negative bignum below -2^127 is out of range".into(),
        ));
    }
    Ok(value_to_canonical_vec(&value))
}

/// Whether `value` holds a tag-3 bignum whose canonical form the decoder refuses: a
/// 16-byte magnitude with its top bit set, i.e. a value below `-2^127`, which does not fit
/// the `i128` the decoder reads a definite-length bignum into. Larger magnitudes (more than
/// 16 bytes) decode as an opaque tag and are not affected.
fn has_unreadable_bignum(value: &Value) -> bool {
    match value {
        Value::Tag(tag, inner) => {
            if let (encode::TAG_BIGNUM_NEG, Value::Bytes(b)) = (*tag, &**inner) {
                let magnitude = encode::bignum_magnitude(b);
                magnitude.len() == 16 && magnitude[0] & 0x80 != 0
            } else {
                has_unreadable_bignum(inner)
            }
        }
        Value::Array(items) => items.iter().any(has_unreadable_bignum),
        Value::Map(entries) => entries
            .iter()
            .any(|(k, v)| has_unreadable_bignum(k) || has_unreadable_bignum(v)),
        _ => false,
    }
}

/// Model a `T` as a CBOR [`Value`] (the intermediate non-canonical encoding is irrelevant;
/// canonicalization happens on the way out).
fn to_value<T: Serialize>(value: &T) -> Result<Value, CanonicalError> {
    let mut buf = Vec::new();
    ciborium::ser::into_writer(value, &mut buf)
        .map_err(|e| CanonicalError::Serialize(e.to_string()))?;
    ciborium::de::from_reader(buf.as_slice())
        .map_err(|e| CanonicalError::Deserialize(e.to_string()))
}

#[cfg(test)]
mod tests;
