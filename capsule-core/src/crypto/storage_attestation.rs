//! Signed storage attestations — the durability verdict in a form a client can keep (SSoT:
//! [Transfer — Durability: Signed Storage Attestation](https://docs/design/transfer/durability/#signed-storage-attestation)).
//!
//! `POST /v1/storage/verify` answers whether the server holds what a client is about to stop
//! holding. Unsigned, that answer is a claim the server can later deny. Signed under the server
//! attestation key — the key that signs [custody receipts](crate::crypto::receipts) — and bound to
//! a nonce the client chose, it is evidence: an honest server's self-reported loss and a periodic
//! audit both become statements the server cannot take back.
//!
//! # Why this lives in `crypto`
//!
//! For the reason [`crate::crypto::receipts`] does: the server that signs an attestation links
//! `capsule-core` without `native`, and a signed structure defined at both ends stops verifying
//! the day either copy gains a field. One definition, ungated and shared. It must stay
//! `std::fs`-free; persisting an attestation is the client library's business.
//!
//! # What the nonce is for
//!
//! A signed verdict with no client contribution can be replayed: a server that once held the
//! bytes could answer a later audit with the old signature. The nonce is 32 bytes the client
//! draws fresh per request and checks on the way back, so an attestation proves the server looked
//! *after* the client asked. `checked_at` is the server's clock and evidentiary only; freshness
//! rides the nonce.
//!
//! # What a client checks
//!
//! [`verify_storage_attestation`] accepts an attestation only when the signature verifies under
//! a pinned attestation key, the version is this one, the nonce is the one the client sent, and
//! the verdicts answer exactly the question the client asked — the same assets in the same order,
//! each with the same declared hashes in the same order. An attestation that answers a different
//! question is not evidence about this one, however validly it is signed.

use serde::{Deserialize, Serialize};

use crate::cbor::{self, CanonicalError};
use crate::crypto::hash::Hash32;
use crate::crypto::keys::{HybridSignature, HybridVerifyingKey};

/// The schema version every storage attestation carries.
///
/// Also the domain separator from a custody receipt, which the same key signs: a receipt core
/// carries `custody-receipt/v1` and a different field set, so neither decodes as the other.
pub const STORAGE_ATTESTATION_VERSION: &str = "storage-attestation/v1";

/// The length of the client's nonce, in bytes.
pub const ATTESTATION_NONCE_LEN: usize = 32;

/// The client's per-request nonce.
pub type AttestationNonce = [u8; ATTESTATION_NONCE_LEN];

/// One declared blob's verdict, as signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestedBlob {
    /// The content address the client declared.
    pub hash: Hash32,
    /// The role the asset holds it under, or `unknown` when the asset does not hold it.
    pub role: String,
    /// The bytes are present at that address.
    pub stored: bool,
    /// A live asset of the caller's references the address.
    pub indexed: bool,
    /// Nothing is withholding it.
    pub retrievable: bool,
    /// What a deep scan found — `intact`, `corrupt` or `rate_limited` — and absent when none ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep: Option<String>,
}

/// One asset's verdict, as signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestedAsset {
    /// The asset the client asked about.
    pub asset_id: String,
    /// Every declared blob is stored ∧ indexed ∧ retrievable.
    pub durable: bool,
    /// One entry per declared hash, in declaration order.
    pub blobs: Vec<AttestedBlob>,
}

/// The signed core of a [`StorageAttestation`] — every field the attestation key covers.
///
/// Canonical CBOR with the manifest wire-presence discipline: an absent optional is an absent
/// key, never a present `null`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageAttestationCore {
    /// Schema version ([`STORAGE_ATTESTATION_VERSION`]).
    pub version: String,
    /// The server's canonical origin — binds the attestation to one server.
    pub server_id: String,
    /// Fingerprint of the attestation key that signed; survives rotation.
    pub server_key_id: Hash32,
    /// The account the verdicts were computed for. Verdicts are owner-scoped, so the same
    /// question asked by another account has a different answer.
    pub account: String,
    /// The client's nonce, echoed.
    #[serde(with = "serde_bytes")]
    pub nonce: AttestationNonce,
    /// The server's trusted clock when it looked (RFC 3339). Evidentiary only.
    pub checked_at: String,
    /// One verdict per asset the client asked about, in request order.
    pub verdicts: Vec<AttestedAsset>,
}

