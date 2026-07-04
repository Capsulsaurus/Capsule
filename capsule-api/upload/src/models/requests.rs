use salvo::oapi::ToSchema;
use serde::{Deserialize, Serialize};

use crate::models::session::BlobRole;

/// Request body for creating an upload session.
///
/// The transport JSON is strict (`deny_unknown_fields`): an unknown field is a
/// client bug and is rejected with `400 error.upload.malformed_request` rather
/// than silently ignored. Plaintext metadata (filename, capture date, …) is
/// deliberately absent — it rides the encrypted metadata blob, never the wire
/// request (upload-protocol design doc, §Chunk Rules and Strictness).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateUploadRequest {
    /// Ciphertext size in bytes
    pub size: u64,
    /// Ciphertext content hash, lowercase hex; digest length fixed by `crypto_suite_id`
    pub hash: String,
    /// MIME type (closed enum per protocol version; e.g. "image/jpeg")
    pub content_type: String,
    /// Crypto suite the blob is sealed under
    pub crypto_suite_id: u16,
    /// Protocol date (`YYYY-MM-DD`) the client speaks
    pub protocol_version: String,
    /// The blob's role in its asset bundle
    pub blob_role: BlobRole,
    /// The unencrypted manifest fields the server validates (invariants 1–8, 15, 25).
    /// The top-level `crypto_suite_id`/`protocol_version`/`album_id` MUST agree with
    /// the envelope's — a contradiction is `400 error.upload.envelope_mismatch` (S-C1).
    pub manifest_envelope: ManifestEnvelope,
    /// Optional album to add asset to
    pub album_id: Option<String>,
    /// Optional owner ID (defaults to authenticated user)
    pub owner_id: Option<String>,
    /// Album-upgrade intent id (required only during an album upgrade ceremony)
    pub intent_id: Option<String>,
}

/// The server-visible mirror of the signed manifest's envelope fields, as declared
/// at `POST /upload` (owned by the provenance design doc; validated per the
/// threat-model invariants). Strict like the rest of the transport JSON — the
/// Postel unknown-key tolerance applies to the signed CBOR interiors, not to this
/// JSON projection.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ManifestEnvelope {
    pub crypto_suite_id: u16,
    pub protocol_version: String,
    pub album_id: Option<String>,
    /// The asset id this blob belongs to (UUIDv7, same id across sidecar/manifest)
    pub file_id: String,
    pub amk_version: u32,
    /// Ciphertext content hash, lowercase hex — must equal the top-level `hash`
    pub ciphertext_hash: String,
    pub plaintext_size: u64,
    /// STREAM plaintext chunk size (owned by the encryption doc)
    pub chunk_size: u32,
    /// `derived | wrapped` (closed enum owned by the provenance doc)
    pub key_mode: String,
    pub metadata_blob_hash: Option<String>,
    pub created_by_user: String,
    pub created_by_device: String,
    pub client_version: String,
    /// RFC3339; gross-drift sanity checked (invariant 8)
    pub timestamp: String,
    /// Lifecycle action (closed enum; `create` for a fresh bundle)
    pub action: String,
    pub prior_provenance_hash: Option<String>,
    pub retention_until: Option<String>,
}
