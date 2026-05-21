use chrono::{DateTime, Utc};
use salvo::oapi::ToSchema;
use serde::{Deserialize, Serialize};

/// Request body for creating an upload session
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CreateUploadRequest {
    /// Original filename from client
    pub filename: String,
    /// File size in bytes
    pub size: u64,
    /// BLAKE3 hash of the complete file (64-char lowercase hex)
    pub hash: String,
    /// MIME type (e.g., "image/jpeg")
    pub content_type: String,
    /// Optional album to add asset to
    pub album_id: Option<String>,
    /// Optional owner ID (defaults to authenticated user)
    pub owner_id: Option<String>,
    /// Date asset was created/taken (from client EXIF or filesystem)
    pub date: Option<DateTime<Utc>>,
}
