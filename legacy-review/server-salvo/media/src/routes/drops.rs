//! Web-upload guest drops — the drop store, staging inbox, and atomic adoption (slice `S-C5`;
//! SSoT: the [Web Upload design doc](https://docs/design/web-upload/)).
//!
//! Guest-facing (link-capability auth, no account): `POST /u/{opaque-id}/drop` opens a drop
//! session (per-link caps + owner quota checked here; invariants 26–31), and `PATCH
//! /u/{opaque-id}/drop/{id}` appends a chunk reusing the S-C1 chunk mechanics verbatim. On
//! completion the sealed ciphertext is finalized into the owner's drop inbox — never an album
//! asset. Owner-facing (session auth): `GET /drops` lists the inbox, `POST /drops/{id}/adopt`
//! runs the single-transaction inbox→album promotion against the adopter's signed `create`
//! manifest (invariant 32), and `DELETE /drops/{id}` discards. A not-found, revoked, or
//! expired link returns an indistinguishable `404` — never `410`.

use auth::models::errors::ApiError;
use auth::utils::headers::validate_user_from_headers;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::keys::DEK_CIPHERTEXT_LEN;
use capsule_core::crypto::provenance::action::Action;
use capsule_core::crypto::provenance::manifest::AssetManifest;
use capsule_core::drop::PassphraseVerifier;
use capsule_core::utils::hash::{get_file_hash, hash_bytes};
use capsule_core::validation::protocol::check_suite;
use capsule_core::validation::{
    EnvelopeContext, EnvelopeReject, HandshakeReject, check_manifest_envelope,
    metadata_blob_hash_matches, protocol_gate,
};
use capsule_i18n::error_codes;
use jiff::{SignedDuration, Timestamp};
use salvo::oapi::extract::PathParam;
use salvo::prelude::*;
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};
use service::drop::{
    AdoptInput, AdoptOutcome, DropError, Mutation as DropMutation, Query as DropQuery, StageInput,
};
use upload::transport::{BlobRole, UploadError, UploadSession, UploadSessionStatus};

use crate::drop_state::DropState;

/// The KEM-DEM ciphertext overhead beyond the X-Wing ciphertext: `seal_blob(ss, K)` =
/// suite(2) ‖ nonce(12) ‖ K(32) ‖ tag(16) (SSoT: `capsule_core::drop::seal_drop`).
const KEM_DEM_OVERHEAD: usize = 2 + 12 + 32 + 16;
/// The exact `kem_ct` byte length a well-formed `DropDescriptor` carries (invariant 30).
const EXPECTED_KEM_CT_LEN: usize = DEK_CIPHERTEXT_LEN + KEM_DEM_OVERHEAD;
/// Protocol-surface maximum chunk size (mirrors the upload protocol's 16 MiB ceiling).
const MAX_CHUNK_SIZE: u64 = 16 * 1024 * 1024;
/// The RFC3339 account-creation floor for a user whose row is missing (keeps invariant 7 from
/// spuriously failing; the JWT would be invalid anyway).
const EPOCH_RFC3339: &str = "1970-01-01T00:00:00Z";

// ─────────────────────────────── Request / response bodies ───────────────────────────────

/// The unsigned guest descriptor uploaded beside the sealed ciphertext (the canonical shape is
/// `capsule_core::drop::DropDescriptor`; carried here as its JSON projection). Strict
/// (`deny_unknown_fields`): a drop that names an album or supplies a manifest/provenance field
/// is rejected (invariant 30).
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct DropDescriptorBody {
    /// Closed enum for the link's pinned protocol version.
    pub content_type: String,
    /// Total plaintext byte length.
    pub plaintext_size: u64,
    /// STREAM plaintext chunk size.
    pub chunk_size: u32,
    /// STREAM nonce prefix (7 bytes, hex → 14 chars).
    pub nonce_prefix: String,
    /// Content address (hex) of the STREAM ciphertext.
    pub ciphertext_hash: String,
    /// `K` encapsulated to the link's Drop Key (base64); length fixed by the suite.
    pub kem_ct: String,
    /// Guest-supplied, unverified; advisory only.
    pub suggested_filename: Option<String>,
}

