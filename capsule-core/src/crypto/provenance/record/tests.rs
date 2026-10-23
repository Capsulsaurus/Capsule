use super::*;
use crate::crypto::keys::{AmkVersion, HybridSigningKey};
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::crypto::provenance::{ASSET_MANIFEST_VERSION, Action, KeyMode, ManifestCore};

const ASSET: u128 = 0xF11E;

fn dev() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
}
fn wt() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32])
}

fn record(action: Action, prior: Option<Hash32>) -> ProvenanceRecord {
    let core = ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL_VERSION.into(),
        file_id: Uuid::from_u128(ASSET),
        album_id: Uuid::from_u128(0xA1),
        amk_version: AmkVersion(1),
        ciphertext_hash: Hash32([0xCC; 32]),
        plaintext_size: 10,
        chunk_size: 65_520,
        nonce_prefix: [0; 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: None,
        created_by_user: Uuid::from_u128(0x05E2),
        created_by_device: Uuid::from_u128(0xD1),
        client_version: "t".into(),
        timestamp: "2026-05-31T00:00:00Z".into(),
        action,
        prior_provenance_hash: prior,
        upgraded_from: None,
        retention_until: None,
        unknown: Default::default(),
    };
    ProvenanceRecord::new(
        Uuid::from_u128(ASSET),
        core.sign(&dev(), &wt()).unwrap(),
        prior,
    )
}

#[test]
fn the_version_leads_the_canonical_encoding() {
    let bytes = cbor::to_canonical_vec(&record(Action::Create, None)).unwrap();
    let value: ciborium::Value = cbor::from_slice(&bytes).unwrap();
    let first = value.as_map().unwrap().first().unwrap();
    assert_eq!(first.0.as_text(), Some("version"));
    assert_eq!(
        ProvenanceRecord::decode(&bytes).unwrap(),
        record(Action::Create, None)
    );
}

/// Re-encode `rec` with its `version` replaced by `version` (or removed for `None`).
fn with_version(rec: &ProvenanceRecord, version: Option<ciborium::Value>) -> Vec<u8> {
    let value: ciborium::Value = cbor::from_slice(&cbor::to_canonical_vec(rec).unwrap()).unwrap();
    let mut entries: Vec<_> = value
        .into_map()
        .unwrap()
        .into_iter()
        .filter(|(k, _)| k.as_text() != Some("version"))
        .collect();
    if let Some(v) = version {
        entries.push(("version".into(), v));
    }
    cbor::value_to_canonical_vec(&ciborium::Value::Map(entries))
}

#[test]
fn a_newer_version_is_unsupported_on_every_decode_path() {
    let bytes = with_version(&record(Action::Create, None), Some(2.into()));
    assert_eq!(
        ProvenanceRecord::decode(&bytes),
        Err(RecordDecodeError::UnsupportedVersion {
            found: 2,
            max_known: PROVENANCE_RECORD_VERSION
        })
    );
    // The plain serde path (backup restore, rebuild, open) refuses it too.
    assert!(cbor::from_slice::<ProvenanceRecord>(&bytes).is_err());
}

#[test]
fn a_missing_or_zero_version_is_malformed() {
    let rec = record(Action::Create, None);
    for bytes in [with_version(&rec, None), with_version(&rec, Some(0.into()))] {
        assert!(matches!(
            ProvenanceRecord::decode(&bytes),
            Err(RecordDecodeError::Malformed(_))
        ));
    }
}

fn build_chain() -> ProvenanceChain {
    let mut chain = ProvenanceChain::new();
    chain.append(record(Action::Create, None)).unwrap();
    let h1 = chain.head().unwrap();
    chain
        .append(record(Action::MetadataUpdate, Some(h1)))
        .unwrap();
    let h2 = chain.head().unwrap();
    chain.append(record(Action::Delete, Some(h2))).unwrap();
    chain
}

#[test]
fn build_and_walk_a_valid_chain() {
    let chain = build_chain();
    assert_eq!(chain.records().len(), 3);
    ProvenanceChain::verify_walk(chain.records()).unwrap();
}

#[test]
fn non_create_root_is_rejected() {
    let mut chain = ProvenanceChain::new();
    // MetadataUpdate with null prior: mirrors (both null) but is not a create.
    assert_eq!(
        chain.append(record(Action::MetadataUpdate, None)),
        Err(ChainError::BadRoot)
    );
}

#[test]
fn second_create_is_rejected() {
    let mut chain = ProvenanceChain::new();
    chain.append(record(Action::Create, None)).unwrap();
    assert_eq!(
        chain.append(record(Action::Create, None)),
        Err(ChainError::BadRoot)
    );
}

#[test]
fn stale_prior_hash_breaks_the_link() {
    let mut chain = ProvenanceChain::new();
    chain.append(record(Action::Create, None)).unwrap();
    // Append with a wrong (stale) prior hash → BrokenLink.
    assert_eq!(
        chain.append(record(Action::Delete, Some(Hash32([0xEE; 32])))),
        Err(ChainError::BrokenLink)
    );
}

#[test]
fn rewriting_a_past_record_is_detected_by_the_walk() {
    let chain = build_chain();
    let mut records = chain.records().to_vec();
    // Tamper the middle record's timestamp (re-sign so its own sigs still verify, but the
    // chain hash it produced changes, breaking the downstream link).
    records[1].manifest.core.timestamp = "1999-01-01T00:00:00Z".into();
    assert_eq!(
        ProvenanceChain::verify_walk(&records),
        Err(ChainError::BrokenLink),
        "a rewritten middle record breaks the forward walk"
    );
}

#[test]
fn dropping_a_record_is_detected() {
    let chain = build_chain();
    let mut records = chain.records().to_vec();
    records.remove(1); // drop the metadata-update
    assert_eq!(
        ProvenanceChain::verify_walk(&records),
        Err(ChainError::BrokenLink)
    );
}

#[test]
fn manifest_prior_must_mirror_record_prior() {
    let mut chain = ProvenanceChain::new();
    chain.append(record(Action::Create, None)).unwrap();
    let head = chain.head().unwrap();
    // Build a record whose manifest prior disagrees with the record prior.
    let mut rec = record(Action::Delete, Some(head));
    rec.manifest.core.prior_provenance_hash = Some(Hash32([0x77; 32]));
    assert_eq!(chain.append(rec), Err(ChainError::MirrorMismatch));
}
