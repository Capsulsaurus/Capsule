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
mod tests {
    use ciborium::value::{Integer, Value};

    use super::*;

    fn enc(v: Value) -> Vec<u8> {
        value_to_canonical_vec(&v)
    }
    fn int(n: i128) -> Value {
        Value::Integer(Integer::try_from(n).unwrap())
    }

    // ── Shortest-form integers (RFC 8949 Appendix A) ────────────────────────────
    #[test]
    fn integer_shortest_form() {
        assert_eq!(enc(int(0)), [0x00]);
        assert_eq!(enc(int(23)), [0x17]);
        assert_eq!(enc(int(24)), [0x18, 0x18]);
        assert_eq!(enc(int(255)), [0x18, 0xff]);
        assert_eq!(enc(int(256)), [0x19, 0x01, 0x00]);
        assert_eq!(enc(int(1000)), [0x19, 0x03, 0xe8]);
        assert_eq!(enc(int(1_000_000)), [0x1a, 0x00, 0x0f, 0x42, 0x40]);
        assert_eq!(
            enc(int(1_000_000_000_000)),
            [0x1b, 0x00, 0x00, 0x00, 0xe8, 0xd4, 0xa5, 0x10, 0x00]
        );
        assert_eq!(enc(int(-1)), [0x20]);
        assert_eq!(enc(int(-1000)), [0x39, 0x03, 0xe7]);
        // Full unsigned range boundary: 2^64 - 1.
        assert_eq!(
            enc(int(18_446_744_073_709_551_615)),
            [0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
        );
    }

    // ── Shortest-form floats (RFC 8949 Appendix A preferred serializations) ──────
    #[test]
    fn float_shortest_form() {
        let f = |x: f64| enc(Value::Float(x));
        assert_eq!(f(0.0), [0xf9, 0x00, 0x00]);
        assert_eq!(f(-0.0), [0xf9, 0x80, 0x00]);
        assert_eq!(f(1.0), [0xf9, 0x3c, 0x00]);
        assert_eq!(f(1.5), [0xf9, 0x3e, 0x00]);
        assert_eq!(f(65504.0), [0xf9, 0x7b, 0xff]); // largest f16
        assert_eq!(f(5.960_464_477_539_063e-8), [0xf9, 0x00, 0x01]); // smallest f16 subnormal
        assert_eq!(f(100_000.0), [0xfa, 0x47, 0xc3, 0x50, 0x00]); // needs f32
        assert_eq!(
            f(3.402_823_466_385_288_6e38),
            [0xfa, 0x7f, 0x7f, 0xff, 0xff]
        ); // f32 max
        assert_eq!(
            f(1.1),
            [0xfb, 0x3f, 0xf1, 0x99, 0x99, 0x99, 0x99, 0x99, 0x9a]
        ); // needs f64
        assert_eq!(f(f64::INFINITY), [0xf9, 0x7c, 0x00]);
        assert_eq!(f(f64::NEG_INFINITY), [0xf9, 0xfc, 0x00]);
        assert_eq!(f(f64::NAN), [0xf9, 0x7e, 0x00]); // canonical quiet NaN
        // A real GPS coordinate cannot round-trip through f16/f32, so it stays f64.
        assert_eq!(enc(Value::Float(40.7128))[0], 0xfb);
    }

    // ── Map key ordering: bytewise lexicographic on ENCODED keys ────────────────
    #[test]
    fn map_keys_sorted_by_encoded_bytes() {
        // Deliberately out of order. Encoded keys:
        //   10   -> 0x0a            (integer key sorts before any text key)
        //   "a"  -> 0x61 0x61
        //   "b"  -> 0x61 0x62
        //   "aa" -> 0x62 0x61 0x61  (shorter "b" sorts before longer "aa" — bytewise, not length-first)
        let m = Value::Map(vec![
            (Value::Text("b".into()), int(1)),
            (Value::Text("aa".into()), int(2)),
            (int(10), int(3)),
            (Value::Text("a".into()), int(4)),
        ]);
        let got = enc(m);
        let expected = vec![
            0xa4, // map(4)
            0x0a, 0x03, // 10: 3
            0x61, 0x61, 0x04, // "a": 4
            0x61, 0x62, 0x01, // "b": 1
            0x62, 0x61, 0x61, 0x02, // "aa": 2
        ];
        assert_eq!(got, expected);
    }

    #[test]
    fn definite_length_only_and_nested() {
        // Arrays + nested maps use definite length heads.
        let v = Value::Array(vec![
            int(1),
            Value::Map(vec![(Value::Text("k".into()), Value::Bool(true))]),
            Value::Bytes(vec![0xde, 0xad]),
        ]);
        assert_eq!(
            enc(v),
            vec![0x83, 0x01, 0xa1, 0x61, 0x6b, 0xf5, 0x42, 0xde, 0xad]
        );
    }

    // ── Idempotence + round-trip stability ──────────────────────────────────────
    #[test]
    fn canonicalize_is_idempotent() {
        let m = Value::Map(vec![
            (Value::Text("z".into()), Value::Float(1.0)),
            (Value::Text("a".into()), int(2)),
        ]);
        let once = enc(m);
        let twice = canonicalize(&once).unwrap();
        assert_eq!(once, twice, "canonicalize must be a fixed point");
        // Decoding then re-encoding is stable too.
        assert_eq!(canonicalize(&twice).unwrap(), once);
    }

    #[test]
    fn unknown_keys_resorted_among_known() {
        // Mimics a sidecar's `_unknown` map merged with known fields: a future key
        // ("zzz") and an early key ("aaa") must interleave by encoded order, not append.
        let m = Value::Map(vec![
            (Value::Text("sidecar_schema".into()), int(1)),
            (Value::Text("zzz_future".into()), Value::Bool(true)),
            (Value::Text("aaa_future".into()), int(7)),
            (Value::Text("uuid".into()), Value::Text("x".into())),
        ]);
        let bytes = enc(m);
        let decoded: Value = ciborium::de::from_reader(bytes.as_slice()).unwrap();
        // Re-canonicalizing the decoded form yields identical bytes (sort is total + stable).
        assert_eq!(canonicalize(&bytes).unwrap(), bytes);
        // The bytewise sort is over the *encoded* key, whose first byte is the text
        // length head (major 3 | len). So "uuid" (head 0x64, len 4) sorts before the
        // longer keys "aaa_future"/"zzz_future" (head 0x6a, len 10) and "sidecar_schema"
        // (head 0x6e, len 14) — length dominates content here. Full order:
        // uuid, aaa_future, zzz_future, sidecar_schema.
        if let Value::Map(entries) = decoded {
            let keys: Vec<String> = entries
                .iter()
                .map(|(k, _)| match k {
                    Value::Text(s) => s.clone(),
                    _ => unreachable!(),
                })
                .collect();
            assert_eq!(keys, ["uuid", "aaa_future", "zzz_future", "sidecar_schema"]);
        } else {
            panic!("expected map");
        }
    }

    // ── Bignums (tags 2 and 3) in preferred form (RFC 8949 §3.4.3) ───────────────
    fn big(tag: u64, bytes: &[u8]) -> Value {
        Value::Tag(tag, Box::new(Value::Bytes(bytes.to_vec())))
    }

    #[test]
    fn bignum_that_fits_64_bits_is_the_plain_integer() {
        assert_eq!(enc(big(2, &[])), [0x00]);
        assert_eq!(enc(big(3, &[])), [0x20]); // -1 - 0
        assert_eq!(enc(big(2, &[0x00, 0x00, 0x01])), [0x01]);
        assert_eq!(enc(big(3, &[0x00, 0x03, 0xe7])), [0x39, 0x03, 0xe7]); // -1000
        assert_eq!(
            enc(big(2, &[0xff; 8])),
            [0x1b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
        );
        assert_eq!(
            enc(big(
                3,
                &[0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
            )),
            [0x3b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]
        ); // -2^64
    }

    #[test]
    fn bignum_beyond_64_bits_drops_leading_zeros() {
        // 2^64, written with two leading zero bytes.
        let two_pow_64 = [0x01, 0, 0, 0, 0, 0, 0, 0, 0];
        let expected = [0xc2, 0x49, 0x01, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(enc(big(2, &[&[0, 0][..], &two_pow_64].concat())), expected);
        assert_eq!(enc(big(2, &two_pow_64)), expected);
        // -1 - 2^64.
        assert_eq!(
            enc(big(3, &[&[0][..], &two_pow_64].concat())),
            [0xc3, 0x49, 0x01, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        // A magnitude wider than 128 bits keeps every significant byte.
        let wide: Vec<u8> = (1..=17).collect();
        let mut expected = vec![0xc2, 0x51];
        expected.extend_from_slice(&wide);
        assert_eq!(enc(big(2, &[&[0][..], &wide].concat())), expected);
        // A bignum tag over anything but a byte string is just a tag.
        assert_eq!(enc(Value::Tag(2, Box::new(int(5)))), [0xc2, 0x05]);
    }

    #[test]
    fn canonicalize_is_a_fixed_point_on_bignums() {
        let inputs: [&[u8]; 6] = [
            &[0xc2, 0x40],                                     // tag 2 over an empty byte string
            &[0xc2, 0x5f, 0xff],                               // ... framed indefinite-length
            &[0xc2, 0x5f, 0xff, 0xff, 0xff, 0xff, 0x40, 0x5f], // the fuzzer's find (#749)
            &[0xc3, 0x5f, 0x42, 0x03, 0xe7, 0xff],             // -1000, indefinite-length
            &[0xc2, 0x4a, 0x00, 0x01, 0, 0, 0, 0, 0, 0, 0, 0], // 2^64 with a leading zero
            &[
                0xc2, 0x52, 0x00, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17,
            ],
        ];
        for input in inputs {
            let once = canonicalize(input).unwrap();
            assert_eq!(
                canonicalize(&once).unwrap(),
                once,
                "input {}",
                hex::encode(input)
            );
        }
        assert_eq!(canonicalize(&[0xc2, 0x40]).unwrap(), [0x00]);
        assert_eq!(canonicalize(&[0xc2, 0x5f, 0xff]).unwrap(), [0x00]);
        assert_eq!(
            canonicalize(&[0xc3, 0x5f, 0x42, 0x03, 0xe7, 0xff]).unwrap(),
            [0x39, 0x03, 0xe7]
        );
    }

    #[test]
    fn canonicalize_refuses_a_negative_bignum_below_i128() {
        // -1 - 2^127: a 16-byte magnitude with its top bit set.
        let mut magnitude = vec![0x80];
        magnitude.extend_from_slice(&[0; 15]);
        let mut definite = vec![0xc3, 0x50];
        definite.extend_from_slice(&magnitude);
        assert!(canonicalize(&definite).is_err());
        // The same value framed so the decoder accepts it, nested in an array.
        let mut indefinite = vec![0x81, 0xc3, 0x5f, 0x51, 0x00];
        indefinite.extend_from_slice(&magnitude);
        indefinite.push(0xff);
        assert!(matches!(
            canonicalize(&indefinite),
            Err(CanonicalError::Deserialize(_))
        ));
        // One below that bound still canonicalizes.
        let mut fits = vec![0xc3, 0x5f, 0x50, 0x7f];
        fits.extend_from_slice(&[0xff; 15]);
        fits.push(0xff);
        let once = canonicalize(&fits).unwrap();
        assert_eq!(canonicalize(&once).unwrap(), once);
    }

    // ── Golden vector: the cross-language conformance contract ───────────────────
    #[test]
    fn golden_vector_is_stable() {
        // A fixed logical document → a fixed hex string. Any port (Swift/Kotlin/JS) MUST
        // reproduce this exactly. Changing it is a breaking, signature-invalidating change.
        let doc = Value::Map(vec![
            (Value::Text("schema".into()), int(1)),
            (Value::Text("ok".into()), Value::Bool(true)),
            (Value::Text("ratio".into()), Value::Float(0.5)),
            (
                Value::Text("digest".into()),
                Value::Bytes(vec![0x01, 0x02, 0x03]),
            ),
            (
                Value::Text("nested".into()),
                Value::Array(vec![int(-1), int(256)]),
            ),
        ]);
        let hex = hex::encode(enc(doc));
        // Keys sort by encoded bytes: "ok"(62) < "ratio"(65) < "digest"(66 64) <
        // "nested"(66 6e) < "schema"(66 73). 0.5 -> f16 0xf93800; bytes -> 0x43..;
        // [-1,256] -> 0x82 20 19 0100; 1 -> 0x01.
        let expected = concat!(
            "a5",
            "626f6b",
            "f5",
            "657261746",
            "96f",
            "f93800",
            "6664696765737443010203",
            "666e65737465648220190100",
            "66736368656d6101",
        );
        assert_eq!(hex, expected);
    }

    #[test]
    fn struct_serializes_through_canonical() {
        #[derive(serde::Serialize, serde::Deserialize, PartialEq, Debug)]
        struct S {
            b: u32,
            a: u32,
        }
        // Field order in the struct is (b, a) but canonical output sorts keys "a" < "b".
        let bytes = to_canonical_vec(&S { b: 2, a: 1 }).unwrap();
        assert_eq!(bytes, vec![0xa2, 0x61, 0x61, 0x01, 0x61, 0x62, 0x02]);
        let back: S = from_slice(&bytes).unwrap();
        assert_eq!(back, S { b: 2, a: 1 });
    }
}
