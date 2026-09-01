//! The profile surface (slice `S-C54`) — what an account knows about itself, and how it changes
//! the password its sessions are opened with.
//!
//! [`crate::auth::profile`] owns the two ports, the nested-option discipline on a partial update
//! and the reasons the address cannot change here. This module is the wire shape and its
//! refusals.
//!
//! # Three operations, where Salvo had two
//!
//! The Salvo `POST /v1/auth/profile` was one handler that changed a display name, an email
//! address **and** a password, branching on which fields happened to be present. That is three
//! operations wearing one URL, and the failure it invites is the one it had: a caller sending a
//! new password with no current password got a silent no-op rather than a refusal, because the
//! `if let (Some(_), Some(_))` that gated the change had no `else`.
//!
//! Here the routine edit is `PATCH /v1/auth/profile` and the credential rotation is
//! `POST /v1/auth/password`. They have different methods, different bodies, different statuses
//! and different consequences — a password change ends every other session and a name change
//! does not — so they are different operations.
//!
//! The email address changes in neither, and that is a decision rather than an omission; see
//! [`crate::auth::profile`].
//!
//! # `S-C28` audit
//!
//! | Status | Verdict |
//! | --- | --- |
//! | read `200` | the caller's own profile |
//! | read / patch `404 error.auth.profile_not_found` | **reachable with a valid credential**: a session outlives the account row it names when the account is deleted mid-session. The server is working; the account is gone |
//! | patch `200` | the profile as it now stands, including for an empty update |
//! | patch `400 error.auth.profile_invalid` | past the display-name ceiling, or carrying control characters |
//! | change `204` | the password is replaced and every other session is closed |
//! | change `400 error.auth.password_invalid` | the new password is under the floor, or is the one already in use |
//! | change `403 error.auth.current_password_invalid` | the current password did not match. **`403` and not `401`**: the caller *is* authenticated — that is how they reached this operation — and answering `401` would tell a client to sign in again when its session is fine |
//! | change `423 error.auth.account_locked` | the lockout applies here exactly as it applies to a sign-in, because the same directory method decides both |
//! | `401` / `403` on the credential | the framework's, through `Auth` |
//! | `500 error.auth.unavailable` | a collaborator could not answer |
//!
//! No `409` anywhere. Nothing on this surface has a uniqueness constraint left to violate once
//! the address is immutable.

use std::fmt;

use capsule_i18n::error_codes;
use kynos::prelude::*;
use kynos::response::status::NoContent;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};

use crate::auth::{
    AccessToken, AuthContext, Authentication, DirectoryError, MIN_PASSWORD_LENGTH,
    MalformedProfile, PasswordChanged, ProfileRecord, ProfileUpdate, admissible_display_name,
};
use crate::store::UserId;

/// The account's own facts, and the credential it opens sessions with.
#[derive(Tag)]
#[tag(
    name = "profile",
    description = "What an account knows about itself, and changing its password."
)]
pub struct ProfileTag;

// ===========================================================================================
// Wire types
// ===========================================================================================

/// An account's profile as it is served.
///
/// `Deserialize` is derived so the suite reads it back through the same type the server wrote —
/// a test pulling `display_name` out of a `serde_json::Value` would still pass if the field were
/// renamed on the way out.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ProfileResponse {
    /// The account identifier every manifest and every session names.
    pub user_id: String,

    /// The address this account signs in with.
    ///
    /// Read-only on this surface. Changing it needs proof that the caller controls the new
    /// address, and this server has no way to obtain one; see [`crate::auth::profile`].
    pub email: String,

    /// The name the account chose to be shown as, if it chose one.
    ///
    /// Absent rather than `null` when unset, so a client's "has a name" test is a key test.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,

    /// When the account was created, RFC 3339.
    pub created_at: String,
}