/// The drop-session creation request: the descriptor, the ciphertext byte length, and (when the
/// link is passphrase-gated) the Argon2id proof. The passphrase itself is never transmitted.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateDropRequest {
    /// Total ciphertext (STREAM) byte length (invariant 28).
    pub size: u64,
    /// The Argon2id-derived proof (lowercase hex of 32 bytes) for a passphrase-gated link.
    pub passphrase_proof: Option<String>,
    /// The guest's unsigned descriptor.
    pub descriptor: DropDescriptorBody,
}

/// A drop session, ready to receive chunks.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct DropSessionResponse {
    /// The drop session id (chunks `PATCH` against it).
    pub drop_id: String,
}

/// One pending drop in the provisioning user's inbox.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct PendingDropResponse {
    /// The inbox row id.
    pub drop_id: String,
    /// Content address of the drop blob.
    pub ciphertext_hash: String,
    /// Declared ciphertext size.
    pub size: u64,
    /// Declared content type.
    pub content_type: String,
    /// Guest-supplied name, unverified.
    pub suggested_filename: Option<String>,
    /// Server-attested arrival time (RFC 3339).
    pub received_at: String,
}

/// The inbox listing.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct InboxResponse {
    /// Pending drops awaiting review.
    pub drops: Vec<PendingDropResponse>,
}

/// The adoption request: the adopter's signed `create` manifest (canonical CBOR, base64) whose
/// `ciphertext_hash` references the inbox blob, the freshly sealed metadata blob it commits to,
/// and the destination album (server id).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(super) struct AdoptRequest {
    /// The signed `create` manifest (canonical CBOR, base64), `key_mode = wrapped`.
    pub manifest_cbor: String,
    /// The encrypted metadata blob (base64) matching the manifest's `metadata_blob_hash`.
    pub metadata_blob: String,
    /// The destination album (server id) to promote the drop into.
    pub album_id: String,
}

/// The adoption result.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct AdoptResponse {
    /// The promoted asset's id.
    pub asset_id: String,
}

// ───────────────────────────────────── Error rendering ───────────────────────────────────

/// A drop-endpoint rejection. `NotFound` renders a bare, body-less `404` (the indistinguishable
/// link probe); `Coded` renders `status` + a stable `error.*` code the client localizes.
#[derive(Debug)]
pub(super) enum DropReject {
    /// Link not found / revoked / expired, or a not-in-caller's-inbox drop the serve path hides.
    NotFound,
    /// A coded rejection.
    Coded {
        status: StatusCode,
        code: &'static str,
        message: String,
    },
}

impl DropReject {
    fn coded(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self::Coded {
            status,
            code,
            message: message.into(),
        }
    }

    fn unauthorized() -> Self {
        Self::coded(
            StatusCode::UNAUTHORIZED,
            error_codes::UPLOAD_FORBIDDEN,
            "authentication required",
        )
    }

    fn internal() -> Self {
        Self::Coded {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: error_codes::UPLOAD_STORAGE_INCONSISTENT,
            message: "internal server error".to_string(),
        }
    }
}

impl From<DropError> for DropReject {
    fn from(e: DropError) -> Self {
        match e {
            DropError::LinkNotFound => DropReject::NotFound,
            DropError::NotInInbox => DropReject::coded(
                StatusCode::CONFLICT,
                error_codes::DROP_NOT_IN_INBOX,
                e.to_string(),
            ),
            DropError::CapExceeded(_) => DropReject::coded(
                StatusCode::CONFLICT,
                error_codes::DROP_CAP_EXCEEDED,
                e.to_string(),
            ),
            DropError::QuotaExceeded => DropReject::coded(
                StatusCode::FORBIDDEN,
                error_codes::QUOTA_EXCEEDED,
                e.to_string(),
            ),
            DropError::GraceLocked => DropReject::coded(
                StatusCode::FORBIDDEN,
                error_codes::QUOTA_GRACE_LOCKED,
                e.to_string(),
            ),
            DropError::Db(err) => {
                tracing::error!("drop store db error: {err}");
                DropReject::internal()
            }
        }
    }
}

#[async_trait]
impl Writer for DropReject {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        match self {
            DropReject::NotFound => {
                res.status_code(StatusCode::NOT_FOUND);
            }
            DropReject::Coded {
                status,
                code,
                message,
            } => {
                res.status_code(status);
                res.render(Json(ApiError::with_code(message, code)));
            }
        }
    }
}

/// A successful drop-session creation (`201`).
pub(super) struct CreatedSession(DropSessionResponse);
#[async_trait]
impl Writer for CreatedSession {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        res.status_code(StatusCode::CREATED);
        res.render(Json(self.0));
    }
}

