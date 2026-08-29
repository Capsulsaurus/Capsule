//! `POST /v1/auth/login`, `POST /v1/auth/refresh`, `POST /v1/auth/logout`.
//!
//! The first surface of the Stage 6 rebuild with state behind it. What it is a port *of* is the
//! Salvo `capsule-api/auth/src/routes/auth.rs`; what it is not is a transcription of it.
//!
//! # The status audit (`S-C28`)
//!
//! `S-C28` found thirteen response variants across the Salvo surface that render a status
//! `capsule-sdk/openapi.json` never declares — `LoginResponses::undocumented()` returns
//! `[423, 429]`. Kynos makes that class of defect unrepresentable, because the status *is* the
//! return type and there is only one declaration. So each status was audited as its operation
//! was ported, and the verdict lives in the type:
//!
//! | Salvo | Verdict here |
//! | --- | --- |
//! | login `200` | kept — [`TokenResponse`] |
//! | login `400` "Bad request" | **deleted as unreachable.** `LoginResponses::BadRequest` was never constructed; the 400 a malformed body actually produces comes from the `Json` extractor, and Kynos declares it |
//! | login `401` | kept — [`LoginRejection::InvalidCredentials`], now carrying `error.auth.invalid_credentials` |
//! | login `423` | **kept and now documented.** Reachable: lockout is account state the directory owns, not a counter, so it ports. Carries the new `error.auth.account_locked` |
//! | login `429` | **deleted as unreachable.** Rate limiting is a *counter*, `S-C29` deliberately gave counters no port, and `S-C32` owns them. Nothing in this crate can produce a 429, so declaring one would fail `assert_declared_responses_covered`. See "What this port does not have" |
//! | login `500` | kept — [`LoginRejection::Unavailable`], JSON rather than the old debug-leaking `text/plain` |
//! | refresh `200` / `401` / `500` | all kept |
//! | logout `200` | **changed to `204`.** The Salvo success body was `{"error":"Logout successful"}` — a success rendered in the error envelope |
//! | logout `401` | kept, and now the framework's, with the `WWW-Authenticate` challenge the document declares |
//! | logout `500` | kept |
//! | — | **`403` added** to logout: a live refresh token presented as a bearer credential is valid but insufficient. Salvo answered 401 |
//!
//! Every status above is produced by a test. That is not a stylistic claim: the document is
//! generated from these types, and `assert_declared_responses_covered` fails on any response the
//! document promises and no test has made the server send.
//!
//! # What this port does not have
//!
//! **Rate limiting.** The Salvo login is limited to ten attempts per IP per minute, and the port
//! has none, because the counter port that would back it does not exist — `S-C29` excluded
//! counters on the grounds that a lost record and a lost increment are different contracts, and
//! `S-C32` owns the replacement. Inventing one here would be doing that slice's work behind its
//! back. This is a real gap against Salvo parity and is reported as one, not papered over with
//! a 429 the server cannot send.
//!
//! **`user_agent` and `ip_address` on the session record.** Both are always `None`, exactly as
//! in Salvo, which never filled them either. They are read by the devices listing, which is not
//! part of this surface; filling them belongs with the operation that displays them.

use std::fmt;

use capsule_i18n::error_codes;
use kynos::prelude::*;
use kynos::response::status::NoContent;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::{
    AccessToken, AuthContext, Authentication, DirectoryError, IssuedTokens, TokenKind,
    VerifiedToken,
};
use crate::store::{SessionId, SessionRecord, StoreError};

/// The operations that establish and end a session.
#[derive(Tag)]
#[tag(
    name = "auth",
    description = "Establishing, rotating and ending a session."
)]
pub struct AuthTag;

/// The longest advisory cohort hash the server will store.
///
/// The value is not load-bearing — nothing authorizes on a cohort — so an over-long one is
/// dropped rather than refused. Matches the Salvo `MAX_COHORT_HASH_LEN`.
const MAX_COHORT_HASH_LEN: usize = 128;

// ===========================================================================================
// Wire types
// ===========================================================================================

