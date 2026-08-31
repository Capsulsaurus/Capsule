//! The receipt log's own suite.
//!
//! Invariant 33 in three properties: the chain is strictly monotonic, every entry names its
//! predecessor, and there is no way to change one after the fact — the last being a property of
//! the port's shape rather than a case, since no method replaces or removes.

use capsule_core::crypto::keys::HybridVerifyingKey;

use super::*;

/// A key, and the identity it signs under.
fn attestation_key() -> LocalAttestationKey {
    LocalAttestationKey::new("capsule.test", HybridSigningKey::generate())
}

/// A draft for `upload` of `bytes`.
fn draft(upload: &str, asset: &str, bytes: &[u8]) -> ReceiptDraft {
    ReceiptDraft {
        crypto_suite_id: capsule_core::crypto::CRYPTO_SUITE_ID,
        protocol_version: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_owned(),
        upload_id: UploadId::new(upload),
        asset_id: AssetId::new(asset),
        blob_role: "original".to_owned(),
        ciphertext_hash: hash_bytes(bytes),
        size: bytes.len() as u64,
        envelope_hash: Some(hash_bytes(b"the envelope")),
        uploaded_by_user: "01937b7c-0000-7000-8000-000000000001".to_owned(),
        uploaded_by_device: None,
        received_at: "2026-01-01T00:00:00Z".to_owned(),
    }
}

/// Verify a receipt the way a client does.
fn verifies(receipt: &CustodyReceipt, under: &HybridVerifyingKey) -> bool {
    receipt.verify_under(under)
}

#[tokio::test]
async fn a_receipt_verifies_under_the_key_it_names() {
    let log = InMemoryReceipts::new();
    let key = attestation_key();
    let receipt = log
        .issue(draft("u1", "a1", b"the bytes"), &key)
        .await
        .expect("the log issues");

    assert!(verifies(&receipt, &key.verifying_key()));
    assert_eq!(receipt.core.server_id, "capsule.test");
    assert_eq!(
        receipt.core.server_key_id,
        key.key_id(),
        "the fingerprint is derived from the key that signs, so it cannot name a different one"
    );
    assert_eq!(receipt.core.version, RECEIPT_VERSION);

    // Another server's key does not verify it — the binding a cross-server replay is refused on.
    assert!(!verifies(&receipt, &attestation_key().verifying_key()));
}

#[tokio::test]
async fn the_chain_is_monotonic_and_each_entry_names_its_predecessor() {
    let log = InMemoryReceipts::new();
    let key = attestation_key();

    let first = log
        .issue(draft("u1", "a1", b"one"), &key)
        .await
        .expect("issue");
    let second = log
        .issue(draft("u2", "a2", b"two"), &key)
        .await
        .expect("issue");
    let third = log
        .issue(draft("u3", "a3", b"three"), &key)
        .await
        .expect("issue");

    assert_eq!(
        (
            first.core.receipt_seq,
            second.core.receipt_seq,
            third.core.receipt_seq
        ),
        (1, 2, 3)
    );
    assert_eq!(
        first.core.prior_receipt_hash, None,
        "the first receipt has no predecessor, and an absent field says so rather than a zero"
    );
    assert_eq!(
        second.core.prior_receipt_hash,
        Some(hash_bytes(&first.to_canonical_cbor())),
    );
    assert_eq!(
        third.core.prior_receipt_hash,
        Some(hash_bytes(&second.to_canonical_cbor())),
        "the chain is over the *signed* bytes, so an altered predecessor breaks its successor"
    );
}

#[tokio::test]
async fn a_reissued_upload_returns_the_same_receipt() {
    let log = InMemoryReceipts::new();
    let key = attestation_key();
    let first = log
        .issue(draft("u1", "a1", b"one"), &key)
        .await
        .expect("issue");
    let again = log
        .issue(draft("u1", "a1", b"one"), &key)
        .await
        .expect("issue");

    assert_eq!(
        first, again,
        "a retried finalization must not mint a second receipt for one custody event: the chain \
         would carry two signed statements about the same bytes, which is indistinguishable \
         from the server double-counting"
    );
    assert_eq!(
        log.for_asset(&AssetId::new("a1"))
            .await
            .expect("read")
            .len(),
        1,
    );
}

#[tokio::test]
async fn receipts_are_found_by_upload_and_by_asset() {
    let log = InMemoryReceipts::new();
    let key = attestation_key();
    log.issue(draft("u1", "a1", b"one"), &key)
        .await
        .expect("issue");
    log.issue(draft("u2", "a1", b"two"), &key)
        .await
        .expect("issue");
    log.issue(draft("u3", "a2", b"three"), &key)
        .await
        .expect("issue");

    assert_eq!(
        log.for_upload(&UploadId::new("u2"))
            .await
            .expect("read")
            .map(|receipt| receipt.core.receipt_seq),
        Some(2),
    );
    assert_eq!(
        log.for_upload(&UploadId::new("never-issued"))
            .await
            .expect("read"),
        None,
    );

    let asset = log.for_asset(&AssetId::new("a1")).await.expect("read");
    assert_eq!(
        asset.iter().map(|r| r.core.receipt_seq).collect::<Vec<_>>(),
        vec![1, 2],
        "an asset's receipts come back in chain order, which is the order they can be walked in"
    );
}

#[tokio::test]
async fn the_receipt_carries_the_facts_the_server_established() {
    let log = InMemoryReceipts::new();
    let key = attestation_key();
    let bytes = b"the ciphertext the server stored";
    let receipt = log
        .issue(draft("u1", "a1", bytes), &key)
        .await
        .expect("issue");

    assert_eq!(receipt.core.ciphertext_hash, hash_bytes(bytes));
    assert_eq!(receipt.core.size, bytes.len() as u64);
    assert_eq!(receipt.core.blob_role, "original");
}

#[test]
fn a_key_never_reaches_a_log_line() {
    let printed = format!("{:?}", attestation_key());
    assert!(printed.contains("capsule.test"));
    assert!(
        !printed.contains("signing"),
        "the identity is printable and the secret is not, which is why `Debug` is hand-written"
    );
}
