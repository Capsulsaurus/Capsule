//! The second factor's surface (slice `S-C55`) — enrolling an authenticator, confirming it,
//! removing it, and completing a sign-in with it.
//!
//! [`crate::auth::totp`] owns the algorithm, the replay ledger and the reasons each is where it
//! is — including the defect this slice fixes: the retired surface had all four operations and
//! its login **never issued a challenge**, so a confirmed second factor gated nothing.
//!
//! # The shape of a sign-in with a second factor
//!
//! `POST /v1/auth/login` answers **`202 Accepted`** with a short-lived challenge instead of
//! `200` with a token pair. `202` is the honest status: the credentials were accepted and the
//! request is not complete. No session is opened, no cohort is recorded and no refresh token is
//! minted, because none of those may exist for an authentication that has not finished.
//!
//! The client then posts the challenge and a code to `POST /v1/auth/login/verify-totp`, and
//! *that* is where the session is opened. A client which cannot tell `200` from `202` gets a
//! body with no `access_token` in it and fails loudly, which is the failure mode to want.
//!
//! # `S-C28` audit
//!
//! | Status | Verdict |
//! | --- | --- |
//! | enroll `200` | a secret, and the `otpauth://` URI an app scans |
//! | enroll `409 error.auth.totp_already_active` | there is a **confirmed** factor already. A pending one is replaced silently — nothing is protecting an unconfirmed secret |
//! | confirm `204` | the factor is on |
//! | confirm `409 error.auth.totp_not_pending` | nothing is waiting to be confirmed |
//! | disable `204` | the factor is off |
//! | disable `409 error.auth.totp_not_enrolled` | there was nothing to remove |
//! | confirm / disable `403 error.auth.totp_invalid_code` | the code did not verify, or had been used. **`403` and not `401`** — the caller is authenticated, and a `401` would send a client to a sign-in its live session does not need |
//! | verify-login `200` | the pair, at last |
//! | verify-login `401 error.auth.totp_challenge_invalid` | the half-finished sign-in did not verify, expired, or was another kind of token |
//! | verify-login `401 error.auth.totp_invalid_code` | the code did not verify or had been used |
//! | verify-login `429 error.auth.rate_limited` | five attempts per challenge (`S-C32`) |
//! | `500 error.auth.unavailable` | a collaborator could not answer |
//!
//! **There is no `enabled` field on any response and no "is TOTP on" endpoint.** A client learns
//! the state by acting: an enroll that answers `409` says it is on, and a sign-in that answers
//! `202` says the same. An endpoint that reported it would be one more thing to keep in step
//! with the store, and an unauthenticated one would be an oracle over which accounts are worth
//! phishing.
//!
//! # What this surface deliberately does not gate
//!
//! `POST /v1/auth/reauthenticate` (`S-C7`) still takes a password alone. The second factor
//! guards *becoming* a session; re-authentication is performed **by** a session that already
//! exists, and demanding a code there would protect nothing an attacker holding that session has
//! not already got past. Recorded here rather than left to be noticed.

use std::fmt;

use capsule_i18n::error_codes;
use kynos::prelude::*;
use kynos::response::status::NoContent;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};

use crate::auth::{
    AccessToken, ActivateOutcome, AuthContext, BeginOutcome, ConsumeOutcome, DirectoryError,
    EnrollmentState, TotpCodes, TotpContext, TotpEnrollment,
};
use crate::counter::{CounterContext, CounterKey, budgets};
use crate::routes::auth::TokenResponse;
use crate::store::UserId;

/// The second factor of the local auth path.
#[derive(Tag)]
#[tag(
    name = "totp",
    description = "Enrolling a time-based one-time-password authenticator, and signing in with it."
)]
pub struct TotpTag;

// ===========================================================================================
// Wire types
// ===========================================================================================