/// Credentials, plus the two advisory identifiers a client may volunteer.
///
/// `Debug` is hand-written. A derived one would print the password into any log line, panic
/// message or `tracing` field that formatted the request — which is the single worst thing this
/// file could do, and is one `#[derive(Debug)]` away at all times.
/// No `#[schema(min_length = ...)]` on either credential, deliberately. Kynos 0.1.0 publishes a
/// string constraint into the document but does not enforce it on the request path — an empty
/// password reaches the handler — so declaring one would put a promise in the contract that the
/// server does not keep, which is the exact class of drift this rebuild exists to remove. Length
/// is a body-size concern and belongs to a limits middleware; it is recorded as owed rather than
/// asserted here.
#[derive(Schema, Deserialize)]
pub struct LoginRequest {
    /// The account's email address.
    pub email: String,

    /// The account's password.
    ///
    /// Verified by the account directory and never retained, logged, or echoed.
    pub password: String,

    /// An advisory device-cohort hash grouping one physical device's re-enrollments
    /// (slice `S-C13`).
    ///
    /// Legibility metadata only: no authorization path reads it, and an unusable value is
    /// dropped rather than refused — a sign-in must not fail over a field that gates nothing.
    pub cohort_hash: Option<String>,

    /// The directory device the client claims to be (slice `S-N3`), as a UUID.
    ///
    /// Client-asserted and unverified. Dropped, not refused, when it is not a usable UUID, for
    /// the same reason as `cohort_hash`.
    pub device_id: Option<String>,
}

impl fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LoginRequest")
            .field("email", &"<redacted>")
            .field("password", &"<redacted>")
            .field("cohort_hash", &self.cohort_hash)
            .field("device_id", &self.device_id)
            .finish()
    }
}

/// The refresh token being exchanged for a new pair.
///
/// `Debug` is hand-written, for the same reason as [`LoginRequest`]: this field is a live
/// credential.
#[derive(Schema, Deserialize)]
pub struct RefreshRequest {
    /// The refresh token issued by a previous login or refresh.
    ///
    /// Unconstrained in the schema for the reason [`LoginRequest`] records: an empty one is a
    /// token that does not verify, which is a 401 the handler already answers correctly.
    pub refresh_token: String,
}

impl fmt::Debug for RefreshRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RefreshRequest")
            .field("refresh_token", &"<redacted>")
            .finish()
    }
}

/// A freshly issued token pair.
///
/// The field names and `expires_by`'s meaning are a live client contract — `capsule-sdk`'s
/// `TokenResponseBody` reads exactly these — so they are preserved verbatim from the Salvo
/// surface. `Debug` is hand-written; both tokens are bearer credentials.
///
/// `Deserialize` is derived so the suite reads the pair back through the same type the server
/// wrote — a test that pulled `access_token` out of a `serde_json::Value` would still pass if
/// the field were renamed on the way out.
#[derive(Schema, Serialize, Deserialize)]
pub struct TokenResponse {
    /// The short-lived credential for ordinary requests.
    pub access_token: String,

    /// The long-lived credential that buys new pairs from `POST /v1/auth/refresh`.
    pub refresh_token: String,

    /// Always `Bearer`.
    pub token_type: String,

    /// The **absolute** Unix-seconds instant `access_token` stops being honoured.
    ///
    /// Absolute rather than a duration, which is what the field has always carried despite its
    /// name; the SDK depends on it.
    pub expires_by: u64,
}

impl fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenResponse")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("token_type", &self.token_type)
            .field("expires_by", &self.expires_by)
            .finish()
    }
}

impl From<IssuedTokens> for TokenResponse {
    fn from(issued: IssuedTokens) -> Self {
        Self {
            access_token: issued.access_token,
            refresh_token: issued.refresh_token,
            token_type: "Bearer".to_owned(),
            // A pre-epoch deadline is not representable and would mean a token that expired
            // before Unix time began; reporting `0` says "already expired", which is true.
            expires_by: u64::try_from(issued.access_expires_at.as_second()).unwrap_or(0),
        }
    }
}

// ===========================================================================================
// Rejections
// ===========================================================================================

