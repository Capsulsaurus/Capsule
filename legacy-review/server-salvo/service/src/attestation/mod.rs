//! Server-signed storage evidence (slice `S-C15`): the [`CustodyReceipt`], the signed
//! [`StorageAttestation`], and the long-lived **attestation keypair** that signs them.
//!
//! These are the two objects that make custody and loss *provable* rather than merely
//! checkable (SSoT: [Storage Verification — Custody Receipts / Signed Storage Attestation /
//! Proof of Loss]). A receipt is the server-signed complement of the client-signed
//! provenance chain: the envelope proves what a client *claimed and signed*; the receipt
//! proves what the server *accepted*, over a ciphertext hash it recomputed itself, inside
//! the same finalization transaction that flips `uploaded` (invariant 33 — both or neither).
//!
//! The attestation key is a long-lived **hybrid Ed25519 + ML-DSA-65** keypair, distinct
//! from the classical operational key (a receipt is evidence for the life of the asset, so
//! it sits outside the operational-signature carve-out). The keyring keeps an **append-only
//! key history** so a receipt signed years ago still verifies; `server_key_id` selects the
//! verification key ([Federation — Server Identity and Key Rotation]).
//!
//! The signing/verification here is pure and key-only; the persistence half (the append-only
//! `receipt_seq` chain in Postgres + the content-addressed mirror) lives in [`mutation`] and
//! [`query`].
//!
//! [Storage Verification — Custody Receipts / Signed Storage Attestation / Proof of Loss]:
//!     ../../../../../capsule-docs/src/content/docs/design/import/storage-verification.md
//! [Federation — Server Identity and Key Rotation]:
//!     ../../../../../capsule-docs/src/content/docs/design/federation.md

pub mod mutation;
pub mod query;

use capsule_core::cbor;
use capsule_core::crypto::CRYPTO_SUITE_ID;
use capsule_core::crypto::hash::{Hash32, hash_bytes};
use capsule_core::crypto::keys::{HybridSignature, HybridSigningKey, HybridVerifyingKey};
use jiff::Timestamp;
pub use mutation::{Mutation, ReceiptInput};
pub use query::Query;
use serde::{Deserialize, Serialize};
use tracing::{instrument, warn};

/// Schema version string carried on every custody receipt.
pub const CUSTODY_RECEIPT_VERSION: &str = "custody-receipt/v1";
/// Schema version string carried on every signed storage attestation.
pub const STORAGE_ATTESTATION_VERSION: &str = "storage-attestation/v1";

// ─── Signed evidence types ────────────────────────────────────────────────────

/// The signed core of a [`CustodyReceipt`] — every field the server attestation key covers.
///
/// Canonical CBOR with the same wire-presence discipline as manifests (absent optionals
/// encode as absent keys), and deliberately server-visible: it carries ciphertext hashes
/// only, never plaintext.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustodyReceiptCore {
    /// Schema version (`custody-receipt/v1`).
    pub version: String,
    /// Primitive bundle id.
    pub crypto_suite_id: u16,
    /// Album protocol pin (`YYYY-MM-DD`).
    pub protocol_version: String,
    /// This server's canonical origin — binds the receipt to one server.
    pub server_id: String,
    /// Fingerprint of the attestation key that signed; survives rotation.
    pub server_key_id: Hash32,
    /// Strictly monotonic per server.
    pub receipt_seq: u64,
    /// SHA-256 over the previous receipt in the server's log; absent only for the first
    /// receipt — the provenance chain's append-only discipline, applied to the server's log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_receipt_hash: Option<Hash32>,
    /// The upload session that produced custody.
    pub upload_id: String,
    /// The asset id.
    pub asset_id: String,
    /// `original | derivative | metadata | provenance`.
    pub blob_role: String,
    /// RECOMPUTED by the server at finalization — never echoed from the client.
    pub ciphertext_hash: Hash32,
    /// Ciphertext size in bytes.
    pub size: u64,
    /// SHA-256 of the manifest envelope CBOR — binds the receipt to the asset's
    /// provenance-chain position.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub envelope_hash: Option<Hash32>,
    /// The user that uploaded.
    pub uploaded_by_user: String,
    /// The device that uploaded, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uploaded_by_device: Option<String>,
    /// Server's trusted clock at the finalization commit (RFC 3339).
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
    /// The receipt's own content hash — the SHA-256 the *next* receipt chains from. Computed
    /// over the full signed receipt's canonical CBOR (core + signature), so the chain binds
    /// the signature too.
    #[must_use]
    pub fn content_hash(&self) -> Hash32 {
        hash_bytes(&cbor::to_canonical_vec(self).expect("custody receipt serializes"))
    }

    /// Verify the signature under a specific attestation public key.
    #[must_use]
    pub fn verify_under(&self, key: &HybridVerifyingKey) -> bool {
        key.verify(&self.core.signing_bytes(), &self.server_sig)
    }

    /// Encode the full signed receipt as canonical CBOR (the persisted/served form).
    #[must_use]
    pub fn to_canonical_cbor(&self) -> Vec<u8> {
        cbor::to_canonical_vec(self).expect("custody receipt serializes")
    }

    /// Decode a canonical-CBOR receipt.
    pub fn from_canonical_cbor(bytes: &[u8]) -> Result<Self, cbor::CanonicalError> {
        cbor::from_slice(bytes)
    }
}

