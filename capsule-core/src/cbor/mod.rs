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

use ciborium::value::Value;
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
pub fn canonicalize(bytes: &[u8]) -> Result<Vec<u8>, CanonicalError> {
    let value: Value =
        ciborium::de::from_reader(bytes).map_err(|e| CanonicalError::Deserialize(e.to_string()))?;
    Ok(value_to_canonical_vec(&value))
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
        assert_eq!(f(5.960464477539063e-8), [0xf9, 0x00, 0x01]); // smallest f16 subnormal
        assert_eq!(f(100000.0), [0xfa, 0x47, 0xc3, 0x50, 0x00]); // needs f32
        assert_eq!(f(3.4028234663852886e38), [0xfa, 0x7f, 0x7f, 0xff, 0xff]); // f32 max
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
