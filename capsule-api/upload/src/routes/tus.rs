use auth::utils::headers::validate_user_from_headers;
use capsule_core::utils::hash::hash_bytes;
use salvo::oapi::extract::PathParam;
use salvo::prelude::*;

use crate::error::UploadError;
use crate::models::requests::CreateUploadRequest;
use crate::models::responses::{
    CreateUploadResponse, CreateUploadResponses, DeleteUploadResponses, HeadUploadResponse,
    HeadUploadResponses, ListSessionsResponse, ListSessionsResponses, PatchUploadResponses,
};
use crate::models::session::UploadSessionStatus;
use crate::service::upload::{AppendOutcome, CreateOutcome};
use crate::state::AppState;

// Constants for chunk sizes (4KB aligned)
const KB: u64 = 1024;
const CHUNK_SIZE_256KB: u64 = 256 * KB;
const CHUNK_SIZE_1MB: u64 = 1024 * KB;
const CHUNK_SIZE_4MB: u64 = 4 * 1024 * KB;
/// Protocol-surface maximum chunk size (upload-protocol doc, §Chunk Rules).
const MAX_CHUNK_SIZE: u64 = 16 * 1024 * KB;

/// Calculate suggested chunk size based on total file size.
/// A starting point only — adaptation is the client's concern; these tiers are
/// server-tunable and not protocol surface.
fn get_suggested_chunk_size(total_size: Option<u64>) -> u64 {
    match total_size {
        Some(size) if size < 10 * 1024 * KB => CHUNK_SIZE_256KB, // < 10MB
        Some(size) if size < 100 * 1024 * KB => CHUNK_SIZE_1MB,  // < 100MB
        _ => CHUNK_SIZE_4MB,                                     // >= 100MB or unknown
    }
}

/// Create a new upload session
#[endpoint(
    operation_id = "create_upload",
    tags("upload"),
    security(("bearer" = []))
)]
pub async fn create_upload(req: &mut Request, dep: &mut Depot) -> CreateUploadResponses {
    let state = dep
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    // Parse the strict request body ourselves so an unknown/malformed field is our own
    // `400 error.upload.malformed_request` (Strictness Table), not the extractor's untyped
    // 400. The transport JSON is `deny_unknown_fields`.
    let request = match req.parse_json::<CreateUploadRequest>().await {
        Ok(r) => r,
        Err(e) => {
            return CreateUploadResponses::Error(UploadError::InvalidUpload(format!(
                "malformed request body: {e}"
            )));
        }
    };

    // Authenticate User
    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return CreateUploadResponses::Unauthorized(e.to_string()),
        };

    // Use user_id as owner_id if not specified
    let owner_id = request.owner_id.clone().unwrap_or_else(|| user_id.clone());

    // Permission check if owner is different
    if owner_id != user_id {
        let allowed =
            service::friendship::Query::can_upload_with_owner(&state.conn, &user_id, &owner_id)
                .await
                .map_err(|e| CreateUploadResponses::InternalServerError(eyre::eyre!(e).into()));

        match allowed {
            Ok(true) => {} // Permitted
            Ok(false) => {
                return CreateUploadResponses::Error(UploadError::OwnerNotPermitted);
            }
            Err(e) => return e,
        }
    }

    match state
        .upload_service
        .create_session(&request, &owner_id, &user_id)
        .await
    {
        Ok(CreateOutcome::Created(session)) => {
            let suggested_chunk_size = get_suggested_chunk_size(Some(request.size));
            CreateUploadResponses::Success(CreateUploadResponse {
                id: session.id.clone(),
                upload_url: format!("/upload/{}", session.id),
                suggested_chunk_size,
            })
        }
        Ok(CreateOutcome::Existing(session)) => {
            let suggested_chunk_size = get_suggested_chunk_size(Some(session.total_size));
            CreateUploadResponses::Existing {
                response: CreateUploadResponse {
                    id: session.id.clone(),
                    upload_url: format!("/upload/{}", session.id),
                    suggested_chunk_size,
                },
                offset: session.received_bytes,
            }
        }
        // Typed rejections (duplicate_blob 409, malformed 400, …) render with
        // their taxonomy status + error.* code instead of collapsing to 500.
        Err(e) => CreateUploadResponses::Error(e),
    }
}