/// One blob's verdict, as the signed attestation carries it (mirrors the unsigned
/// `/storage/verify` per-blob verdict, key-free).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestedBlob {
    /// The declared content address (hex).
    pub hash: String,
    /// `original | metadata | derivative | provenance | unknown`.
    pub role: String,
    /// Present in the blob store at its content address.
    pub stored: bool,
    /// Referenced by a committed, `uploaded = true` row.
    pub indexed: bool,
    /// Refcount > 0, not `collectable_since`, not quarantined.
    pub retrievable: bool,
}

/// One asset's durability verdict — the unchanged `/storage/verify` verdict, signed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttestedVerdict {
    /// The asset id.
    pub asset_id: String,
    /// Every required blob stored ∧ indexed ∧ retrievable.
    pub durable: bool,
    /// Per-blob detail.
    pub blobs: Vec<AttestedBlob>,
    /// The server's trusted clock at verification (RFC 3339).
    pub checked_at: String,
}

/// The signed core of a [`StorageAttestation`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageAttestationCore {
    /// Schema version (`storage-attestation/v1`).
    pub version: String,
    /// Primitive bundle id.
    pub crypto_suite_id: u16,
    /// This server's canonical origin.
    pub server_id: String,
    /// Fingerprint of the attestation key that signed.
    pub server_key_id: Hash32,
    /// Client-supplied freshness challenge, echoed verbatim — a stale `durable = true`
    /// cannot be replayed as current. Absent when the client sent none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce: Option<serde_bytes::ByteBuf>,
    /// The unchanged verdict, including `checked_at`.
    pub verdict: AttestedVerdict,
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
    /// Verify the signature under a specific attestation public key.
    #[must_use]
    pub fn verify_under(&self, key: &HybridVerifyingKey) -> bool {
        key.verify(&self.core.signing_bytes(), &self.server_sig)
    }
}

// ─── The attestation keyring ──────────────────────────────────────────────────

/// One published attestation key in the append-only history.
#[derive(Debug, Clone)]
pub struct PublishedKey {
    /// The key fingerprint (`server_key_id`): SHA-256 of the hybrid public key bytes.
    pub key_id: Hash32,
    /// The hybrid public verifying key.
    pub public: HybridVerifyingKey,
    /// When the key became active (informational; ordering rides the receipt chain).
    pub active_from: Timestamp,
    /// When the key was retired, or `None` for the current key.
    pub active_to: Option<Timestamp>,
}

/// The server's attestation keyring: the active signing key plus the append-only history of
/// every key it has ever used, so old receipts verify forever.
///
/// Cheap to clone (the ML-DSA/Ed25519 seeds are small); shared behind an `Arc` in server
/// state.
#[derive(Clone)]
pub struct AttestationKeyring {
    server_id: String,
    signing: HybridSigningKey,
    active_key_id: Hash32,
    /// Append-only; includes the active key as its last entry.
    history: Vec<PublishedKey>,
}

