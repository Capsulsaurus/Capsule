//! The session ledger — listing a user's devices and revoking one (slices `S-C13`, `S-N3`).
//!
//! Items 1 and 2 of design/authentication.md's session ledger: *list all active sessions*, and
//! *revoke any single session, authenticated by any active session token*. Item 3 — the global
//! revoke — is deliberately somewhere else and deliberately harder: see [`super::auth`].
//!
//! # Why single-session revoke is the easy one
//!
//! The asymmetry is the design. Revoking one session is the everyday tool and takes any live
//! token, because the worst an attacker with a stolen token can do with it is revoke a session —
//! including, if they aim badly, their own. Revoking *every* session is the nuclear option and
//! takes the account's identity key, because an attacker who could invoke it would lock the
//! owner out of every device they own. Putting them on the same authentication would collapse
//! that distinction, so they are two surfaces rather than one with a flag.
//!
//! # The cohort is legibility, and the surface is shaped so it cannot become more
//!
//! Reinstalling re-enrolls with a **new** `device_id` by design — device keys are
//! hardware-bound and non-exportable — so one physical phone accumulates several ledger entries
//! over its life and the user cannot tell them apart. `cohort_hash` groups them.
//!
//! It is client-asserted and unverifiable, so **no authorization path may read it**, and this
//! module is written so that reading one would be conspicuous: the cohort appears on the
//! listing and nowhere else, there is no filter parameter that takes one, and revocation names
//! a `session_id`. A "revoke this cohort" verb would be an authorization decision made from a
//! spoofable string, which is exactly the surface the advisory-only rule exists to refuse.
//!
//! # `last_active_at` moves, and it is stale by up to a minute on purpose
//!
//! `AuthStateStore::touch_session` had no caller on any request path until `S-C48` put the
//! session ledger on the bearer scheme's path, so this field used to be the sign-in time
//! forever. It is now written forward by any authenticated request, which makes it mean what
//! the listing says it means.
//!
//! It is **coalesced** at
//! [`TOUCH_INTERVAL`](crate::auth::scheme::TOUCH_INTERVAL) — one write per minute per session
//! at most — because a touch on every request is a store write on every request. So a device
//! that is actively syncing can read as up to a minute idle. That is deliberate and it is the
//! right trade for a screen a human opens occasionally; the reasoning is in
//! [`crate::auth::scheme`].

use capsule_i18n::error_codes;
use kynos::prelude::*;
use kynos::response::status::NoContent;
use serde::{Deserialize, Serialize};

use crate::auth::{AccessToken, AuthContext};
use crate::store::{SessionId, SessionRecord, UserId};

/// The devices surface: which sessions an account has, and ending one.
#[derive(Tag)]
#[tag(
    name = "devices",
    description = "The session ledger: listing an account's devices and revoking one."
)]
pub struct SessionsTag;

/// One live session.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SessionView {
    /// The session's identifier — what a revoke names.
    pub session_id: String,
    /// When this session *record* was minted, RFC 3339.
    ///
    /// A refresh rotates the session, so after one this is the rotation time and not the
    /// sign-in. `authenticated_at` is the field that answers "when did you last sign in".
    pub created_at: String,
    /// When the user last proved a credential on this session's lineage, RFC 3339.
    ///
    /// Carried forward across refreshes, so it is the one timestamp here that means what a
    /// user reading a devices list expects "signed in" to mean. It is also what the
    /// cross-device add's freshness gate reads (`S-C7`), so a client can show why an add is
    /// about to ask for a password again.
    pub authenticated_at: String,
    /// When it was last seen, RFC 3339.
    ///
    /// Equal to `created_at` until `S-C48` puts the session ledger on the request path. A
    /// client must not label this "last used" before then.
    pub last_active_at: String,
    /// The `User-Agent` the opening ceremony carried, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// The address the opening ceremony came from, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip_address: Option<String>,
    /// The advisory cohort this session asserted, if any. Grouping only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cohort_hash: Option<String>,
    /// The directory device the client claimed to be (`S-N3`), if any.
    ///
    /// A different identifier space from `cohort_hash`: this names one directory device, the
    /// cohort groups re-enrollments of one physical device. Both are client-asserted; neither
    /// gates anything.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_id: Option<String>,
    /// Whether this is the session making the request.
    ///
    /// So a client can label "this device" without comparing tokens it should not be handling,
    /// and so revoking the current session is a deliberate act rather than an accident.
    pub current: bool,
}

/// One cohort this account has been seen under.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct CohortView {
    /// The advisory hash.
    pub cohort_hash: String,
    /// The first time this account was seen under it, RFC 3339.
    ///
    /// What lets a client say *"a device you've used before"* about a session whose own
    /// `device_id` is new — which is the entire reason the map is durable.
    pub first_seen: String,
    /// The most recent time, RFC 3339.
    pub last_seen: String,
}