/// Get upload session status
#[endpoint(
    operation_id = "head_upload",
    tags("upload"),
    security(("bearer" = []))
)]
pub async fn head_upload(
    req: &mut Request,
    dep: &mut Depot,
    id: PathParam<String>,
) -> HeadUploadResponses {
    let state = dep
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    let id = id.into_inner();

    // Authenticate User
    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return HeadUploadResponses::Unauthorized(e.to_string()),
        };

    match state.upload_service.get_session(&id).await {
        Ok(Some(session)) => {
            // Check if user is the uploader or the owner
            if session.upload_user_id != user_id && session.owner_id != user_id {
                return HeadUploadResponses::Forbidden;
            }
            HeadUploadResponses::Success(HeadUploadResponse {
                offset: session.received_bytes,
                total_size: if session.total_size > 0 {
                    Some(session.total_size)
                } else {
                    None
                },
                status: session.status,
            })
        }
        Ok(None) => HeadUploadResponses::NotFound,
        Err(e) => HeadUploadResponses::InternalServerError(eyre::eyre!(e).into()),
    }
}

/// Append a chunk to an upload
#[endpoint(
    operation_id = "patch_upload",
    tags("upload"),
    security(("bearer" = []))
)]
pub async fn patch_upload(
    req: &mut Request,
    dep: &mut Depot,
    id: PathParam<String>,
) -> PatchUploadResponses {
    let state = dep
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    let id = id.into_inner();

    // Authenticate User
    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return PatchUploadResponses::Unauthorized(e.to_string()),
        };

    // Fetch the session once: ownership check + final-chunk alignment exemption.
    let session = match state.upload_service.get_session(&id).await {
        Ok(Some(session)) => {
            if session.upload_user_id != user_id {
                return PatchUploadResponses::Forbidden;
            }
            session
        }
        Ok(None) => return PatchUploadResponses::Error(UploadError::SessionNotFound),
        Err(e) => return PatchUploadResponses::InternalServerError(eyre::eyre!(e).into()),
    };

    // Strict media type: the body is opaque ciphertext bytes (415 otherwise).
    let is_octet_stream = req
        .content_type()
        .is_some_and(|mime| mime.essence_str() == "application/octet-stream");
    if !is_octet_stream {
        return PatchUploadResponses::Error(UploadError::UnsupportedMediaType);
    }

    // Parse X-Capsule-Offset header (invariant 9: required and well-formed).
    let offset: u64 = match req
        .header::<String>("X-Capsule-Offset")
        .and_then(|s| s.parse().ok())
    {
        Some(o) => o,
        None => {
            return PatchUploadResponses::Error(UploadError::MissingOffset);
        }
    };

    // X-Capsule-Checksum is REQUIRED: bare lowercase-hex SHA-256 of the chunk
    // bytes. The (upload_id, offset, chunk_hash) idempotency tuple (invariant 12)
    // is undefined without it.
    let checksum = match req.header::<String>("X-Capsule-Checksum") {
        Some(c)
            if c.len() == 64
                && c.bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            c
        }
        _ => return PatchUploadResponses::Error(UploadError::MissingChecksum),
    };

    // Protocol-surface chunk ceiling: reject an over-large chunk on its declared
    // Content-Length before buffering it (413), rather than letting the transport's body
    // limit surface an untyped error.
    if req
        .header::<u64>("Content-Length")
        .is_some_and(|len| len > MAX_CHUNK_SIZE)
    {
        return PatchUploadResponses::Error(UploadError::ChunkTooLarge);
    }

    // Read the body with a ceiling one byte above the protocol maximum so a legitimate
    // maximum-size chunk is accepted and a `16 MiB + 1` chunk still buffers far enough for
    // the explicit ceiling check below to reject it as `chunk_too_large` (413).
    let body = match req.payload_with_max_size(MAX_CHUNK_SIZE as usize + 1).await {
        Ok(b) => b,
        Err(e) => {
            return PatchUploadResponses::BadRequest(format!("Failed to read body: {e}"));
        }
    };
    let bytes = body.clone();

    // Strictness: empty chunks are a client bug, never a silent no-op.
    if bytes.is_empty() {
        return PatchUploadResponses::Error(UploadError::EmptyChunk);
    }

    // Protocol-surface chunk ceiling (belt-and-suspenders for a chunked body with no
    // declared Content-Length).
    if bytes.len() as u64 > MAX_CHUNK_SIZE {
        return PatchUploadResponses::Error(UploadError::ChunkTooLarge);
    }

    // Verify the checksum against the received bytes BEFORE any write: a
    // mismatch persists nothing (transit-corruption defense, invariant 12).
    let body_hash = hash_bytes(&bytes);
    if body_hash != checksum {
        return PatchUploadResponses::Error(UploadError::ChunkChecksumMismatch {
            header: checksum,
            body: body_hash,
        });
    }

    // 4 KiB alignment for non-final chunks (invariant 10).
    if bytes.len() % 4096 != 0 && session.total_size > 0 {
        let new_offset = session.received_bytes + bytes.len() as u64;
        if new_offset != session.total_size {
            return PatchUploadResponses::Error(UploadError::ChunkNotAligned);
        }
    }

    // Append chunk, keyed by the (upload_id, offset, chunk_hash) idempotency tuple.
    match state
        .upload_service
        .append_chunk(&id, bytes, offset, &checksum)
        .await
    {
        Ok(AppendOutcome::Accepted(session)) => {
            // Completion (received == declared size) runs finalization automatically.
            if session.total_size > 0
                && session.received_bytes == session.total_size
                && let Err(e) = state.upload_service.finalize_upload(&id).await
            {
                return PatchUploadResponses::Error(e);
            }
            PatchUploadResponses::Success {
                new_offset: session.received_bytes,
            }
        }
        // A replayed chunk (lost ACK) is a no-op that returns the same offset — never a
        // second finalize.
        Ok(AppendOutcome::Replay { new_offset }) => PatchUploadResponses::Success { new_offset },
        Err(e) => PatchUploadResponses::Error(e),
    }
}

