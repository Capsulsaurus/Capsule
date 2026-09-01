use capsule_wire::headers as wire_headers;
use model::errors::InternalServerError;
use salvo::oapi::ToSchema;
use serde::{Deserialize, Serialize};

use crate::error::UploadError;
use crate::models::session::{UploadSession, UploadSessionStatus};

/// Response for a successful upload creation
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub(crate) struct CreateUploadResponse {
    /// Upload session ID
    pub id: String,
    /// URL to use for uploading chunks
    pub upload_url: String,
    /// Suggested chunk size for this upload
    pub suggested_chunk_size: u64,
}

/// Response for upload head request
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub(crate) struct HeadUploadResponse {
    /// Current offset (bytes received)
    pub offset: u64,
    /// Total size if known
    pub total_size: Option<u64>,
    /// Upload status
    pub status: UploadSessionStatus,
}

/// Response for session listing
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub(crate) struct ListSessionsResponse {
    pub sessions: Vec<UploadSession>,
}

/// Response for `GET /quota` — the uploader's storage-quota snapshot (S-C6).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub(crate) struct QuotaResponse {
    /// Bytes currently charged to the uploader.
    pub used: u64,
    /// Soft-warning threshold in bytes (`null` when unlimited).
    pub soft_limit: Option<u64>,
    /// Hard-refusal threshold in bytes (`null` when unlimited).
    pub hard_limit: Option<u64>,
    /// Quota state: `ok | soft_warning | hard_exceeded | grace_expired | suspended`.
    pub state: String,
}

/// Response for `POST /albums` — album provisioning (S-C25).
///
/// Deliberately tiny and key-free: the id the caller asked for, echoed back so a client can
/// assert the server stored the exact spelling it derived, plus whether this call created the
/// row. Carries **no** name or description — the server holds no album title (S-C26).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub(crate) struct ProvisionAlbumResponse {
    /// The provisioned album id, echoed verbatim.
    pub album_id: String,
    /// `true` when this call created the row; `false` when it was already provisioned to the
    /// caller and nothing was written. Informational only — a client treats both as success.
    pub created: bool,
}

/// Responses for create upload endpoint
#[allow(dead_code)]
pub(crate) enum CreateUploadResponses {
    Success(CreateUploadResponse),
    /// The active session already open for this idempotency tuple (`200`, carries the
    /// authoritative offset so the client resumes without a `HEAD`).
    Existing {
        response: CreateUploadResponse,
        offset: u64,
    },
    Unauthorized(String),
    Forbidden,
    BadRequest(String),
    /// A typed upload rejection: renders with its taxonomy status + error.* code.
    Error(UploadError),
    InternalServerError(InternalServerError),
}

capsule_wire::salvo_responses! {
    CreateUploadResponses {
        Success(response) => 201,
            header("Location", &response.upload_url)
            header(
                wire_headers::SUGGESTED_CHUNK_SIZE,
                response.suggested_chunk_size.to_string(),
            )
            json(response),
            doc("Upload session created", schema = CreateUploadResponse);
        // Idempotent create: the active session, not a second one (`200`), carrying the
        // authoritative offset so the client resumes without a HEAD.
        Existing { response, offset } => 200,
            header("Location", &response.upload_url)
            header(wire_headers::OFFSET, offset.to_string())
            header(
                wire_headers::SUGGESTED_CHUNK_SIZE,
                response.suggested_chunk_size.to_string(),
            )
            json(response),
            undocumented();
        Unauthorized(msg) => 401, text(msg),
            doc("Unauthorized - invalid or missing token");
        Forbidden {} => 403, empty(), doc("Forbidden - insufficient permissions");
        BadRequest(msg) => 400, text(msg), doc("Bad request - invalid parameters");
        Error(e) => _, delegate(e), undocumented();
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        409 => "Conflict - content hash already finalized (error.upload.duplicate_blob; merge trigger)",
        413 => "Payload too large - declared size exceeds the limit",
        500 => "Internal server error",
    }
}

/// Responses for head upload endpoint
pub(crate) enum HeadUploadResponses {
    Success(HeadUploadResponse),
    Unauthorized(String),
    NotFound,
    Forbidden,
    InternalServerError(InternalServerError),
}