// ───────────────────────────────────── Guest endpoints ───────────────────────────────────

/// Session metadata stashed on the Valkey session so finalization can build the inbox row
/// without re-reading the link.
#[derive(Serialize, Deserialize)]
struct DropSessionMeta {
    link_id: String,
    single_use: bool,
    content_type: String,
    suggested_filename: Option<String>,
    descriptor: serde_json::Value,
}

/// Open a drop session against a live upload link (guest; link-capability auth).
#[handler]
pub async fn create_drop_session(
    req: &mut Request,
    depot: &mut Depot,
    opaque_id: PathParam<String>,
) -> Result<CreatedSession, DropReject> {
    let state = depot
        .obtain::<DropState>()
        .expect("DropState injected")
        .clone();
    let opaque_id = opaque_id.into_inner();

    // Parse the strict body ourselves so a malformed/extra field is a coded 400 (invariant 30).
    let request: CreateDropRequest = req.parse_json().await.map_err(|e| {
        DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::DROP_MALFORMED_DESCRIPTOR,
            format!("malformed drop request: {e}"),
        )
    })?;

    // Invariant 31: per-`{opaque-id}` and per-source-IP rate limit BEFORE any DB work.
    let ip_key = format!("ip:{}", client_ip(req));
    if !state.limiter.check(&format!("opaque:{opaque_id}")) || !state.limiter.check(&ip_key) {
        return Err(DropReject::coded(
            StatusCode::TOO_MANY_REQUESTS,
            error_codes::DROP_RATE_LIMITED,
            "too many drop attempts",
        ));
    }

    let now = Timestamp::now();

    // Invariant 26: the opaque id must resolve to a live link. Not-found/revoked/expired all
    // return an indistinguishable 404.
    let link = DropQuery::live_link_by_opaque(&state.conn, &opaque_id, now)
        .await
        .map_err(DropError::from)?
        .ok_or(DropReject::NotFound)?;

    // Passphrase abuse gate: a passphrase-protected link requires a valid Argon2id proof.
    if let Some(verifier_json) = &link.passphrase_verifier {
        let verifier: PassphraseVerifier =
            serde_json::from_value(verifier_json.clone()).map_err(|_| DropReject::internal())?;
        let ok = request
            .passphrase_proof
            .as_deref()
            .and_then(decode_hex)
            .is_some_and(|proof| ct_eq(&proof, &verifier.verifier));
        if !ok {
            return Err(DropReject::coded(
                StatusCode::FORBIDDEN,
                error_codes::DROP_PASSPHRASE_REQUIRED,
                "valid passphrase proof required",
            ));
        }
    }

    let descriptor = &request.descriptor;

    // Invariant 27: content_type is in the closed enum for the link's protocol version.
    if !state
        .config
        .allowed_content_types
        .iter()
        .any(|t| t == &descriptor.content_type)
    {
        return Err(DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::UPLOAD_UNSUPPORTED_CONTENT_TYPE,
            "unsupported content type",
        ));
    }

    // Invariant 28: size ∈ (0, min(link cap, server cap)].
    if request.size == 0 {
        return Err(DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::UPLOAD_INVALID_SIZE,
            "declared size must be greater than zero",
        ));
    }
    let per_file_cap = link.max_file_size.map_or(state.config.max_file_size, |c| {
        c.min(state.config.max_file_size)
    });
    if request.size > per_file_cap {
        return Err(DropReject::coded(
            StatusCode::PAYLOAD_TOO_LARGE,
            error_codes::UPLOAD_FILE_TOO_LARGE,
            "declared size exceeds the per-file cap",
        ));
    }

    // Invariant 30: the descriptor is structurally well-formed (kem_ct length matches the suite;
    // nonce_prefix + ciphertext_hash well-formed). The no-album/no-manifest rule is enforced by
    // `deny_unknown_fields` on the descriptor body.
    if let Err(msg) = check_descriptor(descriptor) {
        return Err(DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::DROP_MALFORMED_DESCRIPTOR,
            msg,
        ));
    }

    // Invariants 26 (cumulative caps) + 29 (owner quota) — one transaction: caps + quota debit +
    // original-blob reservation + cap counters.
    DropMutation::open_drop_reservation(
        &state.conn,
        &link.link_id,
        &link.owner_id,
        &descriptor.ciphertext_hash,
        request.size,
        &state.config.quota_limits,
        now,
    )
    .await?;

    // Open the Valkey chunk session (S-C1 mechanics). The drop id doubles as the session id and
    // the eventual inbox row id (UUIDv7).
    let drop_id = uuid::Uuid::now_v7().to_string();
    let meta = DropSessionMeta {
        link_id: link.link_id.clone(),
        single_use: link.single_use,
        content_type: descriptor.content_type.clone(),
        suggested_filename: descriptor.suggested_filename.clone(),
        descriptor: serde_json::to_value(descriptor).unwrap_or(serde_json::Value::Null),
    };
    let session = UploadSession {
        id: drop_id.clone(),
        asset_id: drop_id.clone(),
        owner_id: link.owner_id.clone(),
        upload_user_id: link.owner_id.clone(),
        album_id: None,
        content_type: Some(descriptor.content_type.clone()),
        expected_hash: descriptor.ciphertext_hash.clone(),
        crypto_suite_id: link.crypto_suite_id,
        protocol_version: link.protocol_version.clone(),
        blob_role: BlobRole::Original,
        intent_id: None,
        manifest_envelope: serde_json::to_string(&meta).unwrap_or_default(),
        received_bytes: 0,
        total_size: request.size,
        status: UploadSessionStatus::Pending,
        created_at: now,
        last_progress_at: now,
        expires_at: now + SignedDuration::from_hours(24),
    };
    if let Err(e) = state.session_manager.create(&session).await {
        tracing::error!("drop session create failed: {e}");
        // Roll back the reservation so a Valkey failure does not leak the owner's quota.
        let _ = DropMutation::release_reservation(
            &state.conn,
            &link.link_id,
            &descriptor.ciphertext_hash,
            request.size,
        )
        .await;
        return Err(DropReject::internal());
    }

    tracing::info!(%drop_id, owner_id = %link.owner_id, "drop session opened");
    Ok(CreatedSession(DropSessionResponse { drop_id }))
}

