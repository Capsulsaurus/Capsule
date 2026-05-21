use chrono::{DateTime, Utc};

use salvo::oapi::ToSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UploadSession {
    /// Upload Session ID
    pub id: String,
    /// Asset ID (usually created by Postgres during session creation)
    pub asset_id: String,
    /// Owner ID
    pub owner_id: String,
    /// User ID who initiated the upload (this matters for storage quota)
    pub upload_user_id: String,
    /// Optional Album ID to link the upload to
    pub album_id: Option<String>,
    /// Content type of the file being uploaded
    pub content_type: Option<String>,
    /// Expected BLAKE3 hash for verification on finalize (64-char lowercase hex)
    pub expected_hash: String,

    // Upload state
    pub received_bytes: u64,
    pub total_size: u64,
    pub status: UploadSessionStatus,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    /// Expiration timestamp
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize, ToSchema)]
pub enum UploadSessionStatus {
    /// Active with no active upload
    Pending,
    /// Active with an active upload
    Uploading,
    /// Waiting for processing to complete
    WaitingForProcessing,
    /// Completed successfully
    Completed,
    /// Failed to process
    FailedProcessing,
}

impl UploadSessionStatus {
    /// Returns true if the upload is in progress
    #[allow(dead_code)]
    pub fn in_progress(&self) -> bool {
        matches!(self, UploadSessionStatus::Uploading)
    }

    /// Returns true if upload session is still active
    pub fn is_active(&self) -> bool {
        !self.is_inactive()
    }

    /// Returns true is upload session is inactive
    pub fn is_inactive(&self) -> bool {
        matches!(
            self,
            UploadSessionStatus::Completed | UploadSessionStatus::FailedProcessing
        )
    }
}
