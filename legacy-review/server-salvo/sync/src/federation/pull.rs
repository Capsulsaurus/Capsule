//! The federation pull path — both halves (slice `S-E2`).
//!
//! - **Serve side** ([`authorize_pull`], [`authorize_blob_fetch`]): the home server verifies a
//!   presented capability (invariant 19), enforces the per-peer budgets (invariant 21), and
//!   enforces scope by blob role, before serving a page of its sync feed / a blob. Blob bytes and
//!   the feed page ride the *existing* read primitives (`service::sync::Query::feed_page`,
//!   `GET /blob/{hash}`); federation adds only these gates.
//! - **Ingest side** ([`revalidate_pulled`]): the pulling server re-applies the full server-side
//!   invariant battery (1–18 + 25) to every manifest it pulls **before** persisting it — federation
//!   never unlocks looser rules (invariant 20). A failure is soft-failed: rejected locally, its
//!   hash remembered (see [`RejectedHashTable`](super::rejected::RejectedHashTable)).
//!
//! The manifest travels as the same opaque canonical-CBOR envelope projection the feed carries; the
//! ingest side parses it into [`PulledEnvelope`] — a peer parses the contract independently — and
//! reconstructs a [`ManifestCore`] to run `capsule_core`'s pure keyless battery over.

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::AmkVersion;
use capsule_core::crypto::provenance::action::Action;
use capsule_core::crypto::provenance::manifest::{ASSET_MANIFEST_VERSION, KeyMode, ManifestCore};
use capsule_core::validation::structural::{content_type_allowed, size_in_bounds};
use capsule_core::validation::{
    EnvelopeContext, EnvelopeReject, HandshakeReject, check_manifest_envelope,
    check_metadata_blob_envelope, protocol_gate,
};
use capsule_i18n::error_codes;
use jsonwebtoken::DecodingKey;
use serde::{Deserialize, Serialize};
use service::blob_store::is_content_hash;
use service::sync::FeedBlobManifest;
use thiserror::Error;
use uuid::Uuid;

use super::FederationReject;
use super::capability::{
    CapabilityClaims, FederationScope, VerifyContext, authorize_blob_role, verify_capability,
};
use super::compartment::{PeerRegistry, PullCost};
use super::revocation::RevocationList;

/// The server-visible mirror of the signed manifest's envelope fields, as it travels on the sync
/// feed. The ingest side parses this from the feed's opaque canonical CBOR to re-run the invariant
/// battery — the same field set the upload server projects, parsed here independently (a peer
/// parses the contract).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PulledEnvelope {
    /// The primitive-suite id (invariant 2).
    pub crypto_suite_id: u16,
    /// The wire protocol version (invariant 1 / album pin, invariant 6).
    pub protocol_version: String,
    /// The album the asset belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    /// The asset's file id.
    pub file_id: String,
    /// The AMK epoch (invariant 18).
    pub amk_version: u32,
    /// Ciphertext content hash (lowercase hex).
    pub ciphertext_hash: String,
    /// Plaintext byte length.
    pub plaintext_size: u64,
    /// STREAM plaintext chunk size.
    pub chunk_size: u32,
    /// `derived | wrapped`.
    pub key_mode: String,
    /// Content address of the asset's encrypted metadata blob (invariant 25).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_blob_hash: Option<String>,
    /// Producing user.
    pub created_by_user: String,
    /// Producing device (invariant 7).
    pub created_by_device: String,
    /// Producing client build.
    pub client_version: String,
    /// Self-asserted timestamp, RFC 3339 (invariant 8).
    pub timestamp: String,
    /// Lifecycle action (invariant 16 closed enum).
    pub action: String,
    /// Prior provenance hash (invariant 17 / stale-revival).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prior_provenance_hash: Option<String>,
    /// Retention deadline (delete only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retention_until: Option<String>,
}

impl PulledEnvelope {
    /// Serialize to the canonical CBOR the sync feed carries. (Used by tests and any producer that
    /// wants the exact wire projection.)
    pub fn to_canonical_cbor(&self) -> Result<Vec<u8>, capsule_core::cbor::CanonicalError> {
        capsule_core::cbor::to_canonical_vec(self)
    }