/// Append a chunk to a drop session (guest; link-capability). Reuses the upload protocol's
/// chunk rules (invariants 9–12) verbatim; on completion the blob is finalized into the inbox.
#[handler]
pub async fn append_drop_chunk(
    req: &mut Request,
    depot: &mut Depot,
    drop_id: PathParam<String>,
) -> Result<DropChunkOk, DropChunkError> {
    let state = depot
        .obtain::<DropState>()
        .expect("DropState injected")
        .clone();
    let drop_id = drop_id.into_inner();

    let session = state
        .session_manager
        .get(&drop_id)
        .await
        .map_err(|_| DropChunkError::NotFound)?
        .ok_or(DropChunkError::NotFound)?;

    // Strict media type: the body is opaque ciphertext bytes.
    let is_octet_stream = req
        .content_type()
        .is_some_and(|m| m.essence_str() == "application/octet-stream");
    if !is_octet_stream {
        return Err(DropChunkError::Upload(UploadError::UnsupportedMediaType));
    }

    let offset: u64 = req
        .header::<String>("X-Capsule-Offset")
        .and_then(|s| s.parse().ok())
        .ok_or(DropChunkError::Upload(UploadError::MissingOffset))?;

    let checksum = match req.header::<String>("X-Capsule-Checksum") {
        Some(c)
            if c.len() == 64
                && c.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            c
        }
        _ => return Err(DropChunkError::Upload(UploadError::MissingChecksum)),
    };

    if req
        .header::<u64>("Content-Length")
        .is_some_and(|len| len > MAX_CHUNK_SIZE)
    {
        return Err(DropChunkError::Upload(UploadError::ChunkTooLarge));
    }

    let bytes = req
        .payload_with_max_size(MAX_CHUNK_SIZE as usize + 1)
        .await
        .map_err(|_| DropChunkError::Upload(UploadError::EmptyChunk))?
        .clone();

    if bytes.is_empty() {
        return Err(DropChunkError::Upload(UploadError::EmptyChunk));
    }
    if bytes.len() as u64 > MAX_CHUNK_SIZE {
        return Err(DropChunkError::Upload(UploadError::ChunkTooLarge));
    }

    let body_hash = hash_bytes(&bytes);
    if body_hash != checksum {
        return Err(DropChunkError::Upload(UploadError::ChunkChecksumMismatch {
            header: checksum,
            body: body_hash,
        }));
    }

    // 4 KiB alignment for non-final chunks (invariant 10).
    if !bytes.len().is_multiple_of(4096) && session.total_size > 0 {
        let new_offset = session.received_bytes + bytes.len() as u64;
        if new_offset != session.total_size {
            return Err(DropChunkError::Upload(UploadError::ChunkNotAligned));
        }
    }

    if session.status.is_inactive() || session.status == UploadSessionStatus::WaitingForProcessing {
        return Err(DropChunkError::Upload(UploadError::SessionNotActive));
    }

    // Replay / conflict handling (invariant 12).
    if offset < session.received_bytes {
        return match state
            .session_manager
            .get_chunk(&drop_id, offset)
            .await
            .map_err(|_| DropChunkError::Internal)?
        {
            Some((recorded, next)) if recorded == checksum => Ok(DropChunkOk::progress(next)),
            Some(_) => Err(DropChunkError::Upload(UploadError::ChunkConflict)),
            None => Err(DropChunkError::Upload(UploadError::InvalidOffset {
                expected: session.received_bytes,
                actual: offset,
            })),
        };
    }
    if offset != session.received_bytes {
        return Err(DropChunkError::Upload(UploadError::InvalidOffset {
            expected: session.received_bytes,
            actual: offset,
        }));
    }

    // Cumulative bound (invariant 11).
    let new_size = session.received_bytes + bytes.len() as u64;
    if session.total_size > 0 && new_size > session.total_size {
        return Err(DropChunkError::Upload(UploadError::SizeExceeded));
    }

    // Durability before ACK.
    state
        .storage
        .append_at(&drop_id, offset, bytes.clone())
        .await
        .map_err(DropChunkError::Upload)?;
    if session.received_bytes == 0 && session.status == UploadSessionStatus::Pending {
        let _ = state
            .session_manager
            .update_status(&drop_id, UploadSessionStatus::Uploading)
            .await;
    }
    let received = state
        .session_manager
        .increment_received_bytes(&drop_id, bytes.len() as u64)
        .await
        .map_err(|_| DropChunkError::Internal)?;
    let _ = state.session_manager.touch_progress(&drop_id).await;
    let _ = state
        .session_manager
        .record_chunk(&drop_id, offset, &checksum, received)
        .await;

    // Completion → finalize the drop into the inbox.
    if session.total_size > 0 && received == session.total_size {
        finalize_drop(&state, &drop_id, &session).await?;
    }

    Ok(DropChunkOk::progress(received))
}