/// A freshly issued, unconfirmed enrollment.
#[derive(Schema, Serialize, Deserialize, Clone)]
pub struct EnrollmentResponse {
    /// The `otpauth://` URI an authenticator app scans.
    ///
    /// It carries the shared secret, so it is a credential: served once, over the authenticated
    /// channel, and never fetchable again. Losing it before confirming means enrolling again,
    /// which is why a *pending* enrollment is replaceable without ceremony.
    pub provisioning_uri: String,
}

impl fmt::Debug for EnrollmentResponse {
    /// Hand-written. The URI embeds the shared secret; a derived `Debug` publishes it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnrollmentResponse")
            .field("provisioning_uri", &"<redacted>")
            .finish()
    }
}

/// A six-digit code, and nothing else.
#[derive(Schema, Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct CodeRequest {
    /// The code the authenticator app is showing.
    pub totp_code: String,
}

/// A half-finished sign-in.
///
/// `Debug` is hand-written: the token is a credential, even though it authenticates nothing on
/// its own.
#[derive(Schema, Serialize, Deserialize, Clone)]
pub struct SecondFactorChallenge {
    /// The token to present alongside the code.
    pub mfa_token: String,

    /// The **absolute** Unix-seconds instant the challenge stops being honoured.
    ///
    /// Absolute rather than a duration, matching `TokenResponse::expires_by`, so a client has
    /// one convention rather than two.
    pub expires_by: u64,
}

impl fmt::Debug for SecondFactorChallenge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecondFactorChallenge")
            .field("mfa_token", &"<redacted>")
            .field("expires_by", &self.expires_by)
            .finish()
    }
}

/// Completing a sign-in with a second factor.
///
/// It carries the same two advisory identifiers `LoginRequest` does, because *this* is the
/// request that opens the session: without them a TOTP sign-in would land in the devices view as
/// an unknown, ungrouped device (`S-N3`).
#[derive(Schema, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifyLoginRequest {
    /// The challenge issued by `POST /v1/auth/login`.
    pub mfa_token: String,

    /// The code the authenticator app is showing.
    pub totp_code: String,

    /// An advisory device-cohort hash grouping one physical device's re-enrollments (`S-C13`).
    pub cohort_hash: Option<String>,

    /// The directory device the client claims to be (`S-N3`), as a UUID.
    pub device_id: Option<String>,
}

impl fmt::Debug for VerifyLoginRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifyLoginRequest")
            .field("mfa_token", &"<redacted>")
            .field("totp_code", &"<redacted>")
            .field("cohort_hash", &self.cohort_hash)
            .field("device_id", &self.device_id)
            .finish()
    }
}

// ===========================================================================================
// Rejections
// ===========================================================================================

