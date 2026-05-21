use salvo::prelude::*;
use thiserror::Error;

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum UploadError {
    #[error("File exceeds size limit")]
    FileTooLarge,
    #[error("File system error: {0}")]
    IoError(#[from] std::io::Error),
    #[error("Database error: {0}")]
    DbError(#[from] sea_orm::DbErr),
    #[error("Serialization error: {0}")]
    SerdeError(#[from] serde_json::Error),
    #[error("Valkey error: {0}")]
    ValkeyError(#[from] bb8_redis::redis::RedisError),
    #[error("RunError: {0}")]
    RunError(#[from] bb8_redis::bb8::RunError<bb8_redis::redis::RedisError>),
    #[error("Session not found")]
    SessionNotFound,
    #[error("Upload already complete")]
    UploadComplete,
    #[error("Upload session is being processed by another instance")]
    UploadInstanceConflict,
    #[error("Invalid offset: expected {expected}, got {actual}")]
    InvalidOffset { expected: u64, actual: u64 },
    #[error("Invalid upload: {0}")]
    InvalidUpload(String),
    #[error("Processing error: {0}")]
    ProcessingError(String),
    #[error("Parse error: {0}")]
    ParseError(#[from] std::string::ParseError),
    #[error("Checksum mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { expected: String, actual: String },
    #[error("Invalid chunk size: {0}")]
    InvalidChunkSize(String),
    #[error("Unknown error: {0}")]
    Unknown(String),
}

#[async_trait]
impl Writer for UploadError {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        let (status, message) = match self {
            UploadError::FileTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                String::from("File exceeds size limit"),
            ),
            UploadError::IoError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                String::from("File system error"),
            ),
            UploadError::DbError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                String::from("Database error"),
            ),
            UploadError::SerdeError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                String::from("Serialization error"),
            ),
            UploadError::ValkeyError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                String::from("Cache error"),
            ),
            UploadError::RunError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                String::from("Cache connection error"),
            ),
            UploadError::SessionNotFound => (
                StatusCode::NOT_FOUND,
                String::from("Upload session not found"),
            ),
            UploadError::UploadComplete => (
                StatusCode::CONFLICT,
                String::from("Upload already complete"),
            ),
            UploadError::InvalidOffset { expected, actual } => (
                StatusCode::CONFLICT,
                format!("Invalid offset. Expected {}, got {}", expected, actual),
            ),
            UploadError::InvalidUpload(msg) => (StatusCode::BAD_REQUEST, msg),
            UploadError::ProcessingError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                String::from("Processing error"),
            ),
            UploadError::ParseError(_) => (StatusCode::BAD_REQUEST, String::from("Parse error")),
            UploadError::ChecksumMismatch { expected, actual } => (
                StatusCode::BAD_REQUEST,
                format!("Checksum mismatch. Expected {}, got {}", expected, actual),
            ),
            UploadError::InvalidChunkSize(msg) => (
                StatusCode::BAD_REQUEST,
                format!("Invalid chunk size: {msg}"),
            ),
            UploadError::UploadInstanceConflict => (
                StatusCode::CONFLICT,
                String::from("Upload session is being processed by another instance"),
            ),
            UploadError::Unknown(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };

        res.status_code(status);
        res.render(Text::Plain(message));
    }
}