/// Finalize a completed drop transfer: verify the ciphertext hash (invariant 14), commit the
/// blob into the content-addressed store, and stage the inbox row (revoking a single-use link).
async fn finalize_drop(
    state: &DropState,
    drop_id: &str,
    session: &UploadSession,
) -> Result<(), DropChunkError> {
    // CAS into WaitingForProcessing so a replayed final chunk cannot double-finalize.
    if !state
        .session_manager
        .begin_finalize_cas(drop_id)
        .await
        .map_err(|_| DropChunkError::Internal)?
    {
        return Ok(());
    }

    // Invariant 14: recompute the ciphertext hash on the blocking pool.
    let path = state.storage.get_upload_path(drop_id);
    let expected = session.expected_hash.clone();
    let actual = tokio::task::spawn_blocking(move || get_file_hash(&path))
        .await
        .map_err(|_| DropChunkError::Internal)?
        .map_err(|_| DropChunkError::Internal)?;
    if actual != expected {
        let _ = state.storage.remove(drop_id).await;
        let _ = state
            .session_manager
            .update_status(drop_id, UploadSessionStatus::FailedProcessing)
            .await;
        return Err(DropChunkError::Upload(UploadError::ContentHashMismatch {
            expected,
            actual,
        }));
    }

    let meta: DropSessionMeta =
        serde_json::from_str(&session.manifest_envelope).map_err(|_| DropChunkError::Internal)?;

    // Commit the blob into the content-addressed store, then stage the inbox row.
    state
        .storage
        .commit_blob(drop_id, &session.expected_hash)
        .await
        .map_err(|_| DropChunkError::Internal)?;

    if let Err(e) = DropMutation::stage_drop(
        &state.conn,
        StageInput {
            drop_id: drop_id.to_string(),
            owner_id: session.owner_id.clone(),
            link_id: meta.link_id.clone(),
            ciphertext_hash: session.expected_hash.clone(),
            size: session.total_size,
            content_type: meta.content_type.clone(),
            suggested_filename: meta.suggested_filename.clone(),
            descriptor: meta.descriptor.clone(),
            single_use: meta.single_use,
        },
    )
    .await
    {
        tracing::error!("drop staging failed: {e}");
        // Un-finalize: GC the blob and release the reservation.
        let _ = state.storage.remove_blob(&session.expected_hash).await;
        let _ = DropMutation::release_reservation(
            &state.conn,
            &meta.link_id,
            &session.expected_hash,
            session.total_size,
        )
        .await;
        let _ = state
            .session_manager
            .update_status(drop_id, UploadSessionStatus::FailedProcessing)
            .await;
        return Err(DropChunkError::Internal);
    }

    let _ = state
        .session_manager
        .update_status(drop_id, UploadSessionStatus::Completed)
        .await;
    tracing::info!(%drop_id, owner_id = %session.owner_id, "drop finalized into inbox");
    Ok(())
}

