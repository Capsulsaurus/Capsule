//! Client-side signed storage attestations — the evidence form of the durability verdict, kept
//! beside the asset's custody receipts (SSoT:
//! [Transfer — Durability: Signed Storage Attestation](https://docs/design/transfer/durability/#signed-storage-attestation)).
//!
//! A [`StorageAttestation`] is the server's signed answer to "do you hold these blobs?", bound
//! to a nonce the client drew. A receipt proves the server *accepted* a blob; an attestation
//! proves what the server *said it held* at `checked_at`, including an honest server's
//! self-reported loss. Proof of loss names "a signed attestation reporting `H` not stored" as
//! the evidence of later non-holding, so an attestation the client threw away is evidence lost.
//!
//! This module is the client's **persistence** path, and only that. The type and its
//! verification live in [`crate::crypto::storage_attestation`], shared with the server that
//! signs it. Nothing here verifies a signature — the library holds no attestation keys — so a
//! caller appends only an attestation it has already verified for the request it made
//! (`capsule_sdk::verify::StorageVerifyClient::verify_signed`).
//!
//! # The log
//!
//! `media/{YYYY}/{YYYY-MM}/{uuid}.attestations.cbor` is canonical CBOR of an array of byte
//! strings, each one attestation exactly as the server signed it, in the order the client
//! received them. One attestation answers for every asset a request asked about, so the same
//! bytes are appended to the log of each asset it names. The log is append-only and
//! idempotent on the attestation's nonce: re-persisting an attestation already present is a
//! no-op, so a retried run never duplicates it. Persistence is first-class, not a cache: the log
//! is included verbatim in the backup artifact.

use std::fs;
use std::path::Path;

use serde_bytes::ByteBuf;
use uuid::Uuid;

use crate::cbor::{self, CanonicalError};
pub use crate::crypto::storage_attestation::StorageAttestation;
use crate::library::paths::{attestations_path, tmp_path};

/// Errors reading or persisting an asset's attestation log.
#[derive(Debug, thiserror::Error)]
pub enum AttestationStoreError {
    /// The log, or an attestation in it or offered to it, could not be decoded or encoded.
    #[error("attestation log codec: {0}")]
    Codec(#[from] CanonicalError),
    /// The attestation carries no verdict for the asset whose log it was offered to, so it is
    /// not evidence about that asset.
    #[error("the attestation does not answer for asset {0}")]
    NotAboutAsset(Uuid),
    /// A filesystem error reading or writing the log.
    #[error("attestation log I/O: {0}")]
    Io(String),
}

/// Read the raw attestation log of an asset: each entry is one attestation's canonical-CBOR
/// bytes, as received. An absent file is an empty log.
pub fn load_attestation_log(
    root: &Path,
    uuid: &Uuid,
    capture_utc: Option<i64>,
) -> Result<Vec<Vec<u8>>, AttestationStoreError> {
    let path = attestations_path(root, uuid, capture_utc);
    match fs::read(&path) {
        Ok(bytes) if bytes.is_empty() => Ok(Vec::new()),
        Ok(bytes) => {
            let entries: Vec<ByteBuf> = cbor::from_slice(&bytes)?;
            Ok(entries.into_iter().map(ByteBuf::into_vec).collect())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(AttestationStoreError::Io(e.to_string())),
    }
}

/// Read and decode the attestation log of an asset, in the order the attestations were
/// received. An absent file is an empty log.
pub fn load_attestations(
    root: &Path,
    uuid: &Uuid,
    capture_utc: Option<i64>,
) -> Result<Vec<StorageAttestation>, AttestationStoreError> {
    load_attestation_log(root, uuid, capture_utc)?
        .iter()
        .map(|bytes| StorageAttestation::from_canonical_cbor(bytes).map_err(Into::into))
        .collect()
}

/// Append a verified attestation (`cbor`, its bytes as received) to the log of `asset`.
///
/// Refused with [`AttestationStoreError::NotAboutAsset`] when the attestation carries no
/// verdict for `asset`. Idempotent on the nonce: returns `Ok(false)` without writing when an
/// attestation with the same nonce is already in the log, and `Ok(true)` when it appended.
/// The log is rewritten through a temporary sibling and a rename, so a crash mid-write leaves
/// the previous log whole.
#[tracing::instrument(skip(root, cbor), fields(%asset, bytes = cbor.len()))]
pub fn append_attestation(
    root: &Path,
    asset: &Uuid,
    capture_utc: Option<i64>,
    cbor: &[u8],
) -> Result<bool, AttestationStoreError> {
    let attestation = StorageAttestation::from_canonical_cbor(cbor)?;
    let asset_text = asset.to_string();
    if !attestation
        .core
        .verdicts
        .iter()
        .any(|verdict| verdict.asset_id == asset_text)
    {
        tracing::warn!("attestation log: refused an attestation that does not name the asset");
        return Err(AttestationStoreError::NotAboutAsset(*asset));
    }

    let mut log = load_attestation_log(root, asset, capture_utc)?;
    for existing in &log {
        let existing = StorageAttestation::from_canonical_cbor(existing)?;
        if existing.core.nonce == attestation.core.nonce {
            tracing::debug!("attestation log: already holds this nonce; nothing to append");
            return Ok(false);
        }
    }
    log.push(cbor.to_vec());

    let path = attestations_path(root, asset, capture_utc);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| AttestationStoreError::Io(e.to_string()))?;
    }
    let entries: Vec<ByteBuf> = log.into_iter().map(ByteBuf::from).collect();
    let bytes = cbor::to_canonical_vec(&entries)?;
    let tmp = tmp_path(&path);
    fs::write(&tmp, &bytes).map_err(|e| AttestationStoreError::Io(e.to_string()))?;
    fs::rename(&tmp, &path).map_err(|e| AttestationStoreError::Io(e.to_string()))?;
    tracing::debug!(
        entries = entries.len(),
        checked_at = %attestation.core.checked_at,
        "attestation log: appended"
    );
    Ok(true)
}

#[cfg(test)]
mod tests;