impl std::fmt::Debug for AttestationKeyring {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttestationKeyring")
            .field("server_id", &self.server_id)
            .field("active_key_id", &self.active_key_id)
            .field("history_len", &self.history.len())
            .finish_non_exhaustive()
    }
}

/// The fingerprint (`server_key_id`) of a hybrid public key: SHA-256 of its byte encoding.
#[must_use]
pub fn key_fingerprint(public: &HybridVerifyingKey) -> Hash32 {
    hash_bytes(&public.to_bytes())
}

impl AttestationKeyring {
    /// Build a keyring from the active signing key's 64-byte seed (Ed25519 secret ‖ ML-DSA ξ)
    /// and the append-only list of previously retired public keys.
    #[must_use]
    pub fn new(server_id: String, active_seed: &[u8; 64], mut retired: Vec<PublishedKey>) -> Self {
        let signing = HybridSigningKey::from_seed64(active_seed);
        let public = signing.verifying_key();
        let active_key_id = key_fingerprint(&public);
        let active = PublishedKey {
            key_id: active_key_id,
            public,
            active_from: Timestamp::UNIX_EPOCH,
            active_to: None,
        };
        // The active key is the newest entry; retired keys precede it.
        retired.push(active);
        Self {
            server_id,
            signing,
            active_key_id,
            history: retired,
        }
    }

    /// This server's canonical origin.
    #[must_use]
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    /// The active key's fingerprint (`server_key_id`).
    #[must_use]
    pub fn active_key_id(&self) -> Hash32 {
        self.active_key_id
    }

    /// The append-only key history (retired keys first, active last).
    #[must_use]
    pub fn history(&self) -> &[PublishedKey] {
        &self.history
    }

    /// Resolve a `server_key_id` to its verifying key from the history.
    #[must_use]
    pub fn resolve(&self, key_id: &Hash32) -> Option<&HybridVerifyingKey> {
        self.history
            .iter()
            .find(|k| &k.key_id == key_id)
            .map(|k| &k.public)
    }

    /// Sign a fully populated receipt core with the active attestation key.
    #[instrument(skip_all, fields(receipt_seq = core.receipt_seq, asset = %core.asset_id))]
    #[must_use]
    pub fn sign_receipt(&self, core: CustodyReceiptCore) -> CustodyReceipt {
        let server_sig = self.signing.sign(&core.signing_bytes());
        CustodyReceipt { core, server_sig }
    }

    /// Sign a storage-attestation core with the active attestation key.
    #[instrument(skip_all)]
    #[must_use]
    pub fn sign_attestation(&self, core: StorageAttestationCore) -> StorageAttestation {
        let server_sig = self.signing.sign(&core.signing_bytes());
        StorageAttestation { core, server_sig }
    }