impl From<ProfileRecord> for ProfileResponse {
    fn from(record: ProfileRecord) -> Self {
        Self {
            user_id: record.user_id.as_str().to_owned(),
            email: record.email,
            display_name: record.display_name,
            created_at: record.created_at.to_string(),
        }
    }
}

/// A partial edit of the caller's profile.
///
/// `display_name` is a **doubly** optional field on the wire, and the two levels mean different
/// things: an absent key leaves the name alone, and an explicit `null` clears it. That is what
/// `#[serde(default, deserialize_with = …)]` over an `Option<Option<String>>` buys, and it is
/// the whole reason this body is not `deny_unknown_fields`-plus-a-flat-option: a flat one cannot
/// tell "I did not mention the name" from "remove the name", so every partial update would wipe
/// a field the caller never sent.
#[derive(Schema, Deserialize, Debug, Default)]
#[serde(deny_unknown_fields)]
pub struct UpdateProfileRequest {
    /// The display name to set, clear (`null`), or leave alone (absent).
    #[serde(default, deserialize_with = "double_option")]
    pub display_name: Option<Option<String>>,
}

/// Deserialize a present-`null` into `Some(None)` and an absent key into `None`.
///
/// serde collapses both onto `None` for a plain `Option<Option<T>>` unless the field is read
/// through a deserializer that is only called when the key is present. `#[serde(default)]`
/// supplies the absent case; this supplies the other.
fn double_option<'de, D>(deserializer: D) -> Result<Option<Option<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(Some)
}

/// The two passwords a rotation needs.
///
/// `Debug` is hand-written for the reason `routes::auth`'s bodies are: a derived one would print
/// both credentials into any log line that formatted the request.
#[derive(Schema, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangePasswordRequest {
    /// The password currently in use, which authorizes the change.
    ///
    /// Verified through the same directory method a sign-in uses, so a locked account is locked
    /// here too.
    pub current_password: String,

    /// The password to replace it with.
    pub new_password: String,
}

impl fmt::Debug for ChangePasswordRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ChangePasswordRequest")
            .field("current_password", &"<redacted>")
            .field("new_password", &"<redacted>")
            .finish()
    }
}

// ===========================================================================================
// Rejections
// ===========================================================================================