impl StorageAttestationCore {
    /// The canonical bytes the attestation signature covers.
    #[must_use]
    pub fn signing_bytes(&self) -> Vec<u8> {
        cbor::to_canonical_vec(self).expect("storage-attestation core serializes")
    }
}

/// A signed storage attestation: a [`StorageAttestationCore`] plus its hybrid signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageAttestation {
    /// The signed core.
    pub core: StorageAttestationCore,
    /// Hybrid Ed25519 + ML-DSA-65 signature under the server attestation key.
    pub server_sig: HybridSignature,
}

impl StorageAttestation {
    /// Decode a canonical-CBOR attestation (the served and persisted form).
    pub fn from_canonical_cbor(bytes: &[u8]) -> Result<Self, CanonicalError> {
        cbor::from_slice(bytes)
    }

    /// Encode the full signed attestation as canonical CBOR.
    #[must_use]
    pub fn to_canonical_cbor(&self) -> Vec<u8> {
        cbor::to_canonical_vec(self).expect("storage attestation serializes")
    }

    /// Verify the hybrid signature under a specific attestation public key.
    #[must_use]
    pub fn verify_under(&self, key: &HybridVerifyingKey) -> bool {
        key.verify(&self.core.signing_bytes(), &self.server_sig)
    }
}

/// One asset the client asked about, with the hashes it declared — what an attestation must
/// answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredAsset {
    /// The asset id, as the client sent it.
    pub asset_id: String,
    /// The declared content addresses, in the order the client sent them.
    pub blob_hashes: Vec<Hash32>,
}

/// Why an attestation was rejected. Every variant means it is not evidence for this request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttestationRejection {
    /// The signature did not verify under any pinned attestation key.
    #[error("the attestation signature does not verify under a pinned key")]
    Signature,
    /// The attestation carries a schema version this client does not know.
    #[error("unknown attestation version")]
    Version,
    /// The nonce is not the one this request sent: a replayed or misdirected attestation.
    #[error("the attestation does not carry this request's nonce")]
    Nonce,
    /// The verdicts answer a different question from the one asked.
    #[error("the attestation does not answer the request: {0}")]
    Mismatch(&'static str),
}

/// Verify a storage attestation for one request.
///
/// Checks, in order: the signature under one of `pinned_keys` (the published attestation-key
/// history), the schema version, the nonce, and that the verdicts cover exactly `declared` — the
/// same assets in order, each with the same hashes in order. Returns `Ok(())` only when the
/// attestation is admissible evidence for this request.
pub fn verify_storage_attestation(
    attestation: &StorageAttestation,
    pinned_keys: &[HybridVerifyingKey],
    nonce: &AttestationNonce,
    declared: &[DeclaredAsset],
) -> Result<(), AttestationRejection> {
    if !pinned_keys.iter().any(|key| attestation.verify_under(key)) {
        return Err(AttestationRejection::Signature);
    }
    let core = &attestation.core;
    if core.version != STORAGE_ATTESTATION_VERSION {
        return Err(AttestationRejection::Version);
    }
    if &core.nonce != nonce {
        return Err(AttestationRejection::Nonce);
    }
    if core.verdicts.len() != declared.len() {
        return Err(AttestationRejection::Mismatch("asset count"));
    }
    for (verdict, asked) in core.verdicts.iter().zip(declared) {
        if verdict.asset_id != asked.asset_id {
            return Err(AttestationRejection::Mismatch("asset_id"));
        }
        if verdict.blobs.len() != asked.blob_hashes.len()
            || verdict
                .blobs
                .iter()
                .zip(&asked.blob_hashes)
                .any(|(blob, hash)| &blob.hash != hash)
        {
            return Err(AttestationRejection::Mismatch("blob_hashes"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
