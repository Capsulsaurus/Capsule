use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::HybridSigningKey;
use capsule_core::crypto::storage_attestation::{
    ATTESTATION_NONCE_LEN, AttestationNonce, AttestedAsset, AttestedBlob,
    STORAGE_ATTESTATION_VERSION, StorageAttestationCore,
};
use tempfile::TempDir;

use super::*;

fn key() -> HybridSigningKey {
    HybridSigningKey::from_seed64(&[7; 64])
}

/// A signed attestation answering for `assets`, under `nonce`, as canonical CBOR.
fn signed(assets: &[Uuid], nonce: AttestationNonce, stored: bool) -> Vec<u8> {
    let core = StorageAttestationCore {
        version: STORAGE_ATTESTATION_VERSION.to_owned(),
        server_id: "capsule.example".to_owned(),
        server_key_id: Hash32([0x11; 32]),
        account: Uuid::from_u128(0xACC).to_string(),
        nonce,
        checked_at: "2026-10-21T00:00:00Z".to_owned(),
        verdicts: assets
            .iter()
            .map(|asset| AttestedAsset {
                asset_id: asset.to_string(),
                durable: stored,
                blobs: vec![AttestedBlob {
                    hash: Hash32([0x01; 32]),
                    role: "original".to_owned(),
                    stored,
                    indexed: stored,
                    retrievable: stored,
                    deep: None,
                }],
            })
            .collect(),
    };
    StorageAttestation {
        server_sig: key().sign(&core.signing_bytes()),
        core,
    }
    .to_canonical_cbor()
}

const CAPTURE: Option<i64> = Some(1_721_001_600);

#[test]
fn a_persisted_attestation_round_trips_byte_for_byte_and_still_verifies() {
    let dir = TempDir::new().unwrap();
    let asset = Uuid::from_u128(1);
    let bytes = signed(&[asset], [1; ATTESTATION_NONCE_LEN], true);

    assert!(append_attestation(dir.path(), &asset, CAPTURE, &bytes).unwrap());

    let raw = load_attestation_log(dir.path(), &asset, CAPTURE).unwrap();
    assert_eq!(raw, vec![bytes.clone()], "kept exactly as received");
    let decoded = load_attestations(dir.path(), &asset, CAPTURE).unwrap();
    assert_eq!(decoded.len(), 1);
    assert!(
        decoded[0].verify_under(&key().verifying_key()),
        "the kept copy is still evidence: its signature verifies"
    );
    assert!(attestations_path(dir.path(), &asset, CAPTURE).exists());
    assert!(
        !tmp_path(&attestations_path(dir.path(), &asset, CAPTURE)).exists(),
        "no temporary is left behind"
    );
}

#[test]
fn appending_is_idempotent_on_the_nonce_and_keeps_arrival_order() {
    let dir = TempDir::new().unwrap();
    let asset = Uuid::from_u128(2);
    let first = signed(&[asset], [9; ATTESTATION_NONCE_LEN], true);
    // A later audit reports the blob lost: both are evidence, in the order they arrived.
    let second = signed(&[asset], [3; ATTESTATION_NONCE_LEN], false);

    assert!(append_attestation(dir.path(), &asset, CAPTURE, &first).unwrap());
    assert!(append_attestation(dir.path(), &asset, CAPTURE, &second).unwrap());
    assert!(
        !append_attestation(dir.path(), &asset, CAPTURE, &first).unwrap(),
        "a retried persist is a no-op"
    );

    let raw = load_attestation_log(dir.path(), &asset, CAPTURE).unwrap();
    assert_eq!(raw, vec![first, second]);
}

#[test]
fn one_attestation_lands_in_the_log_of_every_asset_it_names() {
    let dir = TempDir::new().unwrap();
    let (a, b) = (Uuid::from_u128(3), Uuid::from_u128(4));
    let bytes = signed(&[a, b], [5; ATTESTATION_NONCE_LEN], true);

    assert!(append_attestation(dir.path(), &a, CAPTURE, &bytes).unwrap());
    assert!(append_attestation(dir.path(), &b, CAPTURE, &bytes).unwrap());

    assert_eq!(
        load_attestation_log(dir.path(), &a, CAPTURE).unwrap(),
        vec![bytes.clone()]
    );
    assert_eq!(
        load_attestation_log(dir.path(), &b, CAPTURE).unwrap(),
        vec![bytes]
    );
}

#[test]
fn an_attestation_about_another_asset_is_refused_and_writes_nothing() {
    let dir = TempDir::new().unwrap();
    let asset = Uuid::from_u128(5);
    let other = signed(&[Uuid::from_u128(6)], [1; ATTESTATION_NONCE_LEN], true);

    let err = append_attestation(dir.path(), &asset, CAPTURE, &other).unwrap_err();
    assert!(
        matches!(err, AttestationStoreError::NotAboutAsset(id) if id == asset),
        "got {err:?}"
    );
    assert!(!attestations_path(dir.path(), &asset, CAPTURE).exists());
}

#[test]
fn bytes_that_are_not_an_attestation_are_refused() {
    let dir = TempDir::new().unwrap();
    let asset = Uuid::from_u128(7);
    let err = append_attestation(dir.path(), &asset, CAPTURE, b"not cbor at all").unwrap_err();
    assert!(
        matches!(err, AttestationStoreError::Codec(_)),
        "got {err:?}"
    );
    assert!(!attestations_path(dir.path(), &asset, CAPTURE).exists());
}

#[test]
fn a_missing_log_is_empty() {
    let dir = TempDir::new().unwrap();
    let asset = Uuid::from_u128(8);
    assert!(
        load_attestation_log(dir.path(), &asset, CAPTURE)
            .unwrap()
            .is_empty()
    );
    assert!(
        load_attestations(dir.path(), &asset, CAPTURE)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_corrupt_log_is_a_codec_error_not_an_empty_one() {
    let dir = TempDir::new().unwrap();
    let asset = Uuid::from_u128(9);
    let path = attestations_path(dir.path(), &asset, CAPTURE);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, b"\xFF garbage").unwrap();

    assert!(matches!(
        load_attestation_log(dir.path(), &asset, CAPTURE),
        Err(AttestationStoreError::Codec(_))
    ));
    // And an append refuses to paper over it rather than truncating the evidence.
    let bytes = signed(&[asset], [2; ATTESTATION_NONCE_LEN], true);
    assert!(append_attestation(dir.path(), &asset, CAPTURE, &bytes).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"\xFF garbage");
}
