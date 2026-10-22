//! Forward compatibility of the provenance wire format (SSoT: [Versioning — System Invariants],
//! invariant 2; ADR-0009): a manifest keeps the keys a newer writer added, re-emits them
//! canonically and stays verifiable over them, and a provenance record leads with its version.
//!
//! [Versioning — System Invariants]: https://docs/design/versioning/#system-invariants

use capsule_core::cbor;
use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::{AmkVersion, HybridSigningKey};
use capsule_core::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use capsule_core::crypto::provenance::{
    ASSET_MANIFEST_VERSION, Action, AssetManifest, KeyMode, ManifestCore,
    PROVENANCE_RECORD_VERSION, ProvenanceRecord,
};
use ciborium::value::Value;
use proptest::prelude::*;
use uuid::Uuid;

const ASSET: Uuid = Uuid::from_u128(0xF11E);

fn device() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
}

fn write_tier() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32])
}

/// A structurally valid `create` core with no unknown keys.
fn core() -> ManifestCore {
    ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL_VERSION.into(),
        file_id: ASSET,
        album_id: Uuid::from_u128(0xA1),
        amk_version: AmkVersion(1),
        ciphertext_hash: Hash32([0xCC; 32]),
        plaintext_size: 1024,
        chunk_size: 65_520,
        nonce_prefix: [1, 2, 3, 4, 5, 6, 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: Some(Hash32([0x4D; 32])),
        created_by_user: Uuid::from_u128(0x05E2),
        created_by_device: Uuid::from_u128(0xD1),
        client_version: "capsule-cli/0.1.0+0123456789ab".into(),
        timestamp: "2026-10-21T00:00:00Z".into(),
        action: Action::Create,
        prior_provenance_hash: None,
        retention_until: None,
        upgraded_from: None,
        unknown: Default::default(),
    }
}

fn verifies(manifest: &AssetManifest) -> bool {
    let bytes = manifest.signing_bytes();
    device()
        .verifying_key()
        .verify(&bytes, &manifest.device_sig)
        && write_tier()
            .verifying_key()
            .verify(&bytes, &manifest.write_sig)
}

fn to_value<T: serde::Serialize>(v: &T) -> Value {
    cbor::from_slice(&cbor::to_canonical_vec(v).expect("serializes")).expect("re-reads")
}

/// Arbitrary CBOR a future writer might put under a new key: scalars of every major type,
/// tagged values, and nested arrays and text-keyed maps. Bignum tags (2 and 3) are left out
/// because a canonical writer only uses them for integers that do not fit the integer type.
fn arb_value() -> impl Strategy<Value = Value> {
    let leaf = prop_oneof![
        any::<i64>().prop_map(Value::from),
        any::<u64>().prop_map(Value::from),
        any::<bool>().prop_map(Value::Bool),
        Just(Value::Null),
        any::<f64>().prop_map(Value::Float),
        ".{0,16}".prop_map(Value::Text),
        prop::collection::vec(any::<u8>(), 0..16).prop_map(Value::Bytes),
    ];
    leaf.prop_recursive(3, 24, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..4).prop_map(Value::Array),
            prop::collection::btree_map("[a-z]{1,6}", inner.clone(), 0..4).prop_map(|m| {
                Value::Map(m.into_iter().map(|(k, v)| (Value::Text(k), v)).collect())
            }),
            (4u64..100_000, inner).prop_map(|(tag, v)| Value::Tag(tag, Box::new(v))),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// Unknown keys survive a canonical round trip byte for byte, stay under both signatures,
    /// and cannot be stripped without failing verification.
    #[test]
    fn unknown_keys_round_trip_canonically_under_the_signature(
        unknown in prop::collection::btree_map("x_[a-z0-9_]{0,12}", arb_value(), 1..5),
    ) {
        let mut c = core();
        c.unknown = unknown.clone();
        let manifest = c.sign(&device(), &write_tier()).expect("signs");
        let bytes = cbor::to_canonical_vec(&manifest).expect("encodes");

        let back: AssetManifest = cbor::from_slice(&bytes).expect("decodes");
        prop_assert_eq!(
            back.core.unknown.keys().collect::<Vec<_>>(),
            unknown.keys().collect::<Vec<_>>()
        );
        prop_assert_eq!(&cbor::to_canonical_vec(&back).expect("re-encodes"), &bytes);
        prop_assert_eq!(back.signing_bytes(), manifest.signing_bytes());
        prop_assert!(verifies(&back));

        let mut stripped = back;
        stripped.core.unknown.clear();
        prop_assert!(!verifies(&stripped), "dropping an unknown key must fail verification");
    }
}

