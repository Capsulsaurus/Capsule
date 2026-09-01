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

/// Request body for `POST /albums` — album provisioning (slice `S-C25`).
///
/// **One field, on purpose.** The album id is derived from the account master key, so it is
/// the only thing the client can tell the server that the server does not already know.
/// Strict (`deny_unknown_fields`): a `name` or `description` field is a `400`, never a
/// silently-ignored extra — the plaintext `albums.name`/`albums.description` columns predate
/// the key-free model and the server is not entitled to album titles, which live in the
/// encrypted sidecar (slice `S-C26` retires the columns).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProvisionAlbumRequest {
    /// The caller's derived album id, as a canonical lowercase hyphenated UUID.
    pub album_id: String,
}

/// Request body for a generic lifecycle write, `POST /albums/{album_id}/ops` (slice `S-C16`).
///
/// The signed manifest bundle: the opaque manifest as its [`ManifestEnvelope`] projection
/// (re-serialized to canonical CBOR server-side, never re-modeled on the wire — the same
/// projection the sync feed carries) plus, when the action carries one, the encrypted
/// metadata blob as standard base64. Strict (`deny_unknown_fields`): an unknown field is a
/// client bug (`400 error.upload.malformed_request`), never silently ignored.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct OpRequest {
    /// The unencrypted manifest envelope fields the server validates (invariants 16–18, 25).
    /// Its `album_id` MUST equal the `{album_id}` path segment and its `action` MUST be a
    /// non-upload lifecycle action — a contradiction is `400 error.upload.envelope_mismatch`.
    pub manifest_envelope: ManifestEnvelope,
    /// The encrypted metadata blob (standard base64), present exactly when the action binds a
    /// metadata blob (`metadata-update`). Its content hash must equal the manifest's committed
    /// `metadata_blob_hash` (invariant 25).
    pub metadata_blob: Option<String>,
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
