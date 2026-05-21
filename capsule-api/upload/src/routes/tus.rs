use crate::models::requests::CreateUploadRequest;
use crate::models::responses::{
    CreateUploadResponse, CreateUploadResponses, DeleteUploadResponses, HeadUploadResponse,
    HeadUploadResponses, ListSessionsResponse, ListSessionsResponses, PatchUploadResponses,
};
use crate::models::session::UploadSessionStatus;
use crate::state::AppState;
use auth::utils::headers::validate_user_from_headers;
use salvo::oapi::extract::{JsonBody, PathParam};
use salvo::prelude::*;

// TODO: Thoroughly review and test this module.

// Constants for chunk sizes (4KB aligned)
const KB: u64 = 1024;
const CHUNK_SIZE_256KB: u64 = 256 * KB;
const CHUNK_SIZE_1MB: u64 = 1024 * KB;
const CHUNK_SIZE_4MB: u64 = 4 * 1024 * KB;

/// Calculate suggested chunk size based on total file size
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
pub async fn create_upload(
    req: &mut Request,
    dep: &mut Depot,
    body: JsonBody<CreateUploadRequest>,
) -> CreateUploadResponses {
    let state = dep.obtain::<AppState>().unwrap();
    let request = body.0;

    // Authenticate User
    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return CreateUploadResponses::Unauthorized(e.to_string()),
        };

    // Use user_id as owner_id if not specified
    let owner_id = request.owner_id.unwrap_or_else(|| user_id.clone());

    // Permission check if owner is different
    if owner_id != user_id {
        let allowed =
            service::friendship::Query::can_upload_with_owner(&state.conn, &user_id, &owner_id)
                .await
                .map_err(|e| CreateUploadResponses::InternalServerError(eyre::eyre!(e).into()));

        match allowed {
            Ok(true) => {} // Permitted
            Ok(false) => return CreateUploadResponses::Forbidden,
            Err(e) => return e,
        }
    }

    match state
        .upload_service
        .create_session(
            &owner_id,
            &user_id,
            Some(request.content_type),
            request.size,
            request.hash,
            request.album_id,
            request.filename,
        )
        .await
    {
        Ok(session) => {
            let suggested_chunk_size = get_suggested_chunk_size(Some(request.size));
            CreateUploadResponses::Success(CreateUploadResponse {
                id: session.id.clone(),
                upload_url: format!("/upload/{}", session.id),
                suggested_chunk_size,
            })
        }
        Err(e) => CreateUploadResponses::InternalServerError(eyre::eyre!(e).into()), // TODO: It swallows all UploadError including duplicate.
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
    let state = dep.obtain::<AppState>().unwrap();
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
    let state = dep.obtain::<AppState>().unwrap();
    let id = id.into_inner();

    // Authenticate User
    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return PatchUploadResponses::Unauthorized(e.to_string()),
        };

    // Verify ownership - only the uploader can append chunks
    match state.upload_service.get_session(&id).await {
        Ok(Some(session)) => {
            if session.upload_user_id != user_id {
                return PatchUploadResponses::Forbidden;
            }
        }
        Ok(None) => return PatchUploadResponses::NotFound,
        Err(e) => return PatchUploadResponses::InternalServerError(eyre::eyre!(e).into()),
    };

    // Parse X-Capsule-Offset header
    let offset: u64 = match req
        .header::<String>("X-Capsule-Offset")
        .and_then(|s| s.parse().ok())
    {
        Some(o) => o,
        None => {
            return PatchUploadResponses::BadRequest("Missing X-Capsule-Offset header".to_string());
        }
    };

    // Parse optional X-Capsule-Checksum header for verification
    let _checksum: Option<String> = req.header::<String>("X-Capsule-Checksum");

    let body = match req.payload().await {
        Ok(b) => b,
        Err(e) => {
            return PatchUploadResponses::BadRequest(format!("Failed to read body: {}", e));
        }
    };
    let bytes = body.clone();

    // Validate 4KB alignment
    if bytes.is_empty() {
        // Empty chunk - nothing to do, just return current offset
        return match state.upload_service.get_session(&id).await {
            Ok(Some(session)) => PatchUploadResponses::Success {
                new_offset: session.received_bytes,
            },
            Ok(None) => PatchUploadResponses::NotFound,
            Err(e) => PatchUploadResponses::InternalServerError(eyre::eyre!(e).into()),
        };
    }

    if bytes.len() % 4096 != 0 {
        // Allow non-aligned chunks only if it's the final chunk
        let session = match state.upload_service.get_session(&id).await {
            Ok(Some(s)) => s,
            Ok(None) => return PatchUploadResponses::NotFound,
            Err(e) => return PatchUploadResponses::InternalServerError(eyre::eyre!(e).into()),
        };

        if session.total_size > 0 {
            let total = session.total_size;
            let new_offset = session.received_bytes + bytes.len() as u64;
            if new_offset != total {
                return PatchUploadResponses::BadRequest(
                    "Chunk size must be 4KB aligned (except for final chunk)".to_string(),
                );
            }
        }
    }

    // Append chunk
    match state.upload_service.append_chunk(&id, bytes, offset).await {
        Ok(session) => {
            // Check for completion
            if session.total_size > 0 && session.received_bytes == session.total_size {
                // Attempt finalize
                match state.upload_service.finalize_upload(&id).await {
                    Ok(_) => {
                        // Finalized successfully
                    }
                    Err(e) => {
                        return PatchUploadResponses::InternalServerError(eyre::eyre!(e).into());
                    }
                }
            }
            PatchUploadResponses::Success {
                new_offset: session.received_bytes,
            }
        }
        Err(e) => {
            if e.to_string().contains("Invalid offset") {
                PatchUploadResponses::Conflict(e.to_string())
            } else {
                PatchUploadResponses::InternalServerError(eyre::eyre!(e).into())
            }
        }
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
    let state = dep.obtain::<AppState>().unwrap();
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
    };

    match state.upload_service.cancel_upload(&id).await {
        Ok(_) => DeleteUploadResponses::Success,
        Err(e) => {
            if matches!(e, crate::error::UploadError::SessionNotFound) {
                DeleteUploadResponses::NotFound
            } else {
                DeleteUploadResponses::InternalServerError(eyre::eyre!(e).into())
            }
        }
    }
}

/// List user's upload sessions
#[endpoint(
    operation_id = "list_sessions",
    tags("upload"),
    security(("bearer" = []))
)]
pub async fn list_sessions(req: &mut Request, dep: &mut Depot) -> ListSessionsResponses {
    let state = dep.obtain::<AppState>().unwrap();

    // Authenticate User
    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return ListSessionsResponses::Unauthorized(e.to_string()),
        };

    // Parse query parameters
    let status_filter: Option<UploadSessionStatus> = req
        .query::<String>("status")
        .and_then(|s| serde_json::from_str::<UploadSessionStatus>(&format!("\"{}\"", s)).ok());

    // List sessions by owner
    match state.upload_service.list_sessions_by_owner(&user_id).await {
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