/// The current writer emits no unknown keys, so adding the map moved no signature: a core with
/// an empty map encodes exactly the known fields.
#[test]
fn an_empty_unknown_map_is_wire_absent() {
    let value = to_value(&core());
    let keys: Vec<&str> = value
        .as_map()
        .expect("a map")
        .iter()
        .filter_map(|(k, _)| k.as_text())
        .collect();
    assert!(!keys.contains(&"unknown"));
    assert_eq!(
        keys.len(),
        18,
        "the eighteen fields a create core always carries: {keys:?}"
    );
}

/// A "future" writer that does not use this crate's types: it builds the record as raw CBOR,
/// adds a manifest field this build has never heard of (holding a tag and a nested map), signs
/// the canonical core it built, and wraps it as a current-version record. This build decodes
/// it, verifies both signatures over the preserved field, and re-emits the record byte for
/// byte, so the chain hash a later record names is the digest of the bytes served.
#[test]
fn a_record_from_a_future_writer_verifies_and_round_trips_byte_identically() {
    let Value::Map(mut core_entries) = to_value(&core()) else {
        panic!("a core is a map")
    };
    core_entries.push((
        Value::Text("future_field".into()),
        Value::Map(vec![
            (
                Value::Text("captured".into()),
                Value::Tag(1, Box::new(Value::from(1_790_000_000u64))),
            ),
            (
                Value::Text("weights".into()),
                Value::Array(vec![Value::Float(0.5), Value::from(-3)]),
            ),
        ]),
    ));
    core_entries.push((Value::Text("z".into()), Value::Bytes(vec![0xFF; 4])));
    let core_value = Value::Map(core_entries);
    let signed_bytes = cbor::value_to_canonical_vec(&core_value);
    let device_sig = device().sign(&signed_bytes);
    let write_sig = write_tier().sign(&signed_bytes);
    let manifest_value = Value::Map(vec![
        (Value::Text("core".into()), core_value),
        (Value::Text("device_sig".into()), to_value(&device_sig)),
        (Value::Text("write_sig".into()), to_value(&write_sig)),
    ]);
    let record_value = Value::Map(vec![
        (
            Value::Text("version".into()),
            Value::from(PROVENANCE_RECORD_VERSION),
        ),
        (Value::Text("asset_id".into()), to_value(&ASSET)),
        (Value::Text("manifest".into()), manifest_value),
        (Value::Text("prior_provenance_hash".into()), Value::Null),
    ]);
    let wire = cbor::value_to_canonical_vec(&record_value);

    let record = ProvenanceRecord::decode(&wire).expect("a current-version record decodes");
    assert_eq!(record.version, PROVENANCE_RECORD_VERSION);
    let unknown = &record.manifest.core.unknown;
    assert_eq!(unknown.keys().collect::<Vec<_>>(), ["future_field", "z"]);
    assert_eq!(record.manifest.signing_bytes(), signed_bytes);
    assert!(verifies(&record.manifest));
    assert_eq!(cbor::to_canonical_vec(&record).expect("re-encodes"), wire);
    assert_eq!(record.record_hash(), hash_bytes(&wire));
}