/// Why a profile was not returned.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ProfileRejection {
    /// The credential is valid and names an account the directory does not hold.
    #[error("this account no longer exists")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the profile could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a profile was not changed.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum UpdateProfileRejection {
    /// The submitted profile is not one the server will store.
    #[error("{detail}")]
    #[problem(status = 400, title = "Invalid profile")]
    Invalid {
        /// What was wrong, in English. The client localizes `code`, not this.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The credential is valid and names an account the directory does not hold.
    #[error("this account no longer exists")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the profile could not be updated")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a password was not replaced.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ChangePasswordRejection {
    /// The proposed password is not one this server will accept.
    #[error("{detail}")]
    #[problem(status = 400, title = "Invalid password")]
    Invalid {
        /// What was wrong, in English.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The current password did not match.
    ///
    /// `403` rather than `401`, because the caller is authenticated — that is how they reached
    /// this operation at all — and a `401` would send a client to a sign-in it does not need.
    #[error("the current password is not correct")]
    #[problem(status = 403, title = "Forbidden")]
    CurrentPasswordInvalid {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The account is refusing credential attempts.
    #[error("the account is locked after too many failed attempts")]
    #[problem(status = 423, title = "Account locked")]
    AccountLocked {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The credential is valid and names an account the directory does not hold.
    #[error("this account no longer exists")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the password could not be changed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

// ===========================================================================================
// Operations
// ===========================================================================================

/// The caller's own profile.
///
/// There is no `{user_id}` segment, for the reason the escrow surface has none: the account
/// comes from the credential, so reading somebody else's profile is not a forbidden request but
/// an unrepresentable one. A directory of *other* people's public facts already exists and is a
/// different surface — `GET /v1/auth/devices/directory/{user_id}` — which publishes keys and
/// nothing else.
#[kynos::get("/v1/auth/profile", operation_id = "get_profile", tag = ProfileTag)]
pub async fn get_profile(
    Inject(auth): Inject<AuthContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<Json<ProfileResponse>, ProfileRejection> {
    let user = UserId::new(credential.user.as_str());
    let record = auth
        .profiles()
        .read(&user)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the account directory could not read a profile");
            ProfileRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?
        .ok_or_else(|| {
            // A live session naming a deleted account. The server is fine; the account is not.
            tracing::warn!(%user, "a live session names an account that no longer exists");
            ProfileRejection::NotFound {
                code: error_codes::AUTH_PROFILE_NOT_FOUND,
            }
        })?;

    Ok(Json(ProfileResponse::from(record)))
}

/// Edit the caller's own profile.
///
/// `PATCH`, because the body is a partial: what it does not mention, it does not change. An
/// empty body is a valid request and answers `200` with the profile unchanged — a client that
/// sent nothing asked for nothing, and refusing it would make "save" fail on a form nobody
/// edited.
#[kynos::patch("/v1/auth/profile", operation_id = "update_profile", tag = ProfileTag)]
pub async fn update_profile(
    Inject(auth): Inject<AuthContext>,
    Auth(credential): Auth<AccessToken>,
    Json(request): Json<UpdateProfileRequest>,
) -> Result<Json<ProfileResponse>, UpdateProfileRejection> {
    let user = UserId::new(credential.user.as_str());

    // Normalized before a store is reached, so a malformed name costs no round trip.
    let display_name = match request.display_name {
        None => None,
        Some(None) => Some(None),
        Some(Some(name)) => Some(admissible_display_name(&name).map_err(|error| {
            tracing::info!(%user, %error, "a profile edit was refused");
            UpdateProfileRejection::invalid(&error)
        })?),
    };
    let update = ProfileUpdate { display_name };

    let record = auth
        .profiles()
        .update(&user, &update)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the account directory could not update a profile");
            UpdateProfileRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?
        .ok_or_else(|| {
            tracing::warn!(%user, "a live session names an account that no longer exists");
            UpdateProfileRejection::NotFound {
                code: error_codes::AUTH_PROFILE_NOT_FOUND,
            }
        })?;

    if !update.is_empty() {
        tracing::info!(%user, "a profile was updated");
    }
    Ok(Json(ProfileResponse::from(record)))
}

/// Replace the password this account's sessions are opened with.
///
/// # Every *other* session ends
///
/// A password change whose point is that a credential has leaked would be worthless if the
/// sessions opened with the leaked credential kept working. So the change closes every session
/// of the account — and then re-opens the caller's own, **under its own session id**, so the
/// person doing the rotation is not signed out of the device they are doing it on while
/// everybody else is.
///
/// Re-opening the same id rather than minting a new one is what lets this answer `204` with no
/// body: the caller's existing token pair keeps working, because the session it names is still
/// there. Returning a fresh pair was considered and rejected — it would make this a second token
/// mint with none of `POST /v1/auth/refresh`'s rotation discipline, for no gain.
///
/// The re-opened record's `authenticated_at` is **now**, and that is not bookkeeping: presenting
/// the current password *is* a credential presentation, so a freshness gate (`S-C7`) measuring
/// from anything earlier would be measuring from the wrong moment.
///
/// # Why the order is verify, write, revoke
///
/// Verification first, because a wrong current password must change nothing. The write next,
/// because a revocation that ran before it would sign everybody out and then fail. The
/// revocation last, and its failure is **logged and not returned**: the password is already
/// changed, so answering `500` would tell the caller the rotation did not happen when it did,
/// and they would try again with a current password that is no longer current.
#[kynos::post("/v1/auth/password", operation_id = "change_password", tag = ProfileTag)]
pub async fn change_password(
    Inject(auth): Inject<AuthContext>,
    Auth(credential): Auth<AccessToken>,
    Json(request): Json<ChangePasswordRequest>,
) -> Result<NoContent, ChangePasswordRejection> {
    let user = UserId::new(credential.user.as_str());

    if request.new_password.chars().count() < MIN_PASSWORD_LENGTH {
        return Err(ChangePasswordRejection::invalid(format!(
            "a password must be at least {MIN_PASSWORD_LENGTH} characters"
        )));
    }
    if request.new_password == request.current_password {
        // Not pedantry: a "change" that changes nothing leaves the caller believing a leaked
        // credential has been rotated when it has not.
        return Err(ChangePasswordRejection::invalid(
            "the new password is the one already in use",
        ));
    }

    // The same method a sign-in takes, so the lockout and the timing equalization apply here
    // too. The account comes from the credential and never from the request, which is what
    // stops this being a way to test another account's password.
    match auth
        .accounts()
        .authenticate_user(&user, &request.current_password)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the account directory could not verify a password");
            ChangePasswordRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })? {
        Authentication::Granted(_) => {}
        Authentication::Locked => {
            tracing::warn!(%user, "a password change was refused: the account is locked");
            return Err(ChangePasswordRejection::AccountLocked {
                code: error_codes::AUTH_ACCOUNT_LOCKED,
            });
        }
        Authentication::Refused => {
            tracing::info!(%user, "a password change was refused: the current password is wrong");
            return Err(ChangePasswordRejection::CurrentPasswordInvalid {
                code: error_codes::AUTH_CURRENT_PASSWORD_INVALID,
            });
        }
    }

    let now = auth.clock().now();
    match auth
        .passwords()
        .set_password(&user, &request.new_password, now)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the account directory could not change a password");
            ChangePasswordRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })? {
        PasswordChanged::Yes => {}
        PasswordChanged::NoSuchAccount => {
            // The directory authenticated the account a moment ago, so this is a deletion that
            // landed between the two calls rather than a wrong credential.
            tracing::warn!(%user, "an account was deleted during its own password change");
            return Err(ChangePasswordRejection::NotFound {
                code: error_codes::AUTH_PROFILE_NOT_FOUND,
            });
        }
    }

    // Everything, including this session — then this session again, under the same id. See the
    // operation docs for why a failure here is logged rather than returned.
    match auth.sessions().close_all_for_user(&user).await {
        Ok(closed) => {
            let mut restored = false;
            if let Some(mine) = closed
                .iter()
                .find(|record| record.session_id == credential.session)
            {
                let mut record = mine.clone();
                // A credential was just presented, so this is the instant a freshness gate
                // measures from.
                record.authenticated_at = now;
                record.last_active_at = now;
                match auth.sessions().open_session(record).await {
                    Ok(()) => restored = true,
                    Err(error) => tracing::error!(
                        %error,
                        %user,
                        session_id = %credential.session,
                        "a password was changed but this session could not be re-opened; \
                         the caller has been signed out along with everybody else"
                    ),
                }
            }
            tracing::info!(
                %user,
                sessions = closed.len(),
                restored,
                "a password was changed; every other session of the account was closed"
            );
        }
        Err(error) => {
            tracing::error!(
                %error,
                %user,
                "a password was changed but its sessions could not be closed; \
                 the old credential's sessions are still live"
            );
        }
    }

    Ok(NoContent)
}

impl UpdateProfileRejection {
    /// The submitted profile is not storable.
    fn invalid(error: &MalformedProfile) -> Self {
        Self::Invalid {
            detail: error.to_string(),
            code: error_codes::AUTH_PROFILE_INVALID,
        }
    }
}

impl ChangePasswordRejection {
    /// The proposed password is not acceptable.
    fn invalid(detail: impl Into<String>) -> Self {
        Self::Invalid {
            detail: detail.into(),
            code: error_codes::AUTH_PASSWORD_INVALID,
        }
    }
}
