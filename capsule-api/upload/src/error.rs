use auth::models::errors::ApiError;
use capsule_i18n::error_codes;
use salvo::prelude::*;
use thiserror::Error;

/// The upload error domain. Every client-visible rejection maps to a stable
/// `error.upload.*` catalog code (the upload-protocol design doc's Error
/// Taxonomy is the SSoT); clients switch on the code, never on the bare HTTP
/// status. Deep enforcement of the envelope invariants is S-C1 — the variants
/// exist now so the taxonomy is frozen.
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
    #[error("Session is already finished or being processed")]
    SessionNotActive,
    #[error("Session is already being finalized")]
    FinalizeInProgress,
    #[error("Invalid offset: expected {expected}, got {actual}")]
    InvalidOffset { expected: u64, actual: u64 },
    #[error("Missing or invalid X-Capsule-Offset header")]
    MissingOffset,
    #[error("Same offset re-sent with a different chunk hash")]
    ChunkConflict,
    #[error("Invalid upload: {0}")]
    InvalidUpload(String),
    #[error("Upload exceeds its declared size")]
    SizeExceeded,
    #[error("This content is already stored as asset {asset_id}")]
    DuplicateBlob { asset_id: String },
    #[error("Chunk body must be application/octet-stream")]
    UnsupportedMediaType,
    #[error("Empty chunk")]
    EmptyChunk,
    #[error("Chunk size must be 4 KiB-aligned (except the final chunk)")]
    ChunkNotAligned,
    #[error("Chunk exceeds the 16 MiB protocol maximum")]
    ChunkTooLarge,
    #[error("Missing or malformed X-Capsule-Checksum header")]
    MissingChecksum,
    #[error("Chunk checksum mismatch: header {header}, body {body}")]
    ChunkChecksumMismatch { header: String, body: String },
    #[error("Content hash mismatch: expected {expected}, got {actual}")]
    ContentHashMismatch { expected: String, actual: String },
    #[error("Processing error: {0}")]
    ProcessingError(String),
    #[error("Parse error: {0}")]
    ParseError(#[from] std::string::ParseError),
    #[error("Upload file length {on_disk} diverged from expected offset {expected}")]
    StorageInconsistent { expected: u64, on_disk: u64 },

    // ── Session-creation envelope invariants (upload-protocol Error Taxonomy). ──
    /// Invariant 1: `X-Capsule-Protocol` / `protocol_version` outside the accepted window.
    #[error("Protocol version is not supported; accepted range is [{min}, {max}]")]
    ProtocolUnsupported { min: String, max: String },
    /// Invariant 2: `crypto_suite_id` not in the primitives inventory.
    #[error("Unknown crypto suite")]
    UnknownCryptoSuite,
    /// Invariant 3: declared `hash` is not lowercase hex of the suite's digest length.
    #[error("Declared hash is not valid for the crypto suite")]
    InvalidHash,
    /// Invariant 4: declared `size` is zero (or otherwise out of `(0, max_file_size]`).
    #[error("Declared size must be greater than zero")]
    InvalidSize,
    /// Invariant 5: `content_type` outside the closed enum for this protocol version.
    #[error("Unsupported content type")]
    UnsupportedContentType,
    /// Invariant 6: album missing / no write capability / protocol drift.
    #[error("Album access denied")]
    AlbumAccessDenied,
    /// Invariant 7: `created_by_device` not authorized in the uploader's directory.
    #[error("Creating device is not authorized")]
    DeviceNotAuthorized,
    /// Invariant 8: envelope `timestamp` beyond the gross-drift sanity window.
    #[error("Envelope timestamp is outside the accepted range")]
    TimestampOutOfRange,
    /// Invariant 15 family: a top-level field contradicts the `manifest_envelope`.
    #[error("Top-level fields contradict the manifest envelope: {0}")]
    EnvelopeMismatch(&'static str),
    /// Invariant 15 (finalization): the envelope failed re-validation.
    #[error("Manifest envelope re-validation failed: {0}")]
    EnvelopeRejected(String),
    /// On-behalf upload without a verified relationship.
    #[error("Uploading on behalf of this owner is not permitted")]
    OwnerNotPermitted,
    /// The declared size would exceed the uploader's storage quota.
    #[error("Quota would be exceeded by the declared size")]
    QuotaExceeded,
    /// The caller is neither the uploader nor the owner of the session.
    #[error("Forbidden")]
    Forbidden,

    #[error("Unknown error: {0}")]
    Unknown(String),
}

impl UploadError {
    /// The stable `error.*` catalog code for this rejection, when one applies.
    /// Constants come from `capsule_i18n::error_codes` so a typo is a compile
    /// error and the code stays in sync with the canonical catalog.
    pub fn code(&self) -> Option<&'static str> {
        match self {
            UploadError::FileTooLarge => Some(error_codes::UPLOAD_FILE_TOO_LARGE),
            UploadError::SessionNotFound => Some(error_codes::UPLOAD_SESSION_NOT_FOUND),
            UploadError::SessionNotActive => Some(error_codes::UPLOAD_SESSION_NOT_ACTIVE),
            UploadError::FinalizeInProgress => Some(error_codes::UPLOAD_FINALIZE_IN_PROGRESS),
            UploadError::InvalidOffset { .. } => Some(error_codes::UPLOAD_OFFSET_MISMATCH),
            UploadError::SizeExceeded => Some(error_codes::UPLOAD_SIZE_EXCEEDED),
            UploadError::DuplicateBlob { .. } => Some(error_codes::UPLOAD_DUPLICATE_BLOB),
            UploadError::UnsupportedMediaType => Some(error_codes::UPLOAD_UNSUPPORTED_MEDIA_TYPE),
            UploadError::EmptyChunk => Some(error_codes::UPLOAD_EMPTY_CHUNK),
            UploadError::ChunkNotAligned => Some(error_codes::UPLOAD_CHUNK_NOT_ALIGNED),
            UploadError::ChunkTooLarge => Some(error_codes::UPLOAD_CHUNK_TOO_LARGE),
            UploadError::MissingChecksum => Some(error_codes::UPLOAD_MISSING_CHECKSUM),
            UploadError::ChunkChecksumMismatch { .. } => {
                Some(error_codes::UPLOAD_CHECKSUM_MISMATCH)
            }
            UploadError::ContentHashMismatch { .. } => {
                Some(error_codes::UPLOAD_CONTENT_HASH_MISMATCH)
            }
            UploadError::StorageInconsistent { .. } => {
                Some(error_codes::UPLOAD_STORAGE_INCONSISTENT)
            }
            UploadError::InvalidUpload(_) => Some(error_codes::UPLOAD_MALFORMED_REQUEST),
            UploadError::MissingOffset => Some(error_codes::UPLOAD_MISSING_OFFSET),
            UploadError::ChunkConflict => Some(error_codes::UPLOAD_CHUNK_CONFLICT),
            UploadError::ProtocolUnsupported { .. } => {
                Some(error_codes::PROTOCOL_VERSION_UNSUPPORTED)
            }
            UploadError::UnknownCryptoSuite => Some(error_codes::UPLOAD_UNKNOWN_CRYPTO_SUITE),
            UploadError::InvalidHash => Some(error_codes::UPLOAD_INVALID_HASH),
            UploadError::InvalidSize => Some(error_codes::UPLOAD_INVALID_SIZE),
            UploadError::UnsupportedContentType => {
                Some(error_codes::UPLOAD_UNSUPPORTED_CONTENT_TYPE)
            }
            UploadError::AlbumAccessDenied => Some(error_codes::UPLOAD_ALBUM_ACCESS_DENIED),
            UploadError::DeviceNotAuthorized => Some(error_codes::UPLOAD_DEVICE_NOT_AUTHORIZED),
            UploadError::TimestampOutOfRange => Some(error_codes::UPLOAD_TIMESTAMP_OUT_OF_RANGE),
            UploadError::EnvelopeMismatch(_) => Some(error_codes::UPLOAD_ENVELOPE_MISMATCH),
            UploadError::EnvelopeRejected(_) => Some(error_codes::UPLOAD_ENVELOPE_REJECTED),
            UploadError::OwnerNotPermitted => Some(error_codes::UPLOAD_OWNER_NOT_PERMITTED),
            UploadError::QuotaExceeded => Some(error_codes::QUOTA_EXCEEDED),
            UploadError::Forbidden => Some(error_codes::UPLOAD_FORBIDDEN),
            _ => None,
        }
    }

    fn status(&self) -> StatusCode {
        match self {
            UploadError::FileTooLarge | UploadError::ChunkTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            UploadError::SessionNotFound => StatusCode::NOT_FOUND,
            UploadError::SessionNotActive
            | UploadError::FinalizeInProgress
            | UploadError::DuplicateBlob { .. }
            | UploadError::ChunkConflict
            | UploadError::InvalidOffset { .. } => StatusCode::CONFLICT,
            UploadError::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            UploadError::ProtocolUnsupported { .. } => StatusCode::UPGRADE_REQUIRED,
            UploadError::AlbumAccessDenied
            | UploadError::DeviceNotAuthorized
            | UploadError::OwnerNotPermitted
            | UploadError::QuotaExceeded
            | UploadError::Forbidden => StatusCode::FORBIDDEN,
            UploadError::InvalidUpload(_)
            | UploadError::SizeExceeded
            | UploadError::EmptyChunk
            | UploadError::ChunkNotAligned
            | UploadError::MissingChecksum
            | UploadError::MissingOffset
            | UploadError::ChunkChecksumMismatch { .. }
            | UploadError::ContentHashMismatch { .. }
            | UploadError::UnknownCryptoSuite
            | UploadError::InvalidHash
            | UploadError::InvalidSize
            | UploadError::UnsupportedContentType
            | UploadError::TimestampOutOfRange
            | UploadError::EnvelopeMismatch(_)
            | UploadError::EnvelopeRejected(_)
            | UploadError::ParseError(_) => StatusCode::BAD_REQUEST,
            UploadError::IoError(_)
            | UploadError::DbError(_)
            | UploadError::SerdeError(_)
            | UploadError::ValkeyError(_)
            | UploadError::RunError(_)
            | UploadError::ProcessingError(_)
            | UploadError::StorageInconsistent { .. }
            | UploadError::Unknown(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

#[async_trait]
impl Writer for UploadError {
    async fn write(self, _req: &mut Request, _depot: &mut Depot, res: &mut Response) {
        let status = self.status();
        // Internal errors keep their detail out of the response body.
        let message = if status == StatusCode::INTERNAL_SERVER_ERROR {
            match &self {
                UploadError::StorageInconsistent { .. } => self.to_string(),
                _ => String::from("Internal server error"),
            }
        } else {
            self.to_string()
        };

        // Stale-offset conflicts carry the authoritative offset so the client
        // can re-align without a extra HEAD round-trip.
        if let UploadError::InvalidOffset { expected, .. } = &self {
            res.add_header("X-Capsule-Offset", expected.to_string(), true)
                .ok();
        }

        // An out-of-window protocol rejection advertises the accepted range so the
        // client can surface an actionable "update to keep uploading" message.
        if let UploadError::ProtocolUnsupported { min, max } = &self {
            res.add_header("X-Capsule-Protocol-Min", min.clone(), true)
                .ok();
            res.add_header("X-Capsule-Protocol-Max", max.clone(), true)
                .ok();
        }

        res.status_code(status);
        match self.code() {
            Some(code) => res.render(Json(ApiError::with_code(message, code))),
            None => res.render(Json(ApiError::new(message))),
        }
    }
}
