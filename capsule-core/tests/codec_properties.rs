//! The canonical codecs, as properties over generated inputs (testing plan, `capsule-core` row:
//! "Canonical CBOR round-trips; unknown fields survive a re-sign; every decoder rejects
//! malformed input without panicking").
//!
//! - **Canonical CBOR** ([Metadata — Canonical CBOR Encoding]): the encoding of any value is a
//!   fixed point of `canonicalize`, and it does not depend on the order a map's entries were
//!   built in — the two facts that let two peers sign and verify the same bytes.
//! - **Signed documents** (the sidecar, the album and stack records): a signed document
//!   round-trips byte-identically with its signature intact, and keys this build does not know
//!   survive a decode and a re-sign by another key verbatim, under the new signature.
//! - **Decoders under hostile input**: every public decoder of untrusted bytes returns an error
//!   for arbitrary bytes and for a truncated valid encoding — it never panics. The `cargo-fuzz`
//!   targets in `capsule-core/fuzz` search the same surfaces coverage-guided; these cases are
//!   the bounded, seeded slice of that search every test run pays for.
//!
//! Every suite runs under [`support::seeded`]: a fixed, printed seed that a failure replays from.
//!
//! [Metadata — Canonical CBOR Encoding]: https://docs/design/metadata/#canonical-cbor-encoding

mod support;

use std::collections::BTreeMap;

use capsule_core::cbor;
use capsule_core::crypto::encryption::open_blob;
use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::{HybridSigningKey, HybridVerifyingKey};
use capsule_core::crypto::primitives::CRYPTO_SUITE_ID;
use capsule_core::crypto::provenance::ProvenanceRecord;
use capsule_core::crypto::receipts::CustodyReceipt;
use capsule_core::crypto::storage_attestation::StorageAttestation;
use capsule_core::lifecycle::EnrollmentPayload;
use capsule_core::lqip::Lqip;
use capsule_core::media::{Decoder, RawshiftDecoder, StillFormat};
use capsule_core::metadata::crdt::{AddId, Lww, OrSet};
use capsule_core::sidecar::{
    ALBUM_DOC_SCHEMA_V1, AlbumDocV1, DocError, Regions, SIDECAR_SCHEMA_V1, STACK_DOC_SCHEMA_V1,
    SidecarV1, StackDocV1,
};
use ciborium::value::{Integer, Value};
use proptest::prelude::*;
use uuid::Uuid;

const ALBUM: Uuid = Uuid::from_u128(0xA1B);
const STACK: Uuid = Uuid::from_u128(0x51);
const TAGS: [&str; 4] = ["beach", "family", "hdr", "Zoë"];

// ── Generators ───────────────────────────────────────────────────────────────

/// A finite float, or one of the values whose canonical form is special (`±0`, `±∞`).
/// NaN is left out of the value generators because `Value`'s equality is IEEE equality, under
/// which a NaN never equals itself; its canonical byte form is pinned by the unit vectors.
fn float() -> impl Strategy<Value = f64> {
    prop_oneof![
        any::<f32>()
            .prop_filter("finite", |f| f.is_finite())
            .prop_map(f64::from),
        any::<f64>().prop_filter("finite", |f| f.is_finite()),
        Just(0.0),
        Just(-0.0),
        Just(f64::INFINITY),
        Just(f64::NEG_INFINITY),
        Just(0.5),
        Just(65504.0),
    ]
}

fn leaf() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<i64>().prop_map(|n| Value::Integer(n.into())),
        any::<u64>().prop_map(|n| Value::Integer(n.into())),
        proptest::collection::vec(any::<u8>(), 0..24).prop_map(Value::Bytes),
        "[a-zA-Z0-9_ é]{0,12}".prop_map(Value::Text),
        any::<bool>().prop_map(Value::Bool),
        Just(Value::Null),
        float().prop_map(Value::Float),
    ]
}

