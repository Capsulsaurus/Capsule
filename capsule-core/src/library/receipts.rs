//! Client-side custody receipts — the accountability half of the verify-before-destroy gate
//! (slice `S-D4` in the repo-root `SLICES.md`; SSoT:
//! [Storage Verification — Custody Receipts](https://docs/design/import/storage-verification/)).
//!
//! A [`CustodyReceipt`] is the server-signed complement of the client-signed provenance chain:
//! the envelope proves what a client *claimed and signed*; the receipt proves what the server
//! *accepted*, over a ciphertext hash the server recomputed itself. Before dropping
//! irreplaceable local bytes a client requires a **verified** receipt for the write, so a
//! server that withholds receipts never becomes the sole holder of an only-copy.
//!
//! This module is the client's **persistence** path, and only that. The receipt type, its
//! verification and the blob-role enum moved to [`crate::crypto::receipts`] (`S-C46`) so the
//! server that *issues* receipts shares one definition instead of mirroring it — a signed
//! structure defined twice is a signature that eventually stops verifying, and the failure would
//! look like the server withholding receipts. The receipt type and its verification are
//! re-exported here and through [`crate::library`], so every path a client already uses keeps
//! working. The role enum is not: it is reached as
//! [`crypto::receipts::BlobRole`](crate::crypto::receipts::BlobRole), and as
//! [`library::BlobRole`](crate::library::BlobRole) through the storage-verify barrel.
//!
//! Persistence is first-class, not a cache: the log is appended to
//! `media/{YYYY}/{YYYY-MM}/{uuid}.receipts.cbor` and included verbatim in the backup artifact —
//! a server destroying the record of its own liability is exactly the adversary.

use std::fs;
use std::path::Path;

use uuid::Uuid;

use crate::cbor::{self, CanonicalError};
pub use crate::crypto::receipts::{
    CustodyReceipt, CustodyReceiptCore, ReceiptExpectations, ReceiptRejection, verify_receipt,
};
use crate::library::paths::receipts_path;

/// Read the persisted custody-receipt log for an asset. An absent file is an empty log (`Ok`).
pub fn load_receipts(
    root: &Path,
    uuid: &Uuid,
    capture_utc: Option<i64>,
) -> Result<Vec<CustodyReceipt>, CanonicalError> {
    let path = receipts_path(root, uuid, capture_utc);
    match fs::read(&path) {
        Ok(bytes) if bytes.is_empty() => Ok(Vec::new()),
        Ok(bytes) => cbor::from_slice(&bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(CanonicalError::Deserialize(e.to_string())),
    }
}

/// Errors persisting a receipt beside the provenance chain.
#[derive(Debug, thiserror::Error)]
pub enum ReceiptStoreError {
    /// The receipt log could not be read/decoded or (re-)encoded.
    #[error("receipt log codec: {0}")]
    Codec(#[from] CanonicalError),
    /// A filesystem error writing the log.
    #[error("receipt log I/O: {0}")]
    Io(String),
}

/// Append a verified receipt to the asset's on-disk log (`{uuid}.receipts.cbor`), idempotent on
/// `receipt_seq`: re-persisting a receipt already present is a no-op, so a retried fetch never
/// duplicates the log. The log is stored as canonical CBOR of the receipt `Vec` in chain order.
pub fn append_receipt(
    root: &Path,
    capture_utc: Option<i64>,
    receipt: &CustodyReceipt,
) -> Result<(), ReceiptStoreError> {
    let uuid = Uuid::parse_str(&receipt.core.asset_id)
        .map_err(|e| ReceiptStoreError::Io(format!("receipt asset_id not a UUID: {e}")))?;
    let mut log = load_receipts(root, &uuid, capture_utc)?;
    if log
        .iter()
        .any(|r| r.core.receipt_seq == receipt.core.receipt_seq)
    {
        return Ok(());
    }
    log.push(receipt.clone());
    log.sort_by_key(|r| r.core.receipt_seq);
    let path = receipts_path(root, &uuid, capture_utc);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| ReceiptStoreError::Io(e.to_string()))?;
    }
    let bytes = cbor::to_canonical_vec(&log)?;
    fs::write(&path, bytes).map_err(|e| ReceiptStoreError::Io(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::crypto::hash::Hash32;
    use crate::crypto::keys::{HybridSignature, HybridSigningKey, HybridVerifyingKey};
    use crate::crypto::receipts::BlobRole;

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

    #[test]
    fn missing_log_is_empty() {
        let dir = TempDir::new().unwrap();
        let log = load_receipts(dir.path(), &Uuid::from_u128(1), None).unwrap();
        assert!(log.is_empty());
    }
}