    /// Reconstruct the [`ManifestCore`] the keyless battery runs over. The battery reads only
    /// `crypto_suite_id`, `protocol_version`, `timestamp`, `action`, `prior_provenance_hash`,
    /// `amk_version`, `metadata_blob_hash`; opaque server identifiers are canonical placeholders.
    fn to_manifest_core(&self) -> Result<ManifestCore, PullBoundaryReject> {
        let key_mode: KeyMode = parse_kebab_enum(&self.key_mode).ok_or_else(|| {
            PullBoundaryReject::structural(16, "key_mode not a closed-enum value")
        })?;
        let action: Action = parse_kebab_enum(&self.action)
            .ok_or_else(|| PullBoundaryReject::structural(16, "action not a closed-enum value"))?;
        let prior_provenance_hash = match &self.prior_provenance_hash {
            Some(h) => Some(
                Hash32::from_hex(h)
                    .map_err(|_| PullBoundaryReject::structural(17, "prior_provenance_hash"))?,
            ),
            None => None,
        };
        let metadata_blob_hash = match &self.metadata_blob_hash {
            Some(h) => Some(
                Hash32::from_hex(h)
                    .map_err(|_| PullBoundaryReject::structural(25, "metadata_blob_hash"))?,
            ),
            None => None,
        };
        Ok(ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: self.crypto_suite_id,
            protocol_version: self.protocol_version.clone(),
            file_id: Uuid::nil(),
            album_id: Uuid::nil(),
            amk_version: AmkVersion(self.amk_version),
            ciphertext_hash: Hash32([0u8; 32]),
            plaintext_size: self.plaintext_size,
            chunk_size: self.chunk_size,
            nonce_prefix: [0u8; 7],
            key_mode,
            wrapped_file_key: None,
            metadata_blob_hash,
            created_by_user: Uuid::nil(),
            created_by_device: Uuid::nil(),
            client_version: self.client_version.clone(),
            timestamp: self.timestamp.clone(),
            action,
            prior_provenance_hash,
            upgraded_from: None,
            retention_until: self.retention_until.clone(),
        })
    }
}

fn parse_kebab_enum<T: for<'de> Deserialize<'de>>(value: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(value.to_string())).ok()
}

/// A pulled-content boundary-check failure, tagged with the server-side invariant it tripped so
/// the caller can soft-fail and the tests can assert the exact refusal point.
#[derive(Debug, Clone, Error)]
#[error("federation pull rejected at invariant {invariant}: {detail}")]
pub struct PullBoundaryReject {
    /// The server-side invariant number that refused the content.
    pub invariant: u8,
    /// The stable `error.*` code the refusal surfaces.
    pub code: &'static str,
    /// A human-facing diagnostic detail.
    pub detail: String,
}

impl PullBoundaryReject {
    fn structural(invariant: u8, detail: &str) -> Self {
        Self {
            invariant,
            code: error_codes::UPLOAD_MALFORMED_REQUEST,
            detail: detail.to_string(),
        }
    }

    fn from_envelope(reject: EnvelopeReject) -> Self {
        let (invariant, code) = match reject {
            EnvelopeReject::UnknownSuite => (2, error_codes::UPLOAD_UNKNOWN_CRYPTO_SUITE),
            EnvelopeReject::AlbumPinMismatch => (6, error_codes::UPLOAD_ENVELOPE_REJECTED),
            EnvelopeReject::DeviceAddedAfter => (7, error_codes::UPLOAD_DEVICE_NOT_AUTHORIZED),
            EnvelopeReject::TimestampUnsane => (8, error_codes::UPLOAD_TIMESTAMP_OUT_OF_RANGE),
            EnvelopeReject::StaleChain => (17, error_codes::UPLOAD_STALE_REVIVAL),
            EnvelopeReject::AmkRegressed => (18, error_codes::UPLOAD_AMK_REGRESSED),
            EnvelopeReject::MetadataBlobHashMismatch => (25, error_codes::UPLOAD_ENVELOPE_MISMATCH),
        };
        Self {
            invariant,
            code,
            detail: format!("{reject:?}"),
        }
    }
}

/// What the ingest side knows about the album and server when re-validating pulled content.
#[derive(Debug, Clone)]
pub struct PullValidationContext<'a> {
    /// The pulling server's lowest accepted protocol version.
    pub protocol_min: &'a str,
    /// The pulling server's highest accepted protocol version.
    pub protocol_max: &'a str,
    /// The album's immutable protocol pin on the pulling server.
    pub album_pin: &'a str,
    /// The producing device's directory `added_at`.
    pub device_added_at: &'a str,
    /// The pulling server's trusted clock (RFC 3339).
    pub server_clock: &'a str,
    /// Allowed timestamp drift in days.
    pub drift_days: i64,
    /// The last accepted provenance head for this asset on the pulling server.
    pub stored_chain_head: Option<Hash32>,
    /// The last accepted `amk_version` for this album on the pulling server.
    pub stored_amk_version: Option<u32>,
    /// The content types this server admits (invariant 5).
    pub allowed_content_types: &'a [&'a str],
    /// The maximum ciphertext blob size (invariant 4).
    pub max_blob_size: u64,
}