/// A successful chunk append (`200` with `X-Capsule-Offset`).
pub(super) struct DropChunkOk {
    new_offset: u64,
}
impl DropChunkOk {
    fn progress(new_offset: u64) -> Self {
        Self { new_offset }
    }
}
#[async_trait]
impl Writer for DropChunkOk {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        res.status_code(StatusCode::OK);
        res.add_header("X-Capsule-Offset", self.new_offset.to_string(), true)
            .ok();
    }
}

/// A chunk-append rejection. Chunk-rule violations reuse the upload error taxonomy verbatim
/// (the chunk rules are the same); a missing session is an indistinguishable `404`.
pub(super) enum DropChunkError {
    NotFound,
    Internal,
    Upload(UploadError),
}
#[async_trait]
impl Writer for DropChunkError {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            DropChunkError::NotFound => {
                res.status_code(StatusCode::NOT_FOUND);
            }
            DropChunkError::Internal => {
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Json(ApiError::with_code(
                    "internal server error",
                    error_codes::UPLOAD_STORAGE_INCONSISTENT,
                )));
            }
            DropChunkError::Upload(e) => e.write(req, depot, res).await,
        }
    }
}

// ───────────────────────────────────── Owner endpoints ───────────────────────────────────

/// List the provisioning user's pending drops (owner; session auth).
#[handler]
pub async fn list_drop_inbox(
    req: &mut Request,
    depot: &mut Depot,
) -> Result<Json<InboxResponse>, DropReject> {
    let state = depot
        .obtain::<DropState>()
        .expect("DropState injected")
        .clone();
    let owner = owner_from_headers(req, &state)?;

    let drops = DropQuery::inbox(&state.conn, &owner)
        .await
        .map_err(DropError::from)?
        .into_iter()
        .map(|d| PendingDropResponse {
            drop_id: d.drop_id,
            ciphertext_hash: d.ciphertext_hash,
            size: d.size,
            content_type: d.content_type,
            suggested_filename: d.suggested_filename,
            received_at: d.received_at,
        })
        .collect();
    Ok(Json(InboxResponse { drops }))
}