/// A CBOR value tree: leaves under arrays, tags and maps keyed by text or integers.
fn value() -> impl Strategy<Value = Value> {
    leaf().prop_recursive(4, 48, 6, |inner| {
        let key = prop_oneof![
            "[a-z]{0,6}".prop_map(Value::Text),
            any::<i16>().prop_map(|n| Value::Integer(n.into())),
        ];
        prop_oneof![
            proptest::collection::vec(inner.clone(), 0..6).prop_map(Value::Array),
            proptest::collection::vec((key, inner.clone()), 0..6).prop_map(unique_keys),
            (0u64..64, inner)
                .prop_map(|(tag, v)| Value::Tag(tag, Box::new(v)))
                .prop_filter("decodable bignum", readable),
            bignum(),
        ]
    })
}

/// A bignum (tag 2 or 3 over a byte string), with leading zero bytes and magnitudes on
/// either side of the 64-bit and 128-bit boundaries, so the preferred-form rules all run.
///
/// The one value left out is a negative bignum below `-2^127` (a 16-byte magnitude with its
/// top bit set): the decoder refuses it, so it has no canonical form to be a fixed point of,
/// and `canonicalize` refuses it on the way in (pinned by the `cbor` unit tests).
fn bignum() -> impl Strategy<Value = Value> {
    (
        2u64..=3,
        0usize..3,
        proptest::collection::vec(any::<u8>(), 0..20),
    )
        .prop_map(|(tag, zeros, magnitude)| {
            let mut bytes = vec![0u8; zeros];
            bytes.extend(magnitude);
            Value::Tag(tag, Box::new(Value::Bytes(bytes)))
        })
        .prop_filter("decodable bignum", readable)
}

/// Whether `v` is anything but a negative bignum below `-2^127`, the one value with no
/// canonical form the decoder reads back.
fn readable(v: &Value) -> bool {
    match v {
        Value::Tag(3, inner) => match &**inner {
            Value::Bytes(b) => {
                let significant: Vec<u8> = b.iter().copied().skip_while(|&b| b == 0).collect();
                !(significant.len() == 16 && significant[0] & 0x80 != 0)
            }
            _ => true,
        },
        _ => true,
    }
}

/// A map from generated entries, keeping the first entry per key: a CBOR map with a repeated
/// key is not valid ([RFC 8949 §5.6]), so no writer this codec answers to produces one.
///
/// [RFC 8949 §5.6]: https://www.rfc-editor.org/rfc/rfc8949#section-5.6
fn unique_keys(entries: Vec<(Value, Value)>) -> Value {
    let mut seen = Vec::new();
    Value::Map(
        entries
            .into_iter()
            .filter(|(k, _)| {
                let encoded = cbor::value_to_canonical_vec(k);
                let fresh = !seen.contains(&encoded);
                seen.push(encoded);
                fresh
            })
            .collect(),
    )
}

/// Keys no current schema defines, as a newer writer would add them.
fn unknown_fields() -> impl Strategy<Value = BTreeMap<String, Value>> {
    proptest::collection::btree_map("zz_future_[a-z]{1,6}", leaf(), 0..4)
}

fn add_id(dev: u8, counter: u64) -> AddId {
    AddId {
        device: Uuid::from_u128(0xD0 + u128::from(dev)),
        counter,
    }
}

fn tags() -> impl Strategy<Value = OrSet<String>> {
    proptest::collection::vec((0u8..3, 0u64..8, 0usize..TAGS.len(), any::<bool>()), 0..6).prop_map(
        |ops| {
            let mut set = OrSet::new();
            for (dev, counter, tag, removed) in ops {
                let id = add_id(dev, counter);
                set.add(TAGS[tag].to_string(), id);
                if removed {
                    set.remove(id).expect("just added");
                }
            }
            set
        },
    )
}

fn caption() -> impl Strategy<Value = Lww<String>> {
    proptest::collection::vec((0u8..60, 0u8..3, "[a-z ]{0,10}"), 0..3).prop_map(|writes| {
        let mut reg = Lww::new();
        for (second, dev, text) in writes {
            reg.set(
                text,
                format!("2026-05-31T10:00:{second:02}Z"),
                Uuid::from_u128(0xD0 + u128::from(dev)),
            );
        }
        reg
    })
}