/// Why an enrollment was not started.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum EnrollRejection {
    /// There is already a confirmed second factor.
    #[error("two-factor authentication is already active on this account")]
    #[problem(status = 409, title = "Already active")]
    AlreadyActive {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the enrollment could not be started")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why an enrollment was not confirmed.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ConfirmRejection {
    /// The code did not verify, or had already been used.
    ///
    /// One answer for both: telling a caller a code was correct but replayed confirms they had
    /// the right code.
    #[error("that code is not right")]
    #[problem(status = 403, title = "Invalid code")]
    InvalidCode {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Nothing is waiting to be confirmed.
    #[error("there is no pending two-factor enrollment for this account")]
    #[problem(status = 409, title = "Nothing pending")]
    NotPending {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the enrollment could not be confirmed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a second factor was not removed.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum DisableRejection {
    /// The code did not verify, or had already been used.
    #[error("that code is not right")]
    #[problem(status = 403, title = "Invalid code")]
    InvalidCode {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// There is no confirmed second factor.
    #[error("two-factor authentication is not active on this account")]
    #[problem(status = 409, title = "Not enrolled")]
    NotEnrolled {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the second factor could not be removed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Why a sign-in was not completed.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum VerifyLoginRejection {
    /// The half-finished sign-in did not verify, expired, or was another kind of token.
    #[error("that sign-in has expired; enter your password again")]
    #[problem(status = 401, title = "Challenge expired")]
    ChallengeInvalid {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The code did not verify, or had already been used.
    #[error("that code is not right")]
    #[problem(status = 401, title = "Invalid code")]
    InvalidCode {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// Too many codes have been tried against this one challenge.
    #[error("too many attempts; enter your password again")]
    #[problem(status = 429, title = "Too many attempts")]
    TooManyAttempts {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the sign-in could not be completed")]
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

/// Start enrolling an authenticator.
///
/// Answers the `otpauth://` URI the app scans. Nothing is gated yet: until a code confirms the
/// secret, sign-in is unchanged — which is what stops a mis-scanned QR code from locking
/// somebody out of their own account.
#[kynos::post("/v1/auth/totp/enroll", operation_id = "totp_enroll", tag = TotpTag)]
pub async fn totp_enroll(
    Inject(auth): Inject<AuthContext>,
    Inject(totp): Inject<TotpContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<Json<EnrollmentResponse>, EnrollRejection> {
    let user = UserId::new(credential.user.as_str());
    let secret = TotpCodes::generate_secret();
    let now = auth.clock().now();

    // The URI is built *before* the write, so a secret this server cannot turn into a usable
    // authenticator is never stored. The account name is the account id rather than the email:
    // the label an app shows is not worth a database round trip on every enroll, and an id is
    // stable across a change of address the profile surface does not offer anyway.
    let provisioning_uri = totp
        .codes()
        .provisioning_uri(&secret, user.as_str())
        .map_err(|error| {
            tracing::error!(%error, %user, "a generated second-factor secret is unusable");
            EnrollRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;

    match totp
        .enrollments()
        .begin(TotpEnrollment {
            user_id: user.clone(),
            secret,
            state: EnrollmentState::Pending,
            last_step: None,
            enrolled_at: now,
            activated_at: None,
        })
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the second-factor store could not start an enrollment");
            EnrollRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })? {
        BeginOutcome::Started => {}
        BeginOutcome::AlreadyActive => {
            tracing::info!(%user, "an enrollment was refused: a second factor is already active");
            return Err(EnrollRejection::AlreadyActive {
                code: error_codes::AUTH_TOTP_ALREADY_ACTIVE,
            });
        }
    }

    tracing::info!(%user, "a second-factor enrollment was started");
    Ok(Json(EnrollmentResponse { provisioning_uri }))
}

/// Confirm an enrollment with a live code.
///
/// The confirming code is **spent**: its step goes straight into the replay ledger, so it cannot
/// also complete a sign-in a moment later. That is the one place the ledger's first entry comes
/// from, and skipping it would leave the newest code in the account's history unused.
#[kynos::post(
    "/v1/auth/totp/verify-enrollment",
    operation_id = "totp_verify_enrollment",
    tag = TotpTag
)]
pub async fn totp_verify_enrollment(
    Inject(auth): Inject<AuthContext>,
    Inject(totp): Inject<TotpContext>,
    Auth(credential): Auth<AccessToken>,
    Json(request): Json<CodeRequest>,
) -> Result<NoContent, ConfirmRejection> {
    let user = UserId::new(credential.user.as_str());
    let now = auth.clock().now();

    let held = totp
        .enrollments()
        .read(&user)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the second-factor store could not answer");
            ConfirmRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?
        .filter(|held| held.state == EnrollmentState::Pending)
        .ok_or_else(|| {
            tracing::info!(%user, "a confirmation was refused: nothing is pending");
            ConfirmRejection::NotPending {
                code: error_codes::AUTH_TOTP_NOT_PENDING,
            }
        })?;

    let step = totp
        .codes()
        .verify(&held.secret, request.totp_code.trim(), now)
        .map_err(|error| {
            // A stored secret this server cannot use is *its* fault, not the caller's, and must
            // not read as "your code is wrong" — that sends somebody round a loop that cannot
            // succeed.
            tracing::error!(%error, %user, "a stored second-factor secret is unusable");
            ConfirmRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?
        .ok_or_else(|| {
            tracing::info!(%user, "a confirmation was refused: the code did not verify");
            ConfirmRejection::InvalidCode {
                code: error_codes::AUTH_TOTP_INVALID_CODE,
            }
        })?;

    match totp
        .enrollments()
        .activate(&user, step, now)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the second-factor store could not activate");
            ConfirmRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })? {
        ActivateOutcome::Activated => {}
        ActivateOutcome::NotPending => {
            // The enrollment was confirmed or abandoned between the read and this write.
            tracing::info!(%user, "a confirmation lost a race: nothing was pending by the write");
            return Err(ConfirmRejection::NotPending {
                code: error_codes::AUTH_TOTP_NOT_PENDING,
            });
        }
    }

    tracing::info!(%user, "a second factor is now active");
    Ok(NoContent)
}

/// Remove the second factor, on presentation of a live code.
///
/// **A session is not enough.** The whole point of the factor is that a stolen access token is
/// insufficient, and a disable that took only a token would let the token turn off the control
/// that makes it insufficient.
#[kynos::post("/v1/auth/totp/disable", operation_id = "totp_disable", tag = TotpTag)]
pub async fn totp_disable(
    Inject(auth): Inject<AuthContext>,
    Inject(totp): Inject<TotpContext>,
    Auth(credential): Auth<AccessToken>,
    Json(request): Json<CodeRequest>,
) -> Result<NoContent, DisableRejection> {
    let user = UserId::new(credential.user.as_str());
    let now = auth.clock().now();

    let held = totp
        .enrollments()
        .read(&user)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the second-factor store could not answer");
            DisableRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?
        .filter(|held| held.state == EnrollmentState::Active)
        .ok_or_else(|| {
            tracing::info!(%user, "a disable was refused: no active second factor");
            DisableRejection::NotEnrolled {
                code: error_codes::AUTH_TOTP_NOT_ENROLLED,
            }
        })?;

    let step = totp
        .codes()
        .verify(&held.secret, request.totp_code.trim(), now)
        .map_err(|error| {
            tracing::error!(%error, %user, "a stored second-factor secret is unusable");
            DisableRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?
        .ok_or_else(|| {
            tracing::info!(%user, "a disable was refused: the code did not verify");
            DisableRejection::InvalidCode {
                code: error_codes::AUTH_TOTP_INVALID_CODE,
            }
        })?;

    // Consumed before the removal, so a replayed code cannot remove the factor — the ledger is
    // about to be deleted, and checking it afterwards would be checking nothing.
    match totp
        .enrollments()
        .consume(&user, step)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the second-factor store could not consume a step");
            DisableRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })? {
        ConsumeOutcome::Fresh => {}
        ConsumeOutcome::Replayed | ConsumeOutcome::NotEnrolled => {
            tracing::warn!(%user, "a disable was refused: the code had already been used");
            return Err(DisableRejection::InvalidCode {
                code: error_codes::AUTH_TOTP_INVALID_CODE,
            });
        }
    }

    let removed = totp
        .enrollments()
        .disable(&user)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the second-factor store could not remove an enrollment");
            DisableRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;

    if removed {
        tracing::info!(%user, "a second factor was removed");
    } else {
        // Removed by another device between the read and here. The caller's goal is met.
        tracing::info!(%user, "a second factor was already gone by the time it was removed");
    }
    Ok(NoContent)
}

/// Complete a sign-in with a code.
///
/// This is where the session is opened — not `POST /v1/auth/login`, which for an account with a
/// second factor opens nothing. The advisory `cohort_hash` and `device_id` ride *this* request
/// for the same reason: the session they describe is created here.
#[kynos::post(
    "/v1/auth/login/verify-totp",
    operation_id = "totp_verify_login",
    tag = TotpTag
)]
pub async fn totp_verify_login(
    Inject(auth): Inject<AuthContext>,
    Inject(totp): Inject<TotpContext>,
    Inject(counters): Inject<CounterContext>,
    Json(request): Json<VerifyLoginRequest>,
) -> Result<Json<TokenResponse>, VerifyLoginRejection> {
    let verified = auth
        .tokens()
        .verify_second_factor(&request.mfa_token)
        .map_err(|error| {
            tracing::info!(%error, "a second factor was refused: the challenge did not verify");
            VerifyLoginRejection::ChallengeInvalid {
                code: error_codes::AUTH_TOTP_CHALLENGE_INVALID,
            }
        })?;
    let user = verified.user;

    // Charged **before** the code is checked, and on every attempt whatever the outcome
    // (`S-C32`). Keyed on the challenge rather than the account: keying on the account would let
    // a stream of first-factor sign-ins from an attacker exhaust the budget of the person whose
    // password they do not have.
    let key = CounterKey::SecondFactor(verified.challenge.as_str().to_owned());
    let verdict = counters
        .hit(&key, budgets::SECOND_FACTOR)
        .await
        .map_err(|error| {
            // Fail closed. A limiter an attacker turns off by loading the counter store is not a
            // limiter — and the thing being guessed here is six digits.
            tracing::error!(%error, "the second-factor attempt counter could not be reached");
            VerifyLoginRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;
    if !verdict.admits() {
        tracing::warn!(%user, challenge_id = %verified.challenge, "a second factor was rate-limited");
        return Err(VerifyLoginRejection::TooManyAttempts {
            code: error_codes::AUTH_RATE_LIMITED,
        });
    }

    let held = totp
        .enrollments()
        .read(&user)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the second-factor store could not answer");
            VerifyLoginRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?
        .filter(|held| held.state == EnrollmentState::Active)
        .ok_or_else(|| {
            // The factor was removed from another device while this challenge was outstanding.
            // Answered as an expired challenge, which is what it is: the sign-in it belonged to
            // no longer describes the account, and the caller starts again — and the second
            // attempt will not ask for a code at all.
            tracing::info!(%user, "a second factor was refused: no active enrollment");
            VerifyLoginRejection::ChallengeInvalid {
                code: error_codes::AUTH_TOTP_CHALLENGE_INVALID,
            }
        })?;

    let now = auth.clock().now();
    let step = totp
        .codes()
        .verify(&held.secret, request.totp_code.trim(), now)
        .map_err(|error| {
            tracing::error!(%error, %user, "a stored second-factor secret is unusable");
            VerifyLoginRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?
        .ok_or_else(|| {
            tracing::info!(%user, "a second factor was refused: the code did not verify");
            VerifyLoginRejection::InvalidCode {
                code: error_codes::AUTH_TOTP_INVALID_CODE,
            }
        })?;

    // The replay ledger, and the reason a shoulder-surfed code is good for one sign-in rather
    // than for the ninety seconds it stays valid.
    match totp
        .enrollments()
        .consume(&user, step)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the second-factor store could not consume a step");
            VerifyLoginRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })? {
        ConsumeOutcome::Fresh => {}
        ConsumeOutcome::Replayed | ConsumeOutcome::NotEnrolled => {
            tracing::warn!(%user, "a second factor was refused: the code had already been used");
            return Err(VerifyLoginRejection::InvalidCode {
                code: error_codes::AUTH_TOTP_INVALID_CODE,
            });
        }
    }

    // The session at last, opened exactly as a password-only sign-in opens one.
    let issued = super::auth::open_session_for(
        &auth,
        &user,
        request.cohort_hash.as_deref(),
        request.device_id.as_deref(),
        now,
    )
    .await
    .map_err(|error| {
        tracing::error!(%error, %user, "a session could not be opened after a second factor");
        VerifyLoginRejection::Unavailable {
            code: error_codes::AUTH_UNAVAILABLE,
        }
    })?;

    // The challenge is finished with, so its attempt budget is released. A caller who signs in
    // correctly on the fifth try should not carry a spent counter into their next sign-in — and
    // the counter is keyed on a challenge that will never be presented again.
    if let Err(error) = counters.reset(&key).await {
        tracing::warn!(%error, "a spent second-factor counter could not be cleared");
    }

    tracing::info!(%user, "completed a sign-in with a second factor");
    Ok(Json(TokenResponse::from(issued)))
}