/// Delete/cancel an upload session
#[endpoint(
    operation_id = "delete_upload",
    tags("upload"),
    security(("bearer" = []))
)]
pub async fn delete_upload(
    req: &mut Request,
    dep: &mut Depot,
    id: PathParam<String>,
) -> DeleteUploadResponses {
    let state = dep
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    let id = id.into_inner();

    // Authenticate User
    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return DeleteUploadResponses::Unauthorized(e.to_string()),
        };

    // Verify ownership - only the uploader or owner can delete
    match state.upload_service.get_session(&id).await {
        Ok(Some(session)) => {
            if session.upload_user_id != user_id && session.owner_id != user_id {
                return DeleteUploadResponses::Forbidden;
            }
        }
        Ok(None) => return DeleteUploadResponses::NotFound,
        Err(e) => return DeleteUploadResponses::InternalServerError(eyre::eyre!(e).into()),
    }

    match state.upload_service.cancel_upload(&id).await {
        Ok(()) => DeleteUploadResponses::Success,
        Err(e) => match e {
            UploadError::SessionNotFound => DeleteUploadResponses::NotFound,
            // Finalization is not interruptible (409).
            UploadError::SessionNotActive => DeleteUploadResponses::Error(e),
            _ => DeleteUploadResponses::InternalServerError(eyre::eyre!(e).into()),
        },
    }
}

/// List user's upload sessions
#[endpoint(
    operation_id = "list_sessions",
    tags("upload"),
    security(("bearer" = []))
)]
pub async fn list_sessions(req: &mut Request, dep: &mut Depot) -> ListSessionsResponses {
    let state = dep
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    // Authenticate User
    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return ListSessionsResponses::Unauthorized(e.to_string()),
        };

    // Parse query parameters
    let status_filter: Option<UploadSessionStatus> = req
        .query::<String>("status")
        .and_then(|s| serde_json::from_str::<UploadSessionStatus>(&format!("\"{s}\"")).ok());

    // List sessions by the uploader (the resuming party).
    match state
        .upload_service
        .list_sessions_by_uploader(&user_id)
        .await
    {
        Ok(sessions) => {
            // Apply status filter if specified
            let filtered_sessions = if let Some(status) = status_filter {
                sessions
                    .into_iter()
                    .filter(|s| s.status == status)
                    .collect()
            } else {
                sessions
            };

            ListSessionsResponses::Success(ListSessionsResponse {
                sessions: filtered_sessions,
            })
        }
        Err(e) => ListSessionsResponses::InternalServerError(eyre::eyre!(e).into()),
    }
}
