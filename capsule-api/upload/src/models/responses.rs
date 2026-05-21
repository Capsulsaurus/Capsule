use crate::models::session::{UploadSession, UploadSessionStatus};
use model::errors::InternalServerError;
use salvo::http::StatusCode;
use salvo::oapi::{EndpointOutRegister, ToSchema};
use salvo::prelude::*;
use serde::{Deserialize, Serialize};

/// Response for a successful upload creation
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateUploadResponse {
    /// Upload session ID
    pub id: String,
    /// URL to use for uploading chunks
    pub upload_url: String,
    /// Suggested chunk size for this upload
    pub suggested_chunk_size: u64,
}

/// Response for upload head request
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct HeadUploadResponse {
    /// Current offset (bytes received)
    pub offset: u64,
    /// Total size if known
    pub total_size: Option<u64>,
    /// Upload status
    pub status: UploadSessionStatus,
}

/// Response for session listing
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ListSessionsResponse {
    pub sessions: Vec<UploadSession>,
}

/// Responses for create upload endpoint
#[allow(dead_code)]
pub enum CreateUploadResponses {
    Success(CreateUploadResponse),
    Unauthorized(String),
    Forbidden,
    BadRequest(String),
    InternalServerError(InternalServerError),
}

#[async_trait]
impl Writer for CreateUploadResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(response) => {
                res.status_code(StatusCode::CREATED);
                res.add_header("Location", &response.upload_url, true).ok();
                res.add_header(
                    "X-Capsule-Suggested-Chunk-Size",
                    response.suggested_chunk_size.to_string(),
                    true,
                )
                .ok();
                res.render(Json(response));
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Text::Plain(msg));
            }
            Self::Forbidden => {
                res.status_code(StatusCode::FORBIDDEN);
            }
            Self::BadRequest(msg) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Text::Plain(msg));
            }
            Self::InternalServerError(e) => {
                e.write(req, depot, res).await;
            }
        }
    }
}

impl EndpointOutRegister for CreateUploadResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("201"),
            salvo::oapi::Response::new("Upload session created").add_content(
                "application/json",
                salvo::oapi::Content::new(CreateUploadResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Bad request - invalid parameters"),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized - invalid or missing token"),
        );
        operation.responses.insert(
            String::from("403"),
            salvo::oapi::Response::new("Forbidden - insufficient permissions"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

/// Responses for head upload endpoint
pub enum HeadUploadResponses {
    Success(HeadUploadResponse),
    Unauthorized(String),
    NotFound,
    Forbidden,
    InternalServerError(InternalServerError),
}

#[async_trait]
impl Writer for HeadUploadResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(response) => {
                res.status_code(StatusCode::OK);
                res.add_header("X-Capsule-Offset", response.offset.to_string(), true)
                    .ok();
                if let Some(total) = response.total_size {
                    res.add_header("X-Capsule-Content-Length", total.to_string(), true)
                        .ok();
                }
                res.add_header("Cache-Control", "no-store", true).ok();
                res.render(Json(response));
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Text::Plain(msg));
            }
            Self::NotFound => {
                res.status_code(StatusCode::NOT_FOUND);
            }
            Self::Forbidden => {
                res.status_code(StatusCode::FORBIDDEN);
            }
            Self::InternalServerError(e) => {
                e.write(req, depot, res).await;
            }
        }
    }
}

impl EndpointOutRegister for HeadUploadResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Upload status").add_content(
                "application/json",
                salvo::oapi::Content::new(HeadUploadResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized"),
        );
        operation.responses.insert(
            String::from("403"),
            salvo::oapi::Response::new("Forbidden - not owner of session"),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("Upload session not found"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

/// Responses for patch upload (append chunk) endpoint
pub enum PatchUploadResponses {
    Success { new_offset: u64 },
    BadRequest(String),
    Unauthorized(String),
    Forbidden,
    NotFound,
    Conflict(String),
    InternalServerError(InternalServerError),
}

#[async_trait]
impl Writer for PatchUploadResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success { new_offset } => {
                res.status_code(StatusCode::NO_CONTENT);
                res.add_header("X-Capsule-Offset", new_offset.to_string(), true)
                    .ok();
            }
            Self::BadRequest(msg) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Text::Plain(msg));
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Text::Plain(msg));
            }
            Self::Forbidden => {
                res.status_code(StatusCode::FORBIDDEN);
            }
            Self::NotFound => {
                res.status_code(StatusCode::NOT_FOUND);
            }
            Self::Conflict(msg) => {
                res.status_code(StatusCode::CONFLICT);
                res.render(Text::Plain(msg));
            }
            Self::InternalServerError(e) => {
                e.write(req, depot, res).await;
            }
        }
    }
}

impl EndpointOutRegister for PatchUploadResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("204"),
            salvo::oapi::Response::new("Chunk uploaded successfully"),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Bad request - invalid chunk size or checksum"),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized"),
        );
        operation.responses.insert(
            String::from("403"),
            salvo::oapi::Response::new("Forbidden - not owner of session"),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("Upload session not found"),
        );
        operation.responses.insert(
            String::from("409"),
            salvo::oapi::Response::new("Conflict - offset mismatch"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

/// Responses for delete upload endpoint
pub enum DeleteUploadResponses {
    Success,
    Unauthorized(String),
    Forbidden,
    NotFound,
    InternalServerError(InternalServerError),
}

#[async_trait]
impl Writer for DeleteUploadResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success => {
                res.status_code(StatusCode::NO_CONTENT);
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Text::Plain(msg));
            }
            Self::Forbidden => {
                res.status_code(StatusCode::FORBIDDEN);
            }
            Self::NotFound => {
                res.status_code(StatusCode::NOT_FOUND);
            }
            Self::InternalServerError(e) => {
                e.write(req, depot, res).await;
            }
        }
    }
}

impl EndpointOutRegister for DeleteUploadResponses {
    fn register(_components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("204"),
            salvo::oapi::Response::new("Upload session deleted"),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized"),
        );
        operation.responses.insert(
            String::from("403"),
            salvo::oapi::Response::new("Forbidden - not owner of session"),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("Upload session not found"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

/// Responses for list sessions endpoint
pub enum ListSessionsResponses {
    Success(ListSessionsResponse),
    Unauthorized(String),
    InternalServerError(InternalServerError),
}

#[async_trait]
impl Writer for ListSessionsResponses {
    async fn write(self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Success(response) => {
                res.status_code(StatusCode::OK);
                res.render(Json(response));
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Text::Plain(msg));
            }
            Self::InternalServerError(e) => {
                e.write(req, depot, res).await;
            }
        }
    }
}

impl EndpointOutRegister for ListSessionsResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("List of upload sessions").add_content(
                "application/json",
                salvo::oapi::Content::new(ListSessionsResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}
