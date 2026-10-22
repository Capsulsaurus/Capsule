use super::*;
use crate::crypto::keys::HybridSigningKey;

fn key(seed: u8) -> HybridSigningKey {
    HybridSigningKey::from_seed64(&[seed; 64])
}

const NONCE: AttestationNonce = [0x5A; ATTESTATION_NONCE_LEN];

fn declared() -> Vec<DeclaredAsset> {
    vec![DeclaredAsset {
        asset_id: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61".to_owned(),
        blob_hashes: vec![Hash32([0x01; 32]), Hash32([0x02; 32])],
    }]
}

fn core(nonce: AttestationNonce) -> StorageAttestationCore {
    StorageAttestationCore {
        version: STORAGE_ATTESTATION_VERSION.to_owned(),
        server_id: "capsule.example".to_owned(),
        server_key_id: Hash32([0x11; 32]),
        account: "01937b7c-0000-7000-8000-000000000001".to_owned(),
        nonce,
        checked_at: "2026-10-21T00:00:00Z".to_owned(),
        verdicts: vec![AttestedAsset {
            asset_id: "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61".to_owned(),
            durable: true,
            blobs: vec![
                AttestedBlob {
                    hash: Hash32([0x01; 32]),
                    role: "provenance".to_owned(),
                    stored: true,
                    indexed: true,
                    retrievable: true,
                    deep: None,
                },
                AttestedBlob {
                    hash: Hash32([0x02; 32]),
                    role: "metadata".to_owned(),
                    stored: true,
                    indexed: true,
                    retrievable: true,
                    deep: Some("intact".to_owned()),
                },
            ],
        }],
    }
}

fn sign(key: &HybridSigningKey, core: StorageAttestationCore) -> StorageAttestation {
    StorageAttestation {
        server_sig: key.sign(&core.signing_bytes()),
        core,
    }
}

#[test]
fn a_signed_attestation_verifies_under_the_pinned_key_and_round_trips() {
    let key = key(1);
    let attestation = sign(&key, core(NONCE));
    let decoded = StorageAttestation::from_canonical_cbor(&attestation.to_canonical_cbor())
        .expect("an attestation round-trips");
    assert_eq!(decoded, attestation);
    assert_eq!(
        verify_storage_attestation(&decoded, &[key.verifying_key()], &NONCE, &declared()),
        Ok(())
    );
}

#[test]
fn any_key_in_the_pinned_history_verifies() {
    let retired = key(2);
    let attestation = sign(&retired, core(NONCE));
    assert_eq!(
        verify_storage_attestation(
            &attestation,
            &[retired.verifying_key(), key(1).verifying_key()],
            &NONCE,
            &declared()
        ),
        Ok(())
    );
}

#[test]
fn an_attestation_under_another_key_is_rejected() {
    let attestation = sign(&key(1), core(NONCE));
    assert_eq!(
        verify_storage_attestation(&attestation, &[key(2).verifying_key()], &NONCE, &declared()),
        Err(AttestationRejection::Signature)
    );
}

#[test]
fn a_tampered_verdict_is_rejected() {
    let key = key(1);
    let mut attestation = sign(&key, core(NONCE));
    // A server that lost the bytes after signing cannot flip the verdict and keep the signature.
    attestation.core.verdicts[0].blobs[1].stored = false;
    assert_eq!(
        verify_storage_attestation(&attestation, &[key.verifying_key()], &NONCE, &declared()),
        Err(AttestationRejection::Signature)
    );

    let mut nonce_swapped = sign(&key, core(NONCE));
    nonce_swapped.core.nonce = [0x00; ATTESTATION_NONCE_LEN];
    assert_eq!(
        verify_storage_attestation(
            &nonce_swapped,
            &[key.verifying_key()],
            &[0x00; ATTESTATION_NONCE_LEN],
            &declared()
        ),
        Err(AttestationRejection::Signature),
        "rewriting the nonce to match a new request breaks the signature"
    );
}