fn sidecar() -> impl Strategy<Value = SidecarV1> {
    (tags(), caption(), unknown_fields(), any::<[u8; 32]>()).prop_map(
        |(tags_user, caption, unknown, hash)| SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: CRYPTO_SUITE_ID,
            uuid: Uuid::from_u128(0x7777),
            hash: Hash32(hash),
            capture_timestamp: "2026-05-31T10:00:00Z".into(),
            capture_timestamp_user: Lww::new(),
            import_timestamp: "2026-05-31T11:00:00Z".into(),
            content_type: "image/jpeg".into(),
            dimensions: None,
            lqip: None,
            tags_user,
            tags_ai: OrSet::new(),
            caption,
            rating: Lww::new(),
            stack_membership: Lww::new(),
            regions: Regions::new(),
            cull: Lww::new(),
            hidden: Lww::new(),
            camera_id: None,
            device_id: Uuid::from_u128(0xD1),
            session_id: Uuid::from_u128(0x5E),
            gps: None,
            gps_user: Lww::new(),
            provenance_chain_hash: None,
            unknown,
            signature: None,
        },
    )
}

fn album_doc() -> impl Strategy<Value = AlbumDocV1> {
    (tags(), caption(), unknown_fields()).prop_map(|(labels, name, unknown)| {
        let mut doc = AlbumDocV1::new(ALBUM);
        doc.labels = labels;
        doc.name = name;
        doc.unknown = unknown;
        doc
    })
}

fn stack_doc() -> impl Strategy<Value = StackDocV1> {
    (tags(), unknown_fields()).prop_map(|(labels, unknown)| {
        let mut doc = StackDocV1::new(STACK, ALBUM);
        doc.labels = labels;
        doc.unknown = unknown;
        doc
    })
}

/// A signing key derived from `seed`, so a case replays with the same key.
fn key(seed: u8) -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[seed; 32], &[seed.wrapping_add(1); 32])
}

/// The value a signed document encodes as, with its signature entry removed: what an older
/// reader that re-signed it must have kept.
fn without_signature(bytes: &[u8]) -> Value {
    let Value::Map(entries) = cbor::from_slice::<Value>(bytes).expect("decodes") else {
        panic!("a signed document is a map");
    };
    Value::Map(
        entries
            .into_iter()
            .filter(|(k, _)| k != &Value::Text("signature".into()))
            .collect(),
    )
}

/// Canonical bytes of an arbitrary value with every map's entries reversed.
fn reversed_maps(value: &Value) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.iter().map(reversed_maps).collect()),
        Value::Map(entries) => Value::Map(
            entries
                .iter()
                .rev()
                .map(|(k, v)| (reversed_maps(k), reversed_maps(v)))
                .collect(),
        ),
        Value::Tag(tag, inner) => Value::Tag(*tag, Box::new(reversed_maps(inner))),
        other => other.clone(),
    }
}

// ── Canonical CBOR ───────────────────────────────────────────────────────────

proptest! {
    #![proptest_config(support::seeded(512))]

    /// Decoding canonical bytes and encoding them again changes nothing.
    #[test]
    fn canonical_encoding_is_a_fixed_point(v in value()) {
        let once = cbor::value_to_canonical_vec(&v);
        let twice = cbor::canonicalize(&once).expect("canonical bytes decode");
        prop_assert_eq!(&twice, &once);
        let decoded: Value = cbor::from_slice(&once).expect("canonical bytes decode");
        prop_assert_eq!(cbor::value_to_canonical_vec(&decoded), once);
    }

    /// The order a writer built a map in never reaches the bytes a signature covers.
    #[test]
    fn canonical_encoding_ignores_map_entry_order(v in value()) {
        prop_assert_eq!(
            cbor::value_to_canonical_vec(&reversed_maps(&v)),
            cbor::value_to_canonical_vec(&v)
        );
    }

    /// Integers take their shortest head, whatever width the writer used.
    #[test]
    fn integers_round_trip_through_the_canonical_codec(n in any::<i64>(), m in any::<u64>()) {
        let v = Value::Array(vec![Value::Integer(n.into()), Value::Integer(m.into())]);
        let bytes = cbor::value_to_canonical_vec(&v);
        let back: (i64, u64) = cbor::from_slice(&bytes).expect("round-trips");
        prop_assert_eq!(back, (n, m));
        prop_assert_eq!(cbor::to_canonical_vec(&(n, m)).expect("serializes"), bytes);
    }
}

