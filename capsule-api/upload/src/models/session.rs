use chrono::{DateTime, Utc};
use salvo::oapi::ToSchema;
use serde::{Deserialize, Serialize};

/// The blob's role within its asset bundle, declared at session creation and
/// recorded on the pending row (closed enum; the visibility gate and staged
/// uploads reason over it — see the upload-protocol design doc).
#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BlobRole {
    Original,
    Derivative,
    Metadata,
    Provenance,
    Backup,
}

impl BlobRole {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            BlobRole::Original => "original",
            BlobRole::Derivative => "derivative",
            BlobRole::Metadata => "metadata",
            BlobRole::Provenance => "provenance",
            BlobRole::Backup => "backup",
        }
    }
}

/// Volatile transfer state for one upload session.
///
/// The record carries everything finalization needs — sizes, hash, crypto/protocol
/// pins, blob role, the manifest envelope, and the parties — so a session is
/// finalizable from its own record with no further client input (upload-protocol
/// design doc, §Endpoints).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub(crate) struct UploadSession {
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
    /// Expected SHA-256 hash for verification on finalize (64-char lowercase hex)
    pub expected_hash: String,
    /// Crypto suite the blob was sealed under (validated against the inventory by
    /// the envelope gate, S-C1)
    pub crypto_suite_id: u16,
    /// Pinned protocol date (`YYYY-MM-DD`) from session creation
    pub protocol_version: String,
    /// This blob's role in its bundle
    pub blob_role: BlobRole,
    /// Album-upgrade intent, when part of an upgrade ceremony
    pub intent_id: Option<String>,
    /// The server-visible manifest envelope, serialized JSON. Stored opaquely on
    /// the session; deep validation is the envelope gate's job (S-C1).
    pub manifest_envelope: String,

    // Upload state
    pub received_bytes: u64,
    pub total_size: u64,
    pub status: UploadSessionStatus,

    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    /// Timestamp of the last accepted chunk (creation time if none). Anchors the
    /// ≥1-hour survival floor; the pressure sweeper (S-C1) evicts
    /// least-recently-progressed first.
    pub last_progress_at: DateTime<Utc>,
    /// Expiration timestamp (the 24-hour cap)
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Copy, Clone, Eq, PartialEq, Serialize, Deserialize, ToSchema)]
pub(crate) enum UploadSessionStatus {
    /// Active with no accepted chunk yet
    Pending,
    /// Active with at least one accepted chunk
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
    pub(crate) fn in_progress(&self) -> bool {
        matches!(self, UploadSessionStatus::Uploading)
    }

    /// Returns true if upload session is still active
    pub(crate) fn is_active(&self) -> bool {
        !self.is_inactive()
    }

    /// Returns true is upload session is inactive
    pub(crate) fn is_inactive(&self) -> bool {
        matches!(
            self,
            UploadSessionStatus::Completed | UploadSessionStatus::FailedProcessing
        )
    }

    /// Wire value for the `X-Capsule-Upload-Status` response header on `HEAD`.
    pub(crate) fn as_header_value(&self) -> &'static str {
        match self {
            UploadSessionStatus::Pending => "pending",
            UploadSessionStatus::Uploading => "uploading",
            UploadSessionStatus::WaitingForProcessing => "waiting_for_processing",
            UploadSessionStatus::Completed => "completed",
            UploadSessionStatus::FailedProcessing => "failed_processing",
        }
    }
}
