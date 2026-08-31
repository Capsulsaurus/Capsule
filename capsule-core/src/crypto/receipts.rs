//! Custody receipts — the server-signed complement of the client-signed provenance chain
//! (`S-C46`; SSoT:
//! [Storage Verification — Custody Receipts](https://docs/design/import/storage-verification/)).
//!
//! The envelope proves what a client *claimed and signed*; a receipt proves what the server
//! *accepted*, over a ciphertext hash the server recomputed itself. Before dropping
//! irreplaceable local bytes a client requires a **verified** receipt for the write, so a
//! server that withholds receipts never becomes the sole holder of an only copy.
//!
//! # Why this lives in `crypto` and not in `library`
//!
//! It used to live in `library::receipts`, which is behind the `native` feature — and the
//! server that *issues* receipts is compiled without `native` on purpose, because a key-free
//! server turning it on would relink SQLite and MLS. The consequence was that the two ends of
//! one signed wire format each defined it, which is the worst shape this codebase has for a
//! signed structure: canonical CBOR sorts map keys, so byte-identity depends on the two copies
//! agreeing exactly on field names, types and the wire-presence discipline. One added field, or
//! one `Option` that serialises as a present `null`, and every receipt the server issues stops
//! verifying on every client — surfacing as "the server is withholding receipts", which is the
//! accusation the mechanism exists to make checkable.
//!
//! So the definition and its verification are here, ungated and shared, exactly as
//! [`crate::validation`] is. What stayed in `library::receipts` is the client's own
//! *persistence* — appending and reading the on-disk log — which is the half that needs
//! `std::fs` and is nobody else's business. This module must stay `std::fs`-free: `crypto` is
//! compiled for `wasm32`, and `build-check-wasm` is what notices if it stops being.
//!
//! Verification is pure offline crypto: [`HybridSignature`] and [`HybridVerifyingKey`] are
//! `capsule-core`'s own types.

use serde::{Deserialize, Serialize};

use crate::cbor::{self, CanonicalError};
use crate::crypto::hash::Hash32;
use crate::crypto::keys::{HybridSignature, HybridVerifyingKey};

/// A blob's role within an asset (closed enum; the value set is owned by the
/// storage-verification doc).
///
/// Lives beside the receipt because the receipt's `blob_role` is written from it and the two
/// have to agree on the spelling of every arm — see [`role_str`]. `library::storage_verify`
/// re-exports it, so the path clients already use keeps working.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobRole {
    /// The original ciphertext blob.
    Original,
    /// The encrypted metadata blob.
    Metadata,
    /// A derivative (thumbnail / preview / embedding) blob.
    Derivative,
    /// The provenance chain.
    Provenance,
}

/// The signed core of a [`CustodyReceipt`] — every field the server attestation key covers.
///
/// Mirrors `capsule-api-service`'s `attestation::CustodyReceiptCore` on the wire. Canonical
/// CBOR with the manifest wire-presence discipline (absent optionals encode as absent keys).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyReceiptCore {
    /// Schema version (`custody-receipt/v1`).
    pub version: String,
    /// Primitive bundle id.
    pub crypto_suite_id: u16,
    /// Album protocol pin (`YYYY-MM-DD`).
    pub protocol_version: String,
    /// The server's canonical origin — binds the receipt to one server.
    pub server_id: String,
    /// Fingerprint of the attestation key that signed; survives rotation.
    pub server_key_id: Hash32,
    /// Strictly monotonic per server.
    pub receipt_seq: u64,
    /// SHA-256 over the previous receipt in the server's log; absent only for the first receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_receipt_hash: Option<Hash32>,
    /// The upload session that produced custody.
    pub upload_id: String,
    /// The asset id.
    pub asset_id: String,
    /// `original | derivative | metadata | provenance`.
    pub blob_role: String,
    /// The server-recomputed ciphertext content address.
    pub ciphertext_hash: Hash32,
    /// Ciphertext size in bytes.
    pub size: u64,
    /// SHA-256 of the manifest envelope CBOR, when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_hash: Option<Hash32>,
    /// The user that uploaded.
    pub uploaded_by_user: String,
    /// The device that uploaded, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploaded_by_device: Option<String>,
    /// The server's trusted clock at the finalization commit (RFC 3339).
    pub received_at: String,
}

impl CustodyReceiptCore {
    /// The canonical bytes the attestation signature covers.
    #[must_use]
    pub fn signing_bytes(&self) -> Vec<u8> {
        cbor::to_canonical_vec(self).expect("custody-receipt core serializes")
    }
}