// ── Signed documents ─────────────────────────────────────────────────────────

proptest! {
    // Each case signs with ML-DSA-65, so fewer cases than the codec suites.
    #![proptest_config(support::seeded(48))]

    #[test]
    fn signed_sidecar_round_trips_byte_identically(mut doc in sidecar(), seed in any::<u8>()) {
        let ik = key(seed);
        doc.sign(&ik);
        let bytes = doc.to_canonical_vec();
        let back = SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).expect("decodes");
        prop_assert!(back.verify(&ik.verifying_key()));
        prop_assert_eq!(back.to_canonical_vec(), bytes);
        prop_assert_eq!(back, doc);
    }

    #[test]
    fn sidecar_unknown_fields_survive_a_re_sign(
        mut doc in sidecar(), first in any::<u8>(), second in any::<u8>()
    ) {
        doc.sign(&key(first));
        let written = doc.to_canonical_vec();
        let mut reader = SidecarV1::from_canonical_slice(&written, SIDECAR_SCHEMA_V1)
            .expect("decodes");
        reader.sign(&key(second));
        let rewritten = reader.to_canonical_vec();
        prop_assert!(reader.verify(&key(second).verifying_key()));
        prop_assert_eq!(&reader.unknown, &doc.unknown);
        prop_assert_eq!(without_signature(&rewritten), without_signature(&written));
    }

    #[test]
    fn album_doc_round_trips_and_keeps_unknown_fields_across_a_re_sign(
        mut doc in album_doc(), first in any::<u8>(), second in any::<u8>()
    ) {
        doc.sign(&key(first));
        let written = doc.to_canonical_vec();
        let mut back = AlbumDocV1::from_canonical_slice(&written, ALBUM_DOC_SCHEMA_V1)
            .expect("decodes");
        prop_assert!(back.verify(&key(first).verifying_key()));
        prop_assert_eq!(back.to_canonical_vec(), written.clone());
        prop_assert_eq!(&back, &doc);

        back.sign(&key(second));
        prop_assert!(back.verify(&key(second).verifying_key()));
        prop_assert_eq!(without_signature(&back.to_canonical_vec()), without_signature(&written));
    }

    #[test]
    fn stack_doc_round_trips_and_keeps_unknown_fields_across_a_re_sign(
        mut doc in stack_doc(), first in any::<u8>(), second in any::<u8>()
    ) {
        doc.sign(&key(first));
        let written = doc.to_canonical_vec();
        let mut back = StackDocV1::from_canonical_slice(&written, STACK_DOC_SCHEMA_V1)
            .expect("decodes");
        prop_assert!(back.verify(&key(first).verifying_key()));
        prop_assert_eq!(back.to_canonical_vec(), written.clone());
        prop_assert_eq!(&back, &doc);

        back.sign(&key(second));
        prop_assert!(back.verify(&key(second).verifying_key()));
        prop_assert_eq!(without_signature(&back.to_canonical_vec()), without_signature(&written));
    }
}

proptest! {
    // Each case signs and verifies with ML-DSA-65, so fewer cases than the codec suites.
    #![proptest_config(support::seeded(48))]

    #[test]
    fn no_decoder_panics_on_a_corrupted_signed_document(
        mut doc in album_doc(),
        flips in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..4),
    ) {
        doc.sign(&key(1));
        let mut bytes = doc.to_canonical_vec();
        for (at, mask) in flips {
            let i = at.index(bytes.len());
            bytes[i] ^= mask | 1;
        }
        decode_everything(&bytes);
        if let Ok(back) = AlbumDocV1::from_canonical_slice(&bytes, ALBUM_DOC_SCHEMA_V1) {
            // A flip the decoder tolerated must still be caught by the signature.
            prop_assert!(!back.verify(&key(1).verifying_key()) || back.to_canonical_vec() == doc.to_canonical_vec());
        }
    }
}

