//! The account activity log, read by its owner.
//!
//! One operation, and it is the account's own: design/activity-log.md makes the log owner-only,
//! and the route takes no account parameter so reading somebody else's is not expressible. There
//! is no admin read, for the reason [`super::moderation`] has none — no admin authentication
//! model exists.
//!
//! The listing sits beside the session list (`GET /v1/auth/devices`): a user asking "was that
//! me?" reads the sessions that are open and the events that opened and closed them.

use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::activity::{ActivityContext, ActivityEvent, MAX_EVENTS_PER_ACCOUNT};
use crate::auth::AccessToken;
use crate::routes::devices::SessionsTag;
use crate::store::UserId;

/// How many events a listing returns when the caller names no `limit`.
pub const DEFAULT_LIMIT: usize = 100;

/// How the listing is narrowed.
#[derive(Schema, QueryParams, Debug)]
pub struct ActivityQuery {
    /// The most events to return, newest first. Defaults to 100; a larger value than the log's
    /// bound (1,000) returns the whole log.
    pub limit: Option<u32>,
}

/// One event in the caller's activity log.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ActivityEventView {
    /// The server's id for the event.
    pub event_id: String,
    /// What happened, e.g. `sign_in.password` or `session.revoked`.
    ///
    /// The set is closed per server version and grows additively: a client renders a kind it
    /// does not know as a generic entry with its time, and never refuses the listing for one.
    pub kind: String,
    /// When it was recorded, RFC 3339.
    pub at: String,
    /// The `X-Request-Id` of the request that caused it — the string an operator finds the
    /// request's log lines by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// The session that acted, when one did. Absent for a sign-in and for a revoke-all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// What the event is about: a session, a share link or a drop link, by its id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

impl From<ActivityEvent> for ActivityEventView {
    fn from(event: ActivityEvent) -> Self {
        Self {
            event_id: event.event_id,
            kind: event.kind.as_str().to_owned(),
            at: event.at.to_string(),
            request_id: event.request_id,
            session_id: event.session_id.map(|id| id.as_str().to_owned()),
            subject: event.subject,
        }
    }
}

/// The caller's activity log.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ActivityResponse {
    /// The newest events inside the retention window (90 days), newest first.
    pub events: Vec<ActivityEventView>,
}

/// Why the log could not be read.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ActivityRejection {
    /// The store could not answer.
    #[error("the activity log could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// The number of events a `limit` asks for, inside the log's bound.
fn page_size(limit: Option<u32>) -> usize {
    limit
        .map_or(DEFAULT_LIMIT, |limit| {
            usize::try_from(limit).unwrap_or(MAX_EVENTS_PER_ACCOUNT)
        })
        .min(MAX_EVENTS_PER_ACCOUNT)
}

/// Serve the caller's own activity log, newest first.
#[kynos::get("/v1/auth/activity", operation_id = "list_activity", tag = SessionsTag)]
pub async fn list_activity(
    Auth(credential): Auth<AccessToken>,
    Inject(activity): Inject<ActivityContext>,
    Query(query): Query<ActivityQuery>,
) -> Result<Json<ActivityResponse>, ActivityRejection> {
    let user = UserId::new(credential.user.as_str());
    let events = activity
        .recent(&user, page_size(query.limit))
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the activity log could not be read");
            ActivityRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;
    tracing::debug!(%user, events = events.len(), "served an activity log");
    Ok(Json(ActivityResponse {
        events: events.into_iter().map(ActivityEventView::from).collect(),
    }))
}

#[cfg(test)]
mod tests;