/// Re-apply the full server-side invariant battery (1–18 + 25) to one pulled feed entry before it
/// is persisted (invariant 20). Returns the parsed envelope on accept; otherwise the exact
/// boundary-check failure, which the caller soft-fails (remembers the manifest hash).
#[tracing::instrument(skip_all, fields(album_pin = %ctx.album_pin))]
pub fn revalidate_pulled(
    manifest_cbor: &[u8],
    blobs: &FeedBlobManifest,
    metadata_blob: Option<&[u8]>,
    ctx: &PullValidationContext<'_>,
) -> Result<PulledEnvelope, PullBoundaryReject> {
    // Top-level unknown fields are rejected (strict schema match); a peer parses the contract
    // independently from the same opaque canonical CBOR the feed carries.
    let envelope: PulledEnvelope = capsule_core::cbor::from_slice(manifest_cbor)
        .map_err(|e| PullBoundaryReject::structural(16, &format!("undecodable envelope: {e}")))?;

    // Invariant 1: the protocol handshake. Federation runs the same gate as every other surface.
    match protocol_gate(
        &envelope.protocol_version,
        ctx.protocol_min,
        ctx.protocol_max,
    ) {
        Ok(()) => {}
        Err(HandshakeReject::ProtocolOutOfRange) => {
            return Err(PullBoundaryReject {
                invariant: 1,
                code: error_codes::PROTOCOL_VERSION_UNSUPPORTED,
                detail: "protocol version outside accepted window".to_string(),
            });
        }
        Err(_) => {
            return Err(PullBoundaryReject {
                invariant: 1,
                code: error_codes::PROTOCOL_VERSION_UNSUPPORTED,
                detail: "protocol version malformed".to_string(),
            });
        }
    }

    // Invariants 3, 4, 5 over the carried blob references.
    check_blob_refs(blobs, ctx)?;

    // Invariants 2, 6, 7, 8, 17, 18: the keyless envelope battery.
    let core = envelope.to_manifest_core()?;
    let env_ctx = EnvelopeContext {
        album_pin: ctx.album_pin,
        device_added_at: ctx.device_added_at,
        server_clock: ctx.server_clock,
        drift_days: ctx.drift_days,
        stored_chain_head: ctx.stored_chain_head,
        stored_amk_version: ctx.stored_amk_version,
    };
    check_manifest_envelope(&core, &env_ctx).map_err(PullBoundaryReject::from_envelope)?;

    // Invariant 25: where a bundle carries a metadata blob, its content hash must match the
    // manifest's committed `metadata_blob_hash`. Federation never unlocks looser rules.
    if let Some(blob) = metadata_blob {
        check_metadata_blob_envelope(&core, blob).map_err(PullBoundaryReject::from_envelope)?;
    }

    tracing::debug!(action = %envelope.action, "pulled manifest passed invariants 1-18 + 25");
    Ok(envelope)
}

/// Invariants 3 (hash length), 4 (size), 5 (content type) over the pulled blob references.
fn check_blob_refs(
    blobs: &FeedBlobManifest,
    ctx: &PullValidationContext<'_>,
) -> Result<(), PullBoundaryReject> {
    let refs = blobs.original.iter().chain(blobs.derivatives.iter());
    for r in refs {
        if !is_content_hash(&r.ciphertext_hash) {
            return Err(PullBoundaryReject {
                invariant: 3,
                code: error_codes::UPLOAD_INVALID_HASH,
                detail: "blob content hash is not 64-char lowercase hex".to_string(),
            });
        }
        if !size_in_bounds(r.size, ctx.max_blob_size) {
            return Err(PullBoundaryReject {
                invariant: 4,
                code: error_codes::UPLOAD_INVALID_SIZE,
                detail: "blob size outside (0, max]".to_string(),
            });
        }
    }
    // Invariant 5 applies to the asset's media type — the original blob's format.
    if let Some(original) = &blobs.original
        && !original.format.is_empty()
        && !content_type_allowed(&original.format, ctx.allowed_content_types)
    {
        return Err(PullBoundaryReject {
            invariant: 5,
            code: error_codes::UPLOAD_UNSUPPORTED_CONTENT_TYPE,
            detail: format!("content type {} not allowed", original.format),
        });
    }
    Ok(())
}

/// Serve-side pull authorization (invariants 19 + 21): verify the capability against `key`, then
/// charge the per-peer budgets for `cost`. Returns the verified claims. The caller runs the
/// blocklist check (`Blocklist::ensure_server_allowed`) first and enforces scope per blob with
/// [`authorize_blob_fetch`].
#[tracing::instrument(skip(token, key, revocation, registry), fields(album = %ctx.album_id))]
#[allow(clippy::too_many_arguments)]
pub fn authorize_pull(
    token: &str,
    key: &DecodingKey,
    ctx: &VerifyContext<'_>,
    revocation: &RevocationList,
    registry: &PeerRegistry,
    registered: bool,
    cost: PullCost,
) -> Result<CapabilityClaims, FederationReject> {
    // Invariant 19: the capability verifies, is unexpired, unrevoked, correct audience.
    let claims = verify_capability(token, key, ctx, revocation).map_err(FederationReject::from)?;
    // Invariant 21: the per-peer transfer budgets are unbroken.
    registry
        .try_consume(&claims.sub, registered, cost, ctx.now)
        .map_err(FederationReject::from)?;
    Ok(claims)
}

/// Serve-side scope enforcement (invariant 19): refuse a blob whose server-visible `role` the
/// capability's scope does not cover.
pub fn authorize_blob_fetch(scope: FederationScope, role: &str) -> Result<(), FederationReject> {
    authorize_blob_role(scope, role).map_err(FederationReject::from)
}