/// Adopt a pending drop: validate the adopter's `create` manifest and atomically promote the
/// inbox blob to an album asset (owner; session auth). Invariant 32.
#[handler]
pub async fn adopt_drop(
    req: &mut Request,
    depot: &mut Depot,
    _drop_id: PathParam<String>,
) -> Result<Json<AdoptResponse>, DropReject> {
    let state = depot
        .obtain::<DropState>()
        .expect("DropState injected")
        .clone();
    let owner = owner_from_headers(req, &state)?;

    let request: AdoptRequest = req.parse_json().await.map_err(|e| {
        DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::UPLOAD_ENVELOPE_REJECTED,
            format!("malformed adoption request: {e}"),
        )
    })?;

    let manifest_cbor = BASE64.decode(&request.manifest_cbor).map_err(|_| {
        DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::UPLOAD_ENVELOPE_REJECTED,
            "manifest_cbor is not valid base64",
        )
    })?;
    let metadata_blob = BASE64.decode(&request.metadata_blob).map_err(|_| {
        DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::UPLOAD_ENVELOPE_REJECTED,
            "metadata_blob is not valid base64",
        )
    })?;

    // Decode the signed create manifest and run the adoption envelope battery (invariants
    // 1–8, 16–18, 25; key_mode ∈ {derived, wrapped} is enforced by the closed enum on decode).
    let manifest: AssetManifest = capsule_core::cbor::from_slice(&manifest_cbor).map_err(|_| {
        DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::UPLOAD_ENVELOPE_REJECTED,
            "manifest_cbor is not a valid AssetManifest",
        )
    })?;
    let now = Timestamp::now();
    let added_at = owner_added_at(&state, &owner).await;
    validate_adoption_manifest(
        &state,
        &manifest,
        &metadata_blob,
        &added_at,
        &now.to_string(),
    )?;

    // Invariant 6 (DB half): the owner has write capability on the destination album.
    let writable = service::album::Query::get_album_access(&state.conn, &owner, &request.album_id)
        .await
        .ok()
        .flatten()
        .as_ref()
        .is_some_and(capsule_core::models::album::AlbumAccess::is_write);
    if !writable {
        return Err(DropReject::coded(
            StatusCode::FORBIDDEN,
            error_codes::UPLOAD_ALBUM_ACCESS_DENIED,
            "no write capability on the destination album",
        ));
    }

    let ciphertext_hash = manifest.core.ciphertext_hash.to_hex();
    let metadata_hash = hash_bytes(&metadata_blob);

    // Durably store the metadata blob before the promotion (content-addressed; a rollback GCs it).
    state
        .storage
        .write_blob(&metadata_hash, &metadata_blob)
        .await
        .map_err(|_| DropReject::internal())?;

    let input = AdoptInput {
        album_id: request.album_id.clone(),
        ciphertext_hash,
        metadata_hash: metadata_hash.clone(),
        metadata_blob,
        manifest_cbor,
        protocol_version: manifest.core.protocol_version.clone(),
    };
    match DropMutation::adopt(&state.conn, &owner, input, &state.config.quota_limits).await {
        Ok(AdoptOutcome::Promoted { asset_id } | AdoptOutcome::AlreadyPromoted { asset_id }) => {
            Ok(Json(AdoptResponse { asset_id }))
        }
        Err(e) => {
            // A failed promotion leaves the just-written metadata blob orphaned; GC it.
            let _ = state.storage.remove_blob(&metadata_hash).await;
            Err(e.into())
        }
    }
}

/// Discard a pending drop; its bytes are GC'd and the owner's quota freed (owner; session auth).
#[handler]
pub async fn discard_drop(
    req: &mut Request,
    depot: &mut Depot,
    drop_id: PathParam<String>,
) -> Result<StatusCode, DropReject> {
    let state = depot
        .obtain::<DropState>()
        .expect("DropState injected")
        .clone();
    let owner = owner_from_headers(req, &state)?;
    let drop_id = drop_id.into_inner();

    match DropMutation::discard(&state.conn, &owner, &drop_id).await? {
        Some(discarded) => {
            if discarded.freed {
                let _ = state.storage.remove_blob(&discarded.ciphertext_hash).await;
            }
            Ok(StatusCode::NO_CONTENT)
        }
        None => Err(DropReject::NotFound),
    }
}

// ────────────────────────────────────────── Helpers ──────────────────────────────────────

/// Resolve the authenticated owner from the bearer token, or a `401`.
fn owner_from_headers(req: &Request, state: &DropState) -> Result<String, DropReject> {
    validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key)
        .map_err(|_| DropReject::unauthorized())
}

/// The owner's device-authorization floor (account-creation time), standing in for the
/// device-directory `added_at` (invariant 7) — mirrors the upload create path.
async fn owner_added_at(state: &DropState, owner: &str) -> String {
    entity::user::Entity::find_by_id(owner)
        .one(&state.conn)
        .await
        .ok()
        .flatten()
        .map_or_else(|| EPOCH_RFC3339.to_string(), |u| u.created_at.to_rfc3339())
}

/// A best-effort source-IP string for the per-IP rate limiter.
fn client_ip(req: &Request) -> String {
    req.header::<String>("X-Forwarded-For")
        .and_then(|v| v.split(',').next().map(|s| s.trim().to_string()))
        .or_else(|| req.header::<String>("X-Real-IP"))
        .unwrap_or_else(|| format!("{:?}", req.remote_addr()))
}