    /// Build a receipt core stamped with this server's identity and active `server_key_id`.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn new_receipt_core(
        &self,
        protocol_version: String,
        receipt_seq: u64,
        prior_receipt_hash: Option<Hash32>,
        upload_id: String,
        asset_id: String,
        blob_role: String,
        ciphertext_hash: Hash32,
        size: u64,
        envelope_hash: Option<Hash32>,
        uploaded_by_user: String,
        uploaded_by_device: Option<String>,
        received_at: String,
    ) -> CustodyReceiptCore {
        CustodyReceiptCore {
            version: CUSTODY_RECEIPT_VERSION.to_string(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version,
            server_id: self.server_id.clone(),
            server_key_id: self.active_key_id,
            receipt_seq,
            prior_receipt_hash,
            upload_id,
            asset_id,
            blob_role,
            ciphertext_hash,
            size,
            envelope_hash,
            uploaded_by_user,
            uploaded_by_device,
            received_at,
        }
    }

    /// Sign the given verdict with the active key, echoing the client `nonce` verbatim
    /// (invariant 34 — the signature covers the same verdict field the unsigned path
    /// returned, plus the echoed nonce).
    #[must_use]
    pub fn attest_verdict(
        &self,
        verdict: AttestedVerdict,
        nonce: Option<Vec<u8>>,
    ) -> StorageAttestation {
        let core = StorageAttestationCore {
            version: STORAGE_ATTESTATION_VERSION.to_string(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            server_id: self.server_id.clone(),
            server_key_id: self.active_key_id,
            nonce: nonce.map(serde_bytes::ByteBuf::from),
            verdict,
        };
        self.sign_attestation(core)
    }

    /// Verify a receipt against this keyring: the `server_id` must be ours, the `server_key_id`
    /// must resolve in our history, and the hybrid signature must verify. Rejects a receipt
    /// issued by a different server (cross-server replay) on the identity binding.
    #[must_use]
    pub fn verify_receipt(&self, receipt: &CustodyReceipt) -> bool {
        if receipt.core.server_id != self.server_id {
            warn!(
                receipt_server = %receipt.core.server_id,
                our_server = %self.server_id,
                "custody receipt rejected: cross-server identity mismatch"
            );
            return false;
        }
        if let Some(key) = self.resolve(&receipt.core.server_key_id) {
            receipt.verify_under(key)
        } else {
            warn!("custody receipt rejected: unknown server_key_id (not in key history)");
            false
        }
    }

    /// Verify a signed storage attestation against this keyring (same identity binding).
    #[must_use]
    pub fn verify_attestation(&self, att: &StorageAttestation) -> bool {
        if att.core.server_id != self.server_id {
            return false;
        }
        self.resolve(&att.core.server_key_id)
            .is_some_and(|key| att.verify_under(key))
    }

    /// The `.well-known` publication document for this server's attestation keys.
    #[must_use]
    pub fn well_known(&self) -> WellKnownAttestation {
        WellKnownAttestation {
            server_id: self.server_id.clone(),
            keys: self
                .history
                .iter()
                .map(|k| PublishedKeyWire {
                    key_id: k.key_id.to_hex(),
                    public: data_encoding::BASE64.encode(&k.public.to_bytes()),
                    algorithm: "hybrid-ed25519-mldsa65".to_string(),
                    active_from: k.active_from.to_string(),
                    active_to: k.active_to.map(|t| t.to_string()),
                })
                .collect(),
        }
    }
}

// ─── Well-known publication wire types ────────────────────────────────────────

/// One published key in the `.well-known` attestation document.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PublishedKeyWire {
    /// The `server_key_id` fingerprint (lowercase hex).
    pub key_id: String,
    /// The hybrid public key (base64: Ed25519 ‖ ML-DSA-65).
    pub public: String,
    /// The signature algorithm identifier.
    pub algorithm: String,
    /// When the key became active (RFC 3339).
    pub active_from: String,
    /// When the key was retired (RFC 3339), or absent for the current key.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_to: Option<String>,
}

/// The attestation-key publication document, served at the server's well-known path with an
/// **append-only key history** so a receipt signed years ago still verifies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WellKnownAttestation {
    /// This server's canonical origin.
    pub server_id: String,
    /// Every attestation key this server has used, newest last; retired entries are never
    /// dropped.
    pub keys: Vec<PublishedKeyWire>,
}

impl WellKnownAttestation {
    /// Reconstruct a [`PublishedKey`] history from a published document (a verifier that
    /// pinned this document earlier resolves `server_key_id` against it).
    pub fn to_history(&self) -> Result<Vec<PublishedKey>, HistoryDecodeError> {
        self.keys
            .iter()
            .map(|k| {
                let bytes = data_encoding::BASE64
                    .decode(k.public.as_bytes())
                    .map_err(|_| HistoryDecodeError::Public)?;
                let public = HybridVerifyingKey::from_bytes(&bytes)
                    .map_err(|_| HistoryDecodeError::Public)?;
                let active_from = k
                    .active_from
                    .parse::<Timestamp>()
                    .map_err(|_| HistoryDecodeError::Time)?;
                let active_to = match &k.active_to {
                    Some(s) => Some(
                        s.parse::<Timestamp>()
                            .map_err(|_| HistoryDecodeError::Time)?,
                    ),
                    None => None,
                };
                Ok(PublishedKey {
                    key_id: key_fingerprint(&public),
                    public,
                    active_from,
                    active_to,
                })
            })
            .collect()
    }
}