/// Why a sign-in was refused.
///
/// Each variant publishes its stable `error.*` code as an RFC 9457 extension member named
/// `code` — the same field name and the same catalog the Salvo `ApiError` used, so a client
/// still localizes the code while the English `detail` stays English. The problem `type` is left
/// at `about:blank`: RFC 9457 wants a URI that resolves to documentation, Capsule has no
/// canonical origin to hang one under yet (`capsule-docs` still carries a `TODO: Get domain
/// later`), and inventing one would publish a link that will never resolve.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum LoginRejection {
    /// No such account, or the wrong password — deliberately one answer.
    #[error("invalid email or password")]
    #[problem(status = 401, title = "Invalid credentials")]
    InvalidCredentials {
        /// The stable catalog code, from `capsule_i18n::error_codes`.
        #[problem(extension)]
        code: &'static str,
    },

    /// The account is refusing attempts after too many failures.
    #[error("the account is locked after too many failed sign-in attempts")]
    #[problem(status = 423, title = "Account locked")]
    AccountLocked {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer, so the request neither succeeded nor was refused.
    #[error("the sign-in could not be completed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl LoginRejection {
    fn invalid_credentials() -> Self {
        Self::InvalidCredentials {
            code: error_codes::AUTH_INVALID_CREDENTIALS,
        }
    }

    fn account_locked() -> Self {
        Self::AccountLocked {
            code: error_codes::AUTH_ACCOUNT_LOCKED,
        }
    }

    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::AUTH_UNAVAILABLE,
        }
    }
}

/// Why a refresh was refused.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum RefreshRejection {
    /// The presented token did not verify, had expired, was the wrong kind, or named a session
    /// the server no longer holds.
    ///
    /// One variant for all four on purpose. A client can act on none of the distinctions — every
    /// one of them means "sign in again" — and telling an attacker which of them applied turns
    /// the endpoint into an oracle for live session ids.
    #[error("the session has ended; sign in again")]
    #[problem(status = 401, title = "Session expired")]
    SessionExpired {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// The session store could not answer.
    #[error("the session could not be refreshed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl RefreshRejection {
    fn session_expired() -> Self {
        Self::SessionExpired {
            code: error_codes::AUTH_SESSION_EXPIRED,
        }
    }

    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::AUTH_UNAVAILABLE,
        }
    }
}

/// Why a sign-out could not be completed.
///
/// One variant: a logout has exactly one way to fail that is not the credential's fault, and
/// the credential's failures belong to [`AccessToken`] rather than here.
#[derive(Debug, thiserror::Error, ApiError)]
#[problem(status = 500, title = "Internal server error")]
#[error("the session could not be closed")]
pub struct LogoutRejection {
    /// The stable catalog code.
    #[problem(extension)]
    code: &'static str,
}

impl LogoutRejection {
    fn unavailable() -> Self {
        Self {
            code: error_codes::AUTH_UNAVAILABLE,
        }
    }
}

// ===========================================================================================
// Operations
// ===========================================================================================