/// Invariant 30: structural well-formedness of a `DropDescriptor`.
fn check_descriptor(d: &DropDescriptorBody) -> Result<(), String> {
    // ciphertext_hash: 64-char lowercase hex.
    if d.ciphertext_hash.len() != 64 || !is_lower_hex(&d.ciphertext_hash) {
        return Err("ciphertext_hash is not a 32-byte lowercase-hex digest".to_string());
    }
    // nonce_prefix: 7 bytes → 14 lowercase-hex chars.
    if d.nonce_prefix.len() != 14 || !is_lower_hex(&d.nonce_prefix) {
        return Err("nonce_prefix is not a 7-byte lowercase-hex value".to_string());
    }
    // kem_ct: base64 decoding to exactly the suite's KEM-DEM length.
    match BASE64.decode(&d.kem_ct) {
        Ok(bytes) if bytes.len() == EXPECTED_KEM_CT_LEN => {}
        _ => {
            return Err(format!(
                "kem_ct length does not match the crypto suite (expected {EXPECTED_KEM_CT_LEN} bytes)"
            ));
        }
    }
    if d.chunk_size == 0 || d.plaintext_size == 0 {
        return Err("chunk_size and plaintext_size must be non-zero".to_string());
    }
    Ok(())
}

/// The adoption envelope battery over a decoded signed manifest (invariants 1–8, 16–18, 25).
fn validate_adoption_manifest(
    state: &DropState,
    manifest: &AssetManifest,
    metadata_blob: &[u8],
    added_at: &str,
    server_clock: &str,
) -> Result<(), DropReject> {
    let core = &manifest.core;
    let reject = |msg: &str| {
        DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::UPLOAD_ENVELOPE_REJECTED,
            msg.to_string(),
        )
    };

    // An adopting write is a `create`.
    if core.action != Action::Create {
        return Err(reject("adoption manifest action must be create"));
    }
    // Structural presence rules (wrapped/metadata/prior-hash by action).
    if !manifest.structural_ok() {
        return Err(reject("manifest fails structural presence rules"));
    }
    // Invariant 1: protocol version in the accepted window.
    match protocol_gate(
        &core.protocol_version,
        &state.config.protocol_min,
        &state.config.protocol_max,
    ) {
        Ok(()) => {}
        Err(HandshakeReject::ProtocolOutOfRange) => {
            return Err(DropReject::coded(
                StatusCode::UPGRADE_REQUIRED,
                error_codes::PROTOCOL_VERSION_UNSUPPORTED,
                "protocol version not supported",
            ));
        }
        Err(_) => return Err(reject("protocol_version is not a YYYY-MM-DD date")),
    }
    // Invariant 2: crypto suite in the inventory.
    check_suite(core.crypto_suite_id).map_err(|_| {
        DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::UPLOAD_UNKNOWN_CRYPTO_SUITE,
            "unknown crypto suite",
        )
    })?;
    // Invariant 25: the bundled metadata blob's content hash matches the manifest's commitment.
    if !metadata_blob_hash_matches(core.metadata_blob_hash, metadata_blob) {
        return Err(DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::UPLOAD_ENVELOPE_MISMATCH,
            "metadata_blob_hash mismatch",
        ));
    }
    // Invariants 6/7/8/17/18: the keyless envelope battery. `album_pin` = the manifest's own
    // protocol version (a create pins the album to the version it is written under); the chain
    // head / amk backstop are unset (a fresh create).
    let ctx = EnvelopeContext {
        album_pin: &core.protocol_version,
        device_added_at: added_at,
        server_clock,
        drift_days: state.config.timestamp_drift_days,
        stored_chain_head: None,
        stored_amk_version: None,
    };
    match check_manifest_envelope(core, &ctx) {
        Ok(()) => Ok(()),
        Err(EnvelopeReject::UnknownSuite) => Err(DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::UPLOAD_UNKNOWN_CRYPTO_SUITE,
            "unknown crypto suite",
        )),
        Err(EnvelopeReject::DeviceAddedAfter) => Err(DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::UPLOAD_DEVICE_NOT_AUTHORIZED,
            "creating device not authorized",
        )),
        Err(EnvelopeReject::TimestampUnsane) => Err(DropReject::coded(
            StatusCode::BAD_REQUEST,
            error_codes::UPLOAD_TIMESTAMP_OUT_OF_RANGE,
            "timestamp outside the accepted range",
        )),
        Err(_) => Err(reject("manifest envelope rejected")),
    }
}

/// Constant-time byte-slice equality.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Decode a lowercase-hex string into bytes.
fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

/// True if `s` is non-empty, all lowercase hex.
fn is_lower_hex(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