/// The session ledger.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DevicesResponse {
    /// Every live session, oldest first.
    pub sessions: Vec<SessionView>,
    /// Every cohort this account has ever been seen under, oldest first sighting first.
    ///
    /// Served **beside** the sessions rather than folded into them, because a cohort outlives
    /// the sessions that carried it: a reinstall's new session groups with a cohort whose other
    /// sessions expired months ago, and a client that only had per-session cohorts could not
    /// say "you have used this device before".
    pub cohorts: Vec<CohortView>,
}

/// The session to revoke.
#[derive(PathParams, Schema)]
pub struct SessionPath {
    /// The session's identifier.
    pub session_id: String,
}

/// Why the ledger could not be read.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ListDevicesRejection {
    /// A store could not answer.
    #[error("the session ledger could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a session was not revoked.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum RevokeSessionRejection {
    /// No such live session for this account.
    ///
    /// One answer for "never existed", "already closed", "expired" and "somebody else's". The
    /// last is the one that matters: distinguishing it would turn this into an oracle over
    /// whether a guessed session id belongs to another account.
    #[error("no such session")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The store could not answer.
    #[error("the session could not be revoked")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// List the caller's live sessions and the cohorts they group under.
///
/// Scoped by credential with no path parameter, for the same reason the escrow is: the only
/// account entitled to a session ledger is its own, and making that structural beats enforcing
/// it.
#[kynos::get("/v1/auth/devices", operation_id = "list_devices", tag = SessionsTag)]
pub async fn list_devices(
    Inject(auth): Inject<AuthContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<Json<DevicesResponse>, ListDevicesRejection> {
    let user = UserId::new(credential.user.as_str());

    let sessions = auth
        .sessions()
        .sessions_for_user(&user)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the session store could not list");
            ListDevicesRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;

    let cohorts = auth
        .cohorts()
        .cohorts_for_user(&user)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the cohort map could not be read");
            ListDevicesRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;

    tracing::debug!(
        %user,
        sessions = sessions.len(),
        cohorts = cohorts.len(),
        "served a session ledger"
    );
    Ok(Json(DevicesResponse {
        sessions: sessions
            .into_iter()
            .map(|record| view(record, &credential.session))
            .collect(),
        cohorts: cohorts
            .into_iter()
            .map(|record| CohortView {
                cohort_hash: record.cohort_hash,
                first_seen: record.first_seen.to_string(),
                last_seen: record.last_seen.to_string(),
            })
            .collect(),
    }))
}

/// Revoke one of the caller's sessions.
///
/// Any live token may do this, including for the session making the request — signing this
/// device out is a legitimate thing to ask for, and refusing it would only push a client into
/// calling `logout` and hoping the two behave the same.
///
/// **Only the caller's own sessions.** The ownership check is against the record the store
/// returns rather than against a separate lookup, so there is no window between checking and
/// closing, and a session id belonging to another account answers exactly as an unknown one
/// does.
#[kynos::delete(
    "/v1/auth/devices/{session_id}",
    operation_id = "revoke_session",
    tag = SessionsTag
)]
pub async fn revoke_session(
    Inject(auth): Inject<AuthContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<SessionPath>,
) -> Result<NoContent, RevokeSessionRejection> {
    let user = UserId::new(credential.user.as_str());
    let target = SessionId::new(&path.session_id);

    // Read first, so somebody else's session is refused without being closed. The alternative —
    // close and check afterwards — would let a caller end an arbitrary account's session and
    // then be told they were not allowed to.
    let held = auth
        .sessions()
        .read_session(&target)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the session store could not be read for a revoke");
            RevokeSessionRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;

    let Some(held) = held.filter(|record| record.user_id == user) else {
        tracing::info!(%user, session_id = %target, "a session revoke found nothing of this account's");
        return Err(RevokeSessionRejection::NotFound {
            code: error_codes::AUTH_SESSION_NOT_FOUND,
        });
    };

    auth.sessions()
        .close_session(&held.session_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, %user, "the session store could not close a session");
            RevokeSessionRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;

    tracing::info!(
        %user,
        session_id = %held.session_id,
        current = held.session_id == credential.session,
        "a session was revoked from the devices surface"
    );
    Ok(NoContent)
}

/// The wire projection of one session record.
fn view(record: SessionRecord, current: &SessionId) -> SessionView {
    SessionView {
        current: record.session_id == *current,
        session_id: record.session_id.to_string(),
        created_at: record.created_at.to_string(),
        authenticated_at: record.authenticated_at.to_string(),
        last_active_at: record.last_active_at.to_string(),
        user_agent: record.user_agent,
        ip_address: record.ip_address,
        cohort_hash: record.cohort_hash,
        device_id: record.device_id.map(|id| id.to_string()),
    }
}