capsule_wire::salvo_responses! {
    HeadUploadResponses {
        // A HEAD response carries no body; progress and state ride headers
        // (X-Capsule-Upload-Status is census-registered).
        Success(response) => 200,
            header(wire_headers::OFFSET, response.offset.to_string())
            header_option(
                wire_headers::CONTENT_LENGTH,
                response.total_size.map(|total| total.to_string()),
            )
            header(wire_headers::UPLOAD_STATUS, response.status.as_header_value())
            header("Cache-Control", "no-store")
            empty(),
            doc(
                "Upload progress and state via X-Capsule-Offset / X-Capsule-Content-Length / X-Capsule-Upload-Status headers (no body)"
            );
        Unauthorized(msg) => 401, text(msg), doc("Unauthorized");
        NotFound {} => 404, empty(), doc("Upload session not found");
        Forbidden {} => 403, empty(), doc("Forbidden - not owner of session");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
    }
}

/// Responses for patch upload (append chunk) endpoint
pub(crate) enum PatchUploadResponses {
    Success {
        new_offset: u64,
    },
    BadRequest(String),
    Unauthorized(String),
    Forbidden,
    /// A typed upload rejection: renders with its taxonomy status + error.* code.
    Error(UploadError),
    InternalServerError(InternalServerError),
}

capsule_wire::salvo_responses! {
    PatchUploadResponses {
        Success { new_offset } => 204,
            header(wire_headers::OFFSET, new_offset.to_string()) empty(),
            doc("Chunk uploaded successfully");
        BadRequest(msg) => 400, text(msg),
            doc("Bad request - invalid chunk size or checksum");
        Unauthorized(msg) => 401, text(msg), doc("Unauthorized");
        Forbidden {} => 403, empty(), doc("Forbidden - not owner of session");
        Error(e) => _, delegate(e), undocumented();
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        404 => "Upload session not found",
        409 => "Conflict - offset mismatch / chunk conflict / session not active",
        413 => "Payload too large - chunk exceeds the 16 MiB maximum",
        415 => "Unsupported media type - chunk body must be application/octet-stream",
        500 => "Internal server error",
    }
}

/// Responses for delete upload endpoint
pub(crate) enum DeleteUploadResponses {
    Success,
    Unauthorized(String),
    Forbidden,
    NotFound,
    /// A typed upload rejection: renders with its taxonomy status + error.* code.
    Error(UploadError),
    InternalServerError(InternalServerError),
}

capsule_wire::salvo_responses! {
    DeleteUploadResponses {
        Success {} => 204, empty(), doc("Upload session deleted");
        Unauthorized(msg) => 401, text(msg), doc("Unauthorized");
        Forbidden {} => 403, empty(), doc("Forbidden - not owner of session");
        NotFound {} => 404, empty(), doc("Upload session not found");
        Error(e) => _, delegate(e), undocumented();
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        409 => "Conflict - finalization in progress is not interruptible",
        500 => "Internal server error",
    }
}

/// Responses for list sessions endpoint
pub(crate) enum ListSessionsResponses {
    Success(ListSessionsResponse),
    Unauthorized(String),
    InternalServerError(InternalServerError),
}

capsule_wire::salvo_responses! {
    ListSessionsResponses {
        Success(response) => 200, json(response),
            doc("List of upload sessions", schema = ListSessionsResponse);
        Unauthorized(msg) => 401, text(msg), doc("Unauthorized");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
    }
}

/// Responses for the `GET /quota` endpoint (S-C6).
pub(crate) enum QuotaResponses {
    Success(QuotaResponse),
    Unauthorized(String),
    InternalServerError(InternalServerError),
}

capsule_wire::salvo_responses! {
    QuotaResponses {
        Success(response) => 200,
            header("Cache-Control", "no-store") json(response),
            doc("The uploader's storage-quota snapshot", schema = QuotaResponse);
        Unauthorized(msg) => 401, text(msg), doc("Unauthorized");
        InternalServerError(e) => _, delegate(e), undocumented();
    }
    delegated {
        500 => "Internal server error",
    }
}