/// A custody receipt: a [`CustodyReceiptCore`] plus its hybrid attestation signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyReceipt {
    /// The signed core.
    pub core: CustodyReceiptCore,
    /// Hybrid Ed25519 + ML-DSA-65 signature under the server attestation key.
    pub server_sig: HybridSignature,
}

impl CustodyReceipt {
    /// Decode a canonical-CBOR receipt (the served / persisted form).
    pub fn from_canonical_cbor(bytes: &[u8]) -> Result<Self, CanonicalError> {
        cbor::from_slice(bytes)
    }

    /// Encode the full signed receipt as canonical CBOR.
    #[must_use]
    pub fn to_canonical_cbor(&self) -> Vec<u8> {
        cbor::to_canonical_vec(self).expect("custody receipt serializes")
    }

    /// Verify the hybrid signature under a specific attestation public key.
    #[must_use]
    pub fn verify_under(&self, key: &HybridVerifyingKey) -> bool {
        key.verify(&self.core.signing_bytes(), &self.server_sig)
    }
}

/// What the client sent to the server, checked against the receipt's server-recomputed facts:
/// a server that took custody of *different* bytes cannot answer with a matching receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiptExpectations {
    /// The ciphertext content address the client uploaded.
    pub ciphertext_hash: Hash32,
    /// The ciphertext size the client uploaded.
    pub size: u64,
    /// The blob's role on the asset.
    pub role: BlobRole,
    /// The manifest envelope hash the write committed to, when the action carries one.
    pub envelope_hash: Option<Hash32>,
}

/// Why a fetched receipt was rejected — every variant refuses release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiptRejection {
    /// The signature did not verify under any pinned attestation key.
    Signature,
    /// A field the client can check (`ciphertext_hash`/`size`/`blob_role`/`envelope_hash`) did
    /// not match what the client sent.
    FieldMismatch(&'static str),
    /// `received_at` failed the gross-drift sanity bound (evidentiary only; ordering rides the
    /// chain, not this timestamp).
    ClockDrift,
}

/// The `blob_role` string the wire uses for a [`BlobRole`].
///
/// Public because the *server* writes the field this reads: one function, so the issuer and the
/// verifier cannot disagree about how a role is spelled.
#[must_use]
pub fn role_str(role: BlobRole) -> &'static str {
    match role {
        BlobRole::Original => "original",
        BlobRole::Metadata => "metadata",
        BlobRole::Derivative => "derivative",
        BlobRole::Provenance => "provenance",
    }
}

/// A gross-drift sanity bound on `received_at`: a year each side of the client's clock. This is
/// evidentiary only — a receipt whose `received_at` is wildly implausible is suspect — never an
/// ordering signal (that rides the hash chain, mirroring the manifest `timestamp` rule).
const RECEIVED_AT_DRIFT_SECS: i64 = 366 * 24 * 3600;

/// Verify a fetched receipt for a finalized upload: the hybrid signature under one of the
/// `pinned_keys` (the published attestation-key history), then that the server's recomputed
/// facts match what the client sent, then the gross-drift bound on `received_at`.
///
/// `now_unix` is the client's current UNIX time (injected for determinism). Returns `Ok(())`
/// only when the receipt is admissible evidence for the write.
pub fn verify_receipt(
    receipt: &CustodyReceipt,
    pinned_keys: &[HybridVerifyingKey],
    expected: &ReceiptExpectations,
    now_unix: i64,
) -> Result<(), ReceiptRejection> {
    if !pinned_keys.iter().any(|k| receipt.verify_under(k)) {
        return Err(ReceiptRejection::Signature);
    }
    let core = &receipt.core;
    if core.ciphertext_hash != expected.ciphertext_hash {
        return Err(ReceiptRejection::FieldMismatch("ciphertext_hash"));
    }
    if core.size != expected.size {
        return Err(ReceiptRejection::FieldMismatch("size"));
    }
    if core.blob_role != role_str(expected.role) {
        return Err(ReceiptRejection::FieldMismatch("blob_role"));
    }
    if core.envelope_hash != expected.envelope_hash {
        return Err(ReceiptRejection::FieldMismatch("envelope_hash"));
    }
    let Ok(ts) = core.received_at.parse::<jiff::Timestamp>() else {
        return Err(ReceiptRejection::ClockDrift);
    };
    if (ts.as_second() - now_unix).abs() > RECEIVED_AT_DRIFT_SECS {
        return Err(ReceiptRejection::ClockDrift);
    }
    Ok(())
}