#[test]
fn a_replayed_attestation_is_rejected_by_its_nonce() {
    let key = key(1);
    // Validly signed — for an earlier request.
    let earlier = sign(&key, core([0x01; ATTESTATION_NONCE_LEN]));
    assert_eq!(
        verify_storage_attestation(&earlier, &[key.verifying_key()], &NONCE, &declared()),
        Err(AttestationRejection::Nonce)
    );
}

#[test]
fn an_unknown_version_is_rejected() {
    let key = key(1);
    let mut core = core(NONCE);
    core.version = "storage-attestation/v2".to_owned();
    let attestation = sign(&key, core);
    assert_eq!(
        verify_storage_attestation(&attestation, &[key.verifying_key()], &NONCE, &declared()),
        Err(AttestationRejection::Version)
    );
}

#[test]
fn an_attestation_answering_a_different_question_is_rejected() {
    let key = key(1);
    let attestation = sign(&key, core(NONCE));
    let pinned = [key.verifying_key()];

    let mut other_asset = declared();
    other_asset[0].asset_id = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e62".to_owned();
    assert_eq!(
        verify_storage_attestation(&attestation, &pinned, &NONCE, &other_asset),
        Err(AttestationRejection::Mismatch("asset_id"))
    );

    let mut reordered = declared();
    reordered[0].blob_hashes.reverse();
    assert_eq!(
        verify_storage_attestation(&attestation, &pinned, &NONCE, &reordered),
        Err(AttestationRejection::Mismatch("blob_hashes"))
    );

    let mut fewer = declared();
    fewer[0].blob_hashes.pop();
    assert_eq!(
        verify_storage_attestation(&attestation, &pinned, &NONCE, &fewer),
        Err(AttestationRejection::Mismatch("blob_hashes"))
    );

    let mut more = declared();
    more.push(more[0].clone());
    assert_eq!(
        verify_storage_attestation(&attestation, &pinned, &NONCE, &more),
        Err(AttestationRejection::Mismatch("asset count"))
    );
}

#[test]
fn a_custody_receipt_signature_does_not_verify_as_an_attestation() {
    // The two signed structures share a key; the version and field set keep them apart.
    use crate::crypto::receipts::{CustodyReceipt, CustodyReceiptCore};
    let key = key(1);
    let receipt_core = CustodyReceiptCore {
        version: "custody-receipt/v1".to_owned(),
        crypto_suite_id: 1,
        protocol_version: "2026-07-10".to_owned(),
        server_id: "capsule.example".to_owned(),
        server_key_id: Hash32([0x11; 32]),
        receipt_seq: 1,
        prior_receipt_hash: None,
        upload_id: "u".to_owned(),
        asset_id: "a".to_owned(),
        blob_role: "original".to_owned(),
        ciphertext_hash: Hash32([0x22; 32]),
        size: 1,
        envelope_hash: None,
        uploaded_by_user: "x".to_owned(),
        uploaded_by_device: None,
        received_at: "2026-10-21T00:00:00Z".to_owned(),
        unknown: Default::default(),
    };
    let receipt = CustodyReceipt {
        server_sig: key.sign(&receipt_core.signing_bytes()),
        core: receipt_core,
    };
    assert!(
        StorageAttestation::from_canonical_cbor(&receipt.to_canonical_cbor()).is_err(),
        "a receipt does not decode as an attestation"
    );
}

#[test]
fn an_absent_deep_verdict_is_an_absent_key() {
    // The wire-presence discipline: `deep: None` must not encode as a present null, or a verdict
    // that never looked at the bytes would sign differently across encoders.
    let mut core = core(NONCE);
    core.verdicts[0].blobs[1].deep = None;
    let bytes = core.signing_bytes();
    let value: ciborium::Value = ciborium::from_reader(bytes.as_slice()).expect("cbor");
    let text = format!("{value:?}");
    assert!(
        !text.contains("\"deep\""),
        "no deep key when no deep scan ran"
    );
}
