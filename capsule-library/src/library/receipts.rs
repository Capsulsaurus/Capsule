//! Client-side custody receipts — the accountability half of the verify-before-destroy gate
//! (SSoT:
//! [Transfer — Durability: Custody Receipts](https://docs/design/transfer/durability/#custody-receipts)).
//!
//! A [`CustodyReceipt`] is the server-signed complement of the client-signed provenance chain:
//! the envelope proves what a client *claimed and signed*; the receipt proves what the server
//! *accepted*, over a ciphertext hash the server recomputed itself. Before dropping
//! irreplaceable local bytes a client requires a **verified** receipt for the write, so a
//! server that withholds receipts never becomes the sole holder of an only-copy.
//!
//! This module is the client's **persistence** path, and only that. The receipt type, its
//! verification and the blob-role enum moved to [`capsule_core::crypto::receipts`] so the
//! server that *issues* receipts shares one definition instead of mirroring it — a signed
//! structure defined twice is a signature that eventually stops verifying, and the failure would
//! look like the server withholding receipts. The receipt type and its verification are
//! re-exported here and through [`crate::library`], so every path a client already uses keeps
//! working. The role enum is not: it is reached as
//! [`crypto::receipts::BlobRole`](capsule_core::crypto::receipts::BlobRole), and as
//! [`library::BlobRole`](crate::library::BlobRole) through the storage-verify barrel.
//!
//! Persistence is first-class, not a cache: the log is appended to
//! `media/{YYYY}/{YYYY-MM}/{uuid}.receipts.cbor` and included verbatim in the backup artifact —
//! a server destroying the record of its own liability is exactly the adversary.

use std::fs;
use std::path::Path;

use capsule_core::cbor::{self, CanonicalError};
pub use capsule_core::crypto::receipts::{
    CustodyReceipt, CustodyReceiptCore, ReceiptExpectations, ReceiptRejection, verify_receipt,
};
use uuid::Uuid;

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
mod tests;