proptest! {
    #![proptest_config(support::seeded(256))]

    /// A document from a newer schema is refused, never stripped of what it cannot read.
    #[test]
    fn a_newer_schema_is_refused(doc in album_doc(), newer in (ALBUM_DOC_SCHEMA_V1 + 1)..=u16::MAX) {
        let mut doc = doc;
        doc.album_doc_schema = newer;
        let refused = AlbumDocV1::from_canonical_slice(&doc.to_canonical_vec(), ALBUM_DOC_SCHEMA_V1);
        prop_assert!(
            matches!(refused, Err(DocError::SchemaTooNew { found, .. }) if found == newer),
            "schema {} was not refused: {:?}", newer, refused
        );
    }

    /// Every strict prefix of a signed document is an error: CBOR here is definite-length,
    /// so a cut-off encoding cannot decode as a shorter valid one.
    #[test]
    fn a_truncated_document_never_decodes(doc in album_doc(), cut in any::<prop::sample::Index>()) {
        let bytes = doc.to_canonical_vec();
        let prefix = &bytes[..cut.index(bytes.len())];
        prop_assert!(AlbumDocV1::from_canonical_slice(prefix, ALBUM_DOC_SCHEMA_V1).is_err());
        prop_assert!(cbor::canonicalize(prefix).is_err());
    }
}

// ── Decoders under hostile input ─────────────────────────────────────────────

/// Drive every public decoder of untrusted bytes over `bytes`. Each must return; the only
/// assertion is the one that holds for every input: no decoder panics, and a decoded LQIP
/// renders, as its contract promises.
fn decode_everything(bytes: &[u8]) {
    let _ = cbor::canonicalize(bytes);
    let _ = cbor::from_slice::<Value>(bytes);
    let _ = SidecarV1::from_canonical_slice(bytes, SIDECAR_SCHEMA_V1);
    let _ = AlbumDocV1::from_canonical_slice(bytes, ALBUM_DOC_SCHEMA_V1);
    let _ = StackDocV1::from_canonical_slice(bytes, STACK_DOC_SCHEMA_V1);
    let _ = CustodyReceipt::from_canonical_cbor(bytes);
    let _ = StorageAttestation::from_canonical_cbor(bytes);
    let _ = ProvenanceRecord::decode(bytes);
    let _ = HybridVerifyingKey::from_bytes(bytes);
    let _ = open_blob(&[7; 32], bytes);
    let _ = StillFormat::from_bytes(bytes);
    let _ = RawshiftDecoder.probe(bytes, "jpg");
    let _ = EnrollmentPayload::decode(&String::from_utf8_lossy(bytes));
    if let Ok(lqip) = Lqip::from_bytes(bytes) {
        let _ = lqip.decode_capped(32, 32);
    }
}

/// Bytes shaped like the inputs these decoders see: raw noise, and CBOR maps with text keys
/// the documents define, holding arbitrary values.
fn hostile_bytes() -> impl Strategy<Value = Vec<u8>> {
    let field = prop_oneof![
        Just("album_id"),
        Just("labels"),
        Just("name"),
        Just("stack_id"),
        Just("signature"),
        Just("uuid"),
        Just("hash"),
        Just("core"),
        Just("server_sig"),
        Just("verdicts"),
        Just("version"),
        Just("asset_id"),
        Just("manifest"),
        Just("prior_provenance_hash"),
    ];
    prop_oneof![
        proptest::collection::vec(any::<u8>(), 0..512),
        proptest::collection::vec((field, value()), 0..6).prop_map(|fields| {
            let mut entries: Vec<(Value, Value)> = fields
                .into_iter()
                .map(|(k, v)| (Value::Text(k.into()), v))
                .collect();
            entries.push((
                Value::Integer(Integer::from(0u8)),
                Value::Integer(1u8.into()),
            ));
            cbor::value_to_canonical_vec(&Value::Map(entries))
        }),
    ]
}

proptest! {
    #![proptest_config(support::seeded(512))]

    #[test]
    fn no_decoder_panics_on_hostile_bytes(bytes in hostile_bytes()) {
        decode_everything(&bytes);
    }
}
