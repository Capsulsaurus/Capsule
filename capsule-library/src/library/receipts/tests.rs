use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::HybridSigningKey;
use capsule_core::crypto::receipts::BlobRole;
use tempfile::TempDir;

use super::*;

fn signing_key(seed: u8) -> HybridSigningKey {
    HybridSigningKey::from_seed64(&[seed; 64])
}

fn core(seq: u64, ct: Hash32, size: u64) -> CustodyReceiptCore {
    CustodyReceiptCore {
        version: "custody-receipt/v1".into(),
        crypto_suite_id: 1,
        protocol_version: "2026-07-10".into(),
        server_id: "capsule.example".into(),
        server_key_id: Hash32([0x11; 32]),
        receipt_seq: seq,
        prior_receipt_hash: None,
        upload_id: Uuid::from_u128(9).to_string(),
        asset_id: Uuid::from_u128(seq as u128 + 100).to_string(),
        blob_role: "original".into(),
        ciphertext_hash: ct,
        size,
        envelope_hash: None,
        uploaded_by_user: Uuid::from_u128(7).to_string(),
        uploaded_by_device: None,
        received_at: "2026-07-10T00:00:00Z".into(),
        unknown: Default::default(),
    }
}

fn sign(key: &HybridSigningKey, core: CustodyReceiptCore) -> CustodyReceipt {
    let server_sig = key.sign(&core.signing_bytes());
    CustodyReceipt { core, server_sig }
}

fn expect(ct: Hash32, size: u64) -> ReceiptExpectations {
    ReceiptExpectations {
        ciphertext_hash: ct,
        size,
        role: BlobRole::Original,
        envelope_hash: None,
    }
}

const NOW: i64 = 1_752_105_600; // ~2025-07-10

#[test]
fn verified_receipt_round_trips_through_canonical_cbor() {
    let key = signing_key(1);
    let ct = Hash32([0xAB; 32]);
    let receipt = sign(&key, core(1, ct, 4096));
    let bytes = receipt.to_canonical_cbor();
    let decoded = CustodyReceipt::from_canonical_cbor(&bytes).unwrap();
    assert_eq!(decoded, receipt);
    assert!(verify_receipt(&decoded, &[key.verifying_key()], &expect(ct, 4096), NOW).is_ok());
}

#[test]
fn wrong_key_or_tamper_or_field_mismatch_rejects() {
    let key = signing_key(1);
    let ct = Hash32([0xAB; 32]);
    let receipt = sign(&key, core(1, ct, 4096));

    // Signed by a different attestation key.
    let other = signing_key(2);
    assert_eq!(
        verify_receipt(&receipt, &[other.verifying_key()], &expect(ct, 4096), NOW),
        Err(ReceiptRejection::Signature)
    );
    // A one-byte core mutation breaks the signature.
    let mut tampered = receipt.clone();
    tampered.core.size = 4097;
    assert_eq!(
        verify_receipt(&tampered, &[key.verifying_key()], &expect(ct, 4097), NOW),
        Err(ReceiptRejection::Signature)
    );
    // Valid signature but the server took custody of a different hash than we sent.
    assert_eq!(
        verify_receipt(
            &receipt,
            &[key.verifying_key()],
            &expect(Hash32([0xCD; 32]), 4096),
            NOW
        ),
        Err(ReceiptRejection::FieldMismatch("ciphertext_hash"))
    );
}

#[test]
fn gross_clock_drift_rejects() {
    let key = signing_key(1);
    let ct = Hash32([0xAB; 32]);
    let receipt = sign(&key, core(1, ct, 4096));
    // Client clock is a decade past the receipt's `received_at`.
    let far_future = NOW + 10 * 366 * 24 * 3600;
    assert_eq!(
        verify_receipt(
            &receipt,
            &[key.verifying_key()],
            &expect(ct, 4096),
            far_future
        ),
        Err(ReceiptRejection::ClockDrift)
    );
}