/// Exchange an email and password for a session.
///
/// Opens a session in the state store and returns the pair of tokens it is worked through. The
/// two advisory identifiers a client may send — `cohort_hash` and `device_id` — are recorded on
/// the session for the devices listing and gate nothing; an unusable one is dropped rather than
/// refused.
#[kynos::post("/v1/auth/login", operation_id = "login_user", tag = AuthTag)]
pub async fn login_user(
    Inject(auth): Inject<AuthContext>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<TokenResponse>, LoginRejection> {
    let cohort_hash = normalize_cohort_hash(request.cohort_hash.as_deref());
    let device_id = normalize_device_id(request.device_id.as_deref());

    // The password crosses this line once and never comes back: `authenticate` borrows it, and
    // what returns is a decision.
    let outcome = auth
        .accounts()
        .authenticate(&request.email, &request.password)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, "the account directory could not answer a sign-in");
            LoginRejection::unavailable()
        })?;

    let user = match outcome {
        Authentication::Granted(user) => user,
        Authentication::Locked => {
            // No account identifier: the directory did not name one, and the route must not
            // guess. The adapter that made this decision logs which account it was.
            tracing::warn!("a sign-in was refused: the account is locked");
            return Err(LoginRejection::account_locked());
        }
        Authentication::Refused => {
            tracing::info!("a sign-in was refused: the credentials did not match");
            return Err(LoginRejection::invalid_credentials());
        }
    };

    let now = auth.clock().now();
    let session_id = new_session_id();
    let record = SessionRecord {
        session_id: session_id.clone(),
        user_id: user.clone(),
        created_at: now,
        last_active_at: now,
        // Both always `None` here, as in Salvo. See the module docs.
        user_agent: None,
        ip_address: None,
        cohort_hash,
        device_id,
    };

    auth.sessions()
        .open_session(record)
        .await
        .map_err(|error| {
            store_unavailable(&error, "open a session");
            LoginRejection::unavailable()
        })?;

    // Issued after the record exists, so a token can never name a session the store has not
    // heard of. The refresh lifetime is the store's own TTL rather than a second constant —
    // a refresh token that outlives its session record verifies and then fails.
    let issued = auth
        .tokens()
        .issue(&user, &session_id, auth.sessions().ttl())
        .map_err(|error| {
            tracing::error!(%error, "a token pair could not be signed");
            LoginRejection::unavailable()
        })?;

    tracing::info!(user_id = %user, session_id = %session_id, "opened a session");
    Ok(Json(TokenResponse::from(issued)))
}

/// Exchange a refresh token for a new pair, rotating the session.
///
/// The presented session is **closed** and a new one opened in its place, so a refresh token is
/// good exactly once. The session's advisory provenance — its cohort hash and device id — is
/// carried across the rotation, or the devices listing would lose track of a device every time
/// its tokens turned over.
#[kynos::post("/v1/auth/refresh", operation_id = "refresh_token", tag = AuthTag)]
pub async fn refresh_token(
    Inject(auth): Inject<AuthContext>,
    Json(request): Json<RefreshRequest>,
) -> Result<Json<TokenResponse>, RefreshRejection> {
    // `TokenKind::Refresh` is the argument, not a check somebody has to remember: the Salvo
    // handler never inspected the token's scopes, so an *access* token rotated a session there.
    let VerifiedToken { user, session } = auth
        .tokens()
        .verify(&request.refresh_token, TokenKind::Refresh)
        .map_err(|error| {
            tracing::info!(%error, "a refresh was refused: the token was not honoured");
            RefreshRejection::session_expired()
        })?;

    let Some(record) = auth
        .sessions()
        .read_session(&session)
        .await
        .map_err(|error| {
            store_unavailable(&error, "read a session for refresh");
            RefreshRejection::unavailable()
        })?
    else {
        tracing::info!(session_id = %session, "a refresh was refused: no live session");
        return Err(RefreshRejection::session_expired());
    };

    if record.user_id != user {
        // Unreachable while the signing key is the server's alone — both halves came out of one
        // token this server signed — and kept because "unreachable while a key is secret" is
        // exactly the assumption worth a cheap check.
        tracing::warn!(
            session_id = %session,
            "a refresh was refused: the token's subject is not the session's owner"
        );
        return Err(RefreshRejection::session_expired());
    }

    // Close before opening. If the process dies between the two the user re-authenticates,
    // which is the safe direction; the other order would leave the presented refresh token
    // still spending a live session, which is the property rotation exists to remove.
    let closed = auth
        .sessions()
        .close_session(&session)
        .await
        .map_err(|error| {
            store_unavailable(&error, "close a session for refresh");
            RefreshRejection::unavailable()
        })?;
    if closed.is_none() {
        // It was live a moment ago, so something else closed it — a concurrent logout, or a
        // second refresh racing this one. Either way this token has nothing left to spend.
        tracing::info!(session_id = %session, "a refresh lost the race to rotate its session");
        return Err(RefreshRejection::session_expired());
    }

    let now = auth.clock().now();
    let rotated = new_session_id();
    let successor = SessionRecord {
        session_id: rotated.clone(),
        user_id: record.user_id.clone(),
        created_at: now,
        last_active_at: now,
        user_agent: record.user_agent,
        ip_address: record.ip_address,
        cohort_hash: record.cohort_hash,
        device_id: record.device_id,
    };

    auth.sessions()
        .open_session(successor)
        .await
        .map_err(|error| {
            store_unavailable(&error, "open the rotated session");
            RefreshRejection::unavailable()
        })?;

    let issued = auth
        .tokens()
        .issue(&record.user_id, &rotated, auth.sessions().ttl())
        .map_err(|error| {
            tracing::error!(%error, "a rotated token pair could not be signed");
            RefreshRejection::unavailable()
        })?;

    tracing::info!(
        user_id = %record.user_id,
        closed_session_id = %session,
        session_id = %rotated,
        "rotated a session"
    );
    Ok(Json(TokenResponse::from(issued)))
}