/// A retired-key history document failed to decode.
#[derive(Debug, thiserror::Error)]
pub enum HistoryDecodeError {
    /// A public key was not valid base64 / not a valid hybrid key.
    #[error("invalid attestation public key in history")]
    Public,
    /// An `active_from` / `active_to` timestamp did not parse.
    #[error("invalid timestamp in attestation key history")]
    Time,
}

/// Parse the operator-supplied append-only key history (base64 of the well-known JSON's
/// `keys` array) into retired [`PublishedKey`]s. A malformed history is logged and treated
/// as empty rather than failing server startup — the active key alone still verifies every
/// receipt it signs.
#[must_use]
pub fn parse_key_history(history_json_b64: Option<&str>) -> Vec<PublishedKey> {
    let Some(b64) = history_json_b64 else {
        return Vec::new();
    };
    let decode = || -> Result<Vec<PublishedKey>, String> {
        let json = data_encoding::BASE64
            .decode(b64.as_bytes())
            .map_err(|e| format!("base64: {e}"))?;
        let keys: Vec<PublishedKeyWire> =
            serde_json::from_slice(&json).map_err(|e| format!("json: {e}"))?;
        WellKnownAttestation {
            server_id: String::new(),
            keys,
        }
        .to_history()
        .map_err(|e| e.to_string())
    };
    match decode() {
        Ok(keys) => keys,
        Err(e) => {
            warn!("ignoring malformed ATTESTATION_KEY_HISTORY ({e}); using active key only");
            Vec::new()
        }
    }
}

// ─── Proof-of-loss composition ────────────────────────────────────────────────

/// A server's legitimate rebuttal to a loss claim: the asset's own provenance chain carried a
/// device-signed `delete` whose retention window has elapsed, proving the bytes were
/// *dis*entrusted, not lost. A minimal binding (asset + elapsed retention); the full
/// manifest-chain check against the receipt's `envelope_hash` position is `verify_asset`'s
/// job (client slice S-D4).
#[derive(Debug, Clone)]
pub struct DeleteRebuttal {
    /// The asset the delete tombstoned.
    pub asset_id: String,
    /// The `retention_until` the delete manifest carried.
    pub retention_until: Timestamp,
}

/// How a verifier classifies an observed non-holding of receipted bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NonHolding {
    /// A provable loss: the server took custody and no longer holds the bytes, with no
    /// authorized purge to explain it.
    Loss,
    /// An authorized purge: a device-signed `delete` with an elapsed retention window
    /// disentrusted the bytes — not negligence.
    AuthorizedPurge,
    /// The claim does not compose into a proof (a signature failed to verify, the identity
    /// binding did not match, or the attestation still reports the bytes as held).
    Unproven,
}

/// Compose a [`CustodyReceipt`] (acceptance) with a signed [`StorageAttestation`] reporting
/// non-holding into a transferable classification, with the burden of proof placed where it
/// can be discharged (SSoT: Storage Verification — Proof of Loss).
///
/// - Both objects must verify under `keyring` and share its `server_id`/`server_key_id`
///   binding — this is what rejects a cross-server replay.
/// - The attestation must actually report the receipt's `ciphertext_hash` as non-`stored`.
/// - A matching [`DeleteRebuttal`] with an elapsed retention window reclassifies the
///   non-holding as an authorized purge.
#[must_use]
pub fn classify_non_holding(
    receipt: &CustodyReceipt,
    attestation: &StorageAttestation,
    rebuttal: Option<&DeleteRebuttal>,
    now: Timestamp,
    keyring: &AttestationKeyring,
) -> NonHolding {
    if !keyring.verify_receipt(receipt) || !keyring.verify_attestation(attestation) {
        return NonHolding::Unproven;
    }
    // The attestation must speak to the receipted hash and report it non-held.
    let target = receipt.core.ciphertext_hash.to_hex();
    let non_held = attestation
        .core
        .verdict
        .blobs
        .iter()
        .find(|b| b.hash == target)
        .is_some_and(|b| !b.stored || !b.retrievable);
    if !non_held {
        return NonHolding::Unproven;
    }
    if let Some(r) = rebuttal
        && r.asset_id == receipt.core.asset_id
        && r.retention_until <= now
    {
        return NonHolding::AuthorizedPurge;
    }
    NonHolding::Loss
}

#[cfg(test)]
mod tests;
