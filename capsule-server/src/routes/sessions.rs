//! `GET /v1/upload/sessions` — what this account has half-uploaded (slice `S-C57`).
//!
//! The resumption listing. A client that lost its local record of an in-flight upload — a
//! reinstall, a second device, a crash before the session id was persisted — has no other way to
//! find it, and without this the bytes already on the server are unreachable and are eventually
//! evicted. That is the whole reason the operation exists, and it is why the listing is scoped to
//! the **uploader** rather than to the owner: resuming is something only the uploading party can
//! do.
//!
//! # A bad `status` is refused, where Salvo ignored it
//!
//! The retired handler parsed the filter with `serde_json::from_str(...).ok()` and dropped the
//! `None` on the floor, so `?status=complete` — one letter off `completed` — silently returned
//! *every* session instead of none. A filter that is quietly ignored is worse than one that is
//! refused: the caller believes the list is filtered and acts on it. Here an unknown value is a
//! `400` naming what was expected.
//!
//! # `S-C28` audit
//!
//! | Status | Verdict |
//! | --- | --- |
//! | `200` | the caller's live sessions, oldest first — an empty list is a normal answer |
//! | `400 error.upload.invalid_status_filter` | `?status=` named something that is not a status |
//! | `401` / `403` | the framework's, through `Auth` |
//! | `500 error.upload.unavailable` | the session store could not answer |
//!
//! No `404`: an account with nothing in flight has an empty library of sessions, not a missing
//! one.

use capsule_i18n::error_codes;
use kynos::prelude::*;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};

use crate::auth::AccessToken;
use crate::routes::upload::UploadTag;
use crate::store::{UploadSessionRecord, UploadSessionStatus, UserId};
use crate::upload::UploadContext;

// ===========================================================================================
// Wire types
// ===========================================================================================

/// How the listing is narrowed.
#[derive(Schema, QueryParams, Debug)]
pub struct SessionsQuery {
    /// Return only sessions in this state.
    ///
    /// One of `pending`, `uploading`, `waiting_for_processing`, `completed`,
    /// `failed_processing` — the same tokens the `X-Capsule-Upload-Status` header carries, so a
    /// client filters on the value it was already given rather than on a second vocabulary.
    pub status: Option<String>,
}

/// One in-flight upload, as the resumption listing serves it.
///
/// Deliberately not the whole [`UploadSessionRecord`]. The manifest envelope, the expected hash
/// and the crypto pin are finalization's inputs and are already the client's own — echoing them
/// to every listing would put a signed document in a response nobody reads it from.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct SessionSummary {
    /// The session id, which is what `PATCH /v1/upload/{id}` resumes against.
    pub id: String,
    /// The asset the bundle belongs to, so a client can group a bundle's sessions.
    pub asset_id: String,
    /// The album it is filed into, when it named one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album_id: Option<String>,
    /// This blob's role in its bundle: `original`, `derivative`, `metadata`, `provenance` or
    /// `backup`.
    pub blob_role: String,
    /// Where the session is in its state machine.
    pub status: String,
    /// Bytes durably accepted so far — the offset a resume continues from.
    pub received_bytes: u64,
    /// The declared total.
    pub total_size: u64,
    /// When the session was created, RFC 3339.
    pub created_at: String,
    /// When the last chunk was accepted, RFC 3339, or the creation time if none.
    ///
    /// What the survival floor is measured from, so a client can tell which of its stalled
    /// sessions is closest to being evicted.
    pub last_progress_at: String,
}

impl From<UploadSessionRecord> for SessionSummary {
    fn from(record: UploadSessionRecord) -> Self {
        Self {
            id: record.upload_id.as_str().to_owned(),
            asset_id: record.asset_id.as_str().to_owned(),
            album_id: record.album_id.map(|album| album.as_str().to_owned()),
            blob_role: record.blob_role.as_str().to_owned(),
            status: record.status.as_str().to_owned(),
            received_bytes: record.received_bytes,
            total_size: record.total_size,
            created_at: record.created_at.to_string(),
            last_progress_at: record.last_progress_at.to_string(),
        }
    }
}

/// The listing.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct SessionsResponse {
    /// Every session the caller can resume, oldest first, ties broken by upload id.
    pub sessions: Vec<SessionSummary>,
}

/// Why a listing was not returned.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum SessionsRejection {
    /// `?status=` named something that is not a status.
    #[error("{detail}")]
    #[problem(status = 400, title = "Invalid status filter")]
    InvalidStatus {
        /// What was wrong, in English. The client localizes `code`, not this.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The session store could not answer.
    #[error("the upload sessions could not be listed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Read a status back from the token the server serves it under.
///
/// The inverse of [`UploadSessionStatus::as_str`], and deliberately spelled here rather than as a
/// `serde` derive on the enum: the port's status is a domain type with no wire traits, and giving
/// it one would let it be deserialized from a request body somewhere it should not be.
fn parse_status(token: &str) -> Option<UploadSessionStatus> {
    Some(match token {
        "pending" => UploadSessionStatus::Pending,
        "uploading" => UploadSessionStatus::Uploading,
        "waiting_for_processing" => UploadSessionStatus::WaitingForProcessing,
        "completed" => UploadSessionStatus::Completed,
        "failed_processing" => UploadSessionStatus::FailedProcessing,
        _ => return None,
    })
}

/// Every upload the caller can resume.
///
/// Oldest first, which is the order the store promises and the order a client wants: the oldest
/// in-flight session is the one closest to eviction.
#[kynos::get(
    "/v1/upload/sessions",
    operation_id = "list_upload_sessions",
    tag = UploadTag
)]
pub async fn list_upload_sessions(
    Inject(upload): Inject<UploadContext>,
    Auth(credential): Auth<AccessToken>,
    Query(query): Query<SessionsQuery>,
) -> Result<Json<SessionsResponse>, SessionsRejection> {
    // Parsed before the store is reached, so a typo costs no round trip — and, more to the
    // point, is answered rather than ignored.
    let filter =
        match query.status.as_deref() {
            None => None,
            Some(token) => Some(parse_status(token).ok_or_else(|| {
                SessionsRejection::InvalidStatus {
                    detail: format!(
                        "{token:?} is not an upload status; expected one of pending, uploading, \
                 waiting_for_processing, completed, failed_processing"
                    ),
                    code: error_codes::UPLOAD_INVALID_STATUS_FILTER,
                }
            })?),
        };

    let caller = UserId::new(credential.user.as_str());
    let sessions = upload
        .sessions()
        .sessions_for_uploader(&caller)
        .await
        .map_err(|error| {
            tracing::error!(%error, user_id = %caller, "the session store could not be listed");
            SessionsRejection::Unavailable {
                code: error_codes::UPLOAD_UNAVAILABLE,
            }
        })?;

    let sessions: Vec<SessionSummary> = sessions
        .into_iter()
        .filter(|record| filter.is_none_or(|status| record.status == status))
        .map(SessionSummary::from)
        .collect();

    tracing::debug!(user_id = %caller, count = sessions.len(), "listed resumable uploads");
    Ok(Json(SessionsResponse { sessions }))
}