/// End the session the presented access token was issued against.
///
/// Idempotent: a session that is already closed, expired, or was never opened produces the same
/// answer, because "there is no longer a session" is what the caller asked for.
#[kynos::post("/v1/auth/logout", operation_id = "logout", tag = AuthTag)]
pub async fn logout(
    Inject(auth): Inject<AuthContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<NoContent, LogoutRejection> {
    let closed = auth
        .sessions()
        .close_session(&credential.session)
        .await
        .map_err(|error| {
            store_unavailable(&error, "close a session for logout");
            LogoutRejection::unavailable()
        })?;

    if let Some(record) = closed {
        tracing::info!(
            user_id = %record.user_id,
            session_id = %credential.session,
            "closed a session on request"
        );
    } else {
        tracing::debug!(
            session_id = %credential.session,
            "a logout found no live session; the token outlived its record"
        );
    }

    Ok(NoContent)
}

// ===========================================================================================
// Helpers
// ===========================================================================================

/// A new session identifier.
///
/// UUIDv7, per the Identifiers rule: a session id is a new id, and its creation time is not a
/// secret — the record carries `created_at` in the clear beside it.
fn new_session_id() -> SessionId {
    SessionId::new(Uuid::now_v7().to_string())
}

/// One log line for every store failure, so a support report can name the operation.
fn store_unavailable(error: &StoreError, doing: &'static str) {
    tracing::error!(%error, operation = doing, "the session store could not answer");
}

/// The advisory cohort hash, or `None` if it is not one worth keeping.
///
/// Trimmed, and dropped when empty or implausibly long. Never refused: the value gates nothing,
/// so failing a sign-in over it would be a security-irrelevant field taking down a security
/// operation.
fn normalize_cohort_hash(raw: Option<&str>) -> Option<String> {
    let trimmed = raw?.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_COHORT_HASH_LEN {
        return None;
    }
    Some(trimmed.to_owned())
}

/// The asserted device id as a `Uuid`, or `None` if it is not a usable one.
///
/// Parsed once, here, so nothing below this point re-normalizes a string — which is exactly what
/// [`SessionRecord::device_id`] being a `Uuid` is for. The nil UUID is refused because it is
/// what a client sends when it has no device id and did not check.
fn normalize_device_id(raw: Option<&str>) -> Option<Uuid> {
    let parsed = Uuid::parse_str(raw?.trim()).ok()?;
    (!parsed.is_nil()).then_some(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_login_request_never_prints_its_credentials() {
        let request = LoginRequest {
            email: "somebody@example.test".to_owned(),
            password: "correct horse battery staple".to_owned(),
            cohort_hash: Some("cohort-1".to_owned()),
            device_id: None,
        };

        let printed = format!("{request:?}");
        assert!(
            !printed.contains("correct horse battery staple"),
            "a password must not reach a log through Debug, got {printed}"
        );
        assert!(
            !printed.contains("somebody@example.test"),
            "an account's email must not reach a log through Debug, got {printed}"
        );
        assert!(
            printed.contains("cohort-1"),
            "the advisory fields are not secrets and stay legible, got {printed}"
        );
    }

    #[test]
    fn a_refresh_request_never_prints_its_token() {
        let request = RefreshRequest {
            refresh_token: "a-live-refresh-token".to_owned(),
        };
        assert!(!format!("{request:?}").contains("a-live-refresh-token"));
    }

    #[test]
    fn a_token_response_never_prints_its_tokens() {
        let response = TokenResponse {
            access_token: "an-access-token".to_owned(),
            refresh_token: "a-refresh-token".to_owned(),
            token_type: "Bearer".to_owned(),
            expires_by: 42,
        };

        let printed = format!("{response:?}");
        assert!(!printed.contains("an-access-token") && !printed.contains("a-refresh-token"));
        assert!(printed.contains("Bearer") && printed.contains("42"));
    }

    #[test]
    fn a_cohort_hash_is_trimmed_kept_or_dropped() {
        assert_eq!(normalize_cohort_hash(None), None);
        assert_eq!(normalize_cohort_hash(Some("   ")), None);
        assert_eq!(
            normalize_cohort_hash(Some("  abc  ")),
            Some("abc".to_owned())
        );

        let at_the_limit = "x".repeat(MAX_COHORT_HASH_LEN);
        assert_eq!(
            normalize_cohort_hash(Some(&at_the_limit)),
            Some(at_the_limit.clone())
        );

        let over_the_limit = "x".repeat(MAX_COHORT_HASH_LEN + 1);
        assert_eq!(
            normalize_cohort_hash(Some(&over_the_limit)),
            None,
            "an implausible cohort is dropped, not stored and not refused"
        );
    }

    #[test]
    fn a_device_id_is_parsed_once_or_dropped() {
        assert_eq!(normalize_device_id(None), None);
        assert_eq!(normalize_device_id(Some("not-a-uuid")), None);
        assert_eq!(
            normalize_device_id(Some("00000000-0000-0000-0000-000000000000")),
            None,
            "the nil uuid is what a client sends when it has none"
        );

        // Accepted in any spelling `Uuid::parse_str` reads, and normalized by being a `Uuid`
        // rather than by a second pass over a string.
        let canonical =
            Uuid::parse_str("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f").expect("the literal is a uuid");
        for spelling in [
            " 018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f ",
            "018F3F1E-4B7A-7C9D-8E2F-1A2B3C4D5E6F",
            "urn:uuid:018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f",
            "{018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f}",
        ] {
            assert_eq!(
                normalize_device_id(Some(spelling)),
                Some(canonical),
                "{spelling:?} names the same device"
            );
        }
    }

    #[test]
    fn session_ids_are_distinct_and_time_ordered() {
        let first = new_session_id();
        let second = new_session_id();
        assert_ne!(first, second, "two sessions are never one session");

        // UUIDv7 sorts by creation time, which is what makes the session listing's "oldest
        // first" order stable when two records share a `created_at` second.
        assert!(first.as_str() < second.as_str());
    }

    #[test]
    fn every_rejection_publishes_its_catalog_code() {
        // The `error.*` code is the discriminator clients switch on, so a variant that lost one
        // — or gained the wrong one — is a client that cannot localize the failure.
        assert!(matches!(
            LoginRejection::invalid_credentials(),
            LoginRejection::InvalidCredentials { code } if code == error_codes::AUTH_INVALID_CREDENTIALS
        ));
        assert!(matches!(
            LoginRejection::account_locked(),
            LoginRejection::AccountLocked { code } if code == error_codes::AUTH_ACCOUNT_LOCKED
        ));
        assert!(matches!(
            LoginRejection::unavailable(),
            LoginRejection::Unavailable { code } if code == error_codes::AUTH_UNAVAILABLE
        ));
        assert!(matches!(
            RefreshRejection::session_expired(),
            RefreshRejection::SessionExpired { code } if code == error_codes::AUTH_SESSION_EXPIRED
        ));
        assert!(matches!(
            RefreshRejection::unavailable(),
            RefreshRejection::Unavailable { code } if code == error_codes::AUTH_UNAVAILABLE
        ));
        assert_eq!(
            LogoutRejection::unavailable().code,
            error_codes::AUTH_UNAVAILABLE
        );
    }
}