#[test]
fn append_is_chain_ordered_and_idempotent() {
    let dir = TempDir::new().unwrap();
    let root = dir.path();
    let key = signing_key(1);
    // Two receipts for the same asset (same asset_id via fixed seq→uuid), appended out of order.
    let asset = Uuid::from_u128(101);
    let mut c1 = core(1, Hash32([1; 32]), 10);
    c1.asset_id = asset.to_string();
    let mut c2 = core(2, Hash32([2; 32]), 20);
    c2.asset_id = asset.to_string();
    let r2 = sign(&key, c2);
    let r1 = sign(&key, c1);

    append_receipt(root, None, &r2).unwrap();
    append_receipt(root, None, &r1).unwrap();
    // Re-appending r1 is a no-op (idempotent on receipt_seq).
    append_receipt(root, None, &r1).unwrap();

    let log = load_receipts(root, &asset, None).unwrap();
    assert_eq!(log.len(), 2, "idempotent: no duplicate");
    assert_eq!(log[0].core.receipt_seq, 1, "chain order");
    assert_eq!(log[1].core.receipt_seq, 2);
}

/// A receipt from a newer server that adds fields this build has never heard of — built as
/// raw CBOR, as a writer that does not share these types would — still verifies here, so
/// an older client does not mistake it for a withheld receipt and refuse to release. The
/// fields survive the persisted log byte for byte, and dropping them fails verification.
#[test]
fn a_receipt_from_a_newer_server_verifies_and_keeps_its_unknown_fields() {
    use ciborium::Value;

    let key = signing_key(1);
    let ct = Hash32([0xAB; 32]);
    let asset = Uuid::from_u128(101);
    let mut known = core(1, ct, 4096);
    known.asset_id = asset.to_string();
    let Value::Map(mut entries) = cbor::from_slice::<Value>(&known.signing_bytes()).unwrap() else {
        panic!("a receipt core is a map")
    };
    entries.push((
        Value::Text("x_replica_count".into()),
        Value::Tag(2, Box::new(Value::Bytes(vec![0x01; 17]))),
    ));
    entries.push((
        Value::Text("x_region".into()),
        Value::Map(vec![(Value::Text("weight".into()), Value::Float(0.5))]),
    ));
    let future_core = Value::Map(entries);
    let signed_bytes = cbor::value_to_canonical_vec(&future_core);
    let server_sig: Value =
        cbor::from_slice(&cbor::to_canonical_vec(&key.sign(&signed_bytes)).unwrap()).unwrap();
    let wire = cbor::value_to_canonical_vec(&Value::Map(vec![
        (Value::Text("core".into()), future_core),
        (Value::Text("server_sig".into()), server_sig),
    ]));

    let receipt = CustodyReceipt::from_canonical_cbor(&wire).unwrap();
    assert_eq!(
        receipt.core.unknown.keys().collect::<Vec<_>>(),
        ["x_region", "x_replica_count"]
    );
    assert_eq!(receipt.core.signing_bytes(), signed_bytes);
    assert_eq!(receipt.to_canonical_cbor(), wire);
    assert!(verify_receipt(&receipt, &[key.verifying_key()], &expect(ct, 4096), NOW).is_ok());

    let dir = TempDir::new().unwrap();
    append_receipt(dir.path(), None, &receipt).unwrap();
    let log = load_receipts(dir.path(), &asset, None).unwrap();
    assert_eq!(log, std::slice::from_ref(&receipt));
    assert_eq!(log[0].to_canonical_cbor(), wire);
    assert!(verify_receipt(&log[0], &[key.verifying_key()], &expect(ct, 4096), NOW).is_ok());

    let mut stripped = receipt;
    stripped.core.unknown.clear();
    assert_eq!(
        verify_receipt(&stripped, &[key.verifying_key()], &expect(ct, 4096), NOW),
        Err(ReceiptRejection::Signature),
        "dropping an unknown field must fail verification"
    );
}

/// Everything this build issues has an empty unknown map, and an empty map is wire-absent,
/// so no receipt signed before the map existed changes its signing bytes.
#[test]
fn an_empty_unknown_map_is_wire_absent() {
    let value: ciborium::Value =
        cbor::from_slice(&core(1, Hash32([1; 32]), 1).signing_bytes()).unwrap();
    let keys: Vec<_> = value
        .as_map()
        .unwrap()
        .iter()
        .filter_map(|(k, _)| k.as_text())
        .collect();
    assert_eq!(keys.len(), 13, "{keys:?}");
    assert!(!keys.contains(&"unknown"), "{keys:?}");
}

#[test]
fn missing_log_is_empty() {
    let dir = TempDir::new().unwrap();
    let log = load_receipts(dir.path(), &Uuid::from_u128(1), None).unwrap();
    assert!(log.is_empty());
}
