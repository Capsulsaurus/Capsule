//! `POST /v1/auth/register`, `POST /v1/auth/login`, `POST /v1/auth/refresh`,
//! `POST /v1/auth/logout`.
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
//! | register `201` | **`200` here.** Kynos's `Created` requires a `Location`, and this server exposes no URL for an account — see [`register_user`] |
//! | register `400` / `409` / `500` | kept, and now coded (`error.auth.registration_invalid`, `error.auth.user_already_exists`, `error.auth.unavailable`) |
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

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_i18n::error_codes;
use kynos::prelude::*;
use kynos::response::status::NoContent;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::{
    AccessToken, AuthContext, Authentication, DirectoryError, IssuedTokens, MIN_PASSWORD_LENGTH,
    Registration, TokenKind, VerifiedToken,
};
use crate::store::{
    ChallengeToken, RevokeAllChallenge, SessionId, SessionRecord, StoreError, UserId,
};

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

/// The `POST /v1/auth/register` body.
///
/// Deliberately the *smallest* thing that can create an account: an address and a password. No
/// display name, no profile, no invitation code — every one of those would be a field the server
/// stores about a person, and this server's whole posture is that it stores as little as it can.
#[derive(Schema, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct RegisterRequest {
    /// The address the account is identified by.
    pub email: String,

    /// The password that will authenticate this account's **sessions**.
    ///
    /// Never the master key's input: the master key does not derive from it and is never visible
    /// to the credential verifier. Hashed by the registry adapter and never retained, logged, or
    /// echoed.
    pub password: String,
}

impl fmt::Debug for RegisterRequest {
    /// Redacted, exactly as [`LoginRequest`]'s is: a `Debug` that printed a password is how one
    /// reaches a log file.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisterRequest")
            .field("email", &self.email)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// Why an account was not created.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum RegisterRejection {
    /// The request cannot create an account as it stands.
    #[error("{detail}")]
    #[problem(status = 400, title = "Invalid registration")]
    Invalid {
        /// What was wrong, in English. The client localizes `code`, not this.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// An account already exists for that address.
    ///
    /// **This is an account oracle and it is the decided contract** — the message catalog fixes
    /// `error.auth.user_already_exists` for exactly this. The alternative, answering success and
    /// creating nothing, leaves a client that then cannot sign in and no way to tell it why.
    /// What bounds the oracle is a rate limiter, and there is none; see
    /// [`crate::auth::registry`] for the fact it is waiting on.
    #[error("an account already exists for that address")]
    #[problem(status = 409, title = "Account already exists")]
    AlreadyExists {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the account could not be created")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl RegisterRejection {
    /// The request was not one that can create an account.
    fn invalid(detail: impl Into<String>) -> Self {
        Self::Invalid {
            detail: detail.into(),
            code: error_codes::AUTH_REGISTRATION_INVALID,
        }
    }

    /// A collaborator could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::AUTH_UNAVAILABLE,
        }
    }
}

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

/// Create an account, and open its first session.
///
/// # Why it signs you in
///
/// The alternative is `201` with no body and a client that immediately posts the same
/// credentials to `/v1/auth/login`, which is one more round trip for one more chance to fail and
/// nothing gained. It also makes the CLI's `capsule register` mean what a person expects: after
/// it, you are registered *and* signed in.
///
/// # What it does not do
///
/// **It does not publish a device directory**, and the account is therefore unable to upload
/// until its client publishes one. That is not an omission here: `S-C20` removed the
/// account-creation fallback for invariant 7's floor precisely so that "was this device in the
/// directory" has an honest answer for a brand-new account, and the honest answer is *no*. A
/// client's first action after registering is `POST /v1/auth/devices/directory`.
///
/// **It is not rate-limited**, and that is a real gap rather than an oversight — see
/// [`crate::auth::registry`] for the fact the limiter is waiting on. This is the one
/// unauthenticated write on the surface.
///
/// # `200`, where Salvo answered `201`
///
/// Kynos's `Created` requires a `Location` — a `201` that does not say *where* tells a client
/// something exists and not how to reach it, which is a defect the type refuses to let you
/// commit. This server exposes no URL for an account: `GET /v1/auth/profile` is among the
/// operations `S-C53` records as unported. Inventing a location to satisfy a status would be
/// inventing a surface, so the status moved instead. What a caller actually needs — the token
/// pair — is in the body either way.
#[kynos::post("/v1/auth/register", operation_id = "register_user", tag = AuthTag)]
pub async fn register_user(
    Inject(auth): Inject<AuthContext>,
    Json(request): Json<RegisterRequest>,
) -> Result<Json<TokenResponse>, RegisterRejection> {
    // Structural, before anything is written. An address the server cannot use and a password
    // under the floor are both the caller's to fix, and neither should reach a store.
    let email = request.email.trim();
    if email.is_empty() || !email.contains('@') {
        return Err(RegisterRejection::invalid("that is not a usable address"));
    }
    if request.password.chars().count() < MIN_PASSWORD_LENGTH {
        return Err(RegisterRejection::invalid(format!(
            "a password must be at least {MIN_PASSWORD_LENGTH} characters"
        )));
    }

    let now = auth.clock().now();
    // Minted here rather than by the adapter: the id is a fact about this server's clock, and
    // two adapters minting their own would be two id schemes.
    let user = crate::auth::new_user_id();
    let user = match auth
        .registry()
        .create(email, &request.password, &user, now)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, "the account registry could not create an account");
            RegisterRejection::unavailable()
        })? {
        Registration::Created(user) => user,
        Registration::AlreadyExists => {
            tracing::info!("a registration was refused: the address is taken");
            return Err(RegisterRejection::AlreadyExists {
                code: error_codes::AUTH_USER_ALREADY_EXISTS,
            });
        }
    };

    // The first session, opened exactly as a sign-in opens one — including
    // `authenticated_at`, because registering *is* a credential presentation and a freshness
    // gate measuring from anything else would be measuring from nothing.
    let session_id = new_session_id();
    auth.sessions()
        .open_session(SessionRecord {
            session_id: session_id.clone(),
            user_id: user.clone(),
            created_at: now,
            authenticated_at: now,
            last_active_at: now,
            user_agent: None,
            ip_address: None,
            cohort_hash: None,
            device_id: None,
        })
        .await
        .map_err(|error| {
            // The account exists and its session does not. Answered as an outage rather than as
            // a failed registration, because it is one: the caller signs in and gets a session.
            store_unavailable(&error, "open the first session of a new account");
            RegisterRejection::unavailable()
        })?;

    let issued = auth
        .tokens()
        .issue(&user, &session_id, auth.sessions().ttl())
        .map_err(|error| {
            tracing::error!(%error, "a token pair could not be signed");
            RegisterRejection::unavailable()
        })?;

    tracing::info!(user_id = %user, session_id = %session_id, "registered an account");
    Ok(Json(TokenResponse::from(issued)))
}

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
        // A sign-in *is* a credential presentation, so this is the moment a freshness gate
        // measures from. A refresh will carry it forward untouched.
        authenticated_at: now,
        last_active_at: now,
        // Both always `None` here, as in Salvo. See the module docs.
        user_agent: None,
        ip_address: None,
        cohort_hash: cohort_hash.clone(),
        device_id,
    };

    auth.sessions()
        .open_session(record)
        .await
        .map_err(|error| {
            store_unavailable(&error, "open a session");
            LoginRejection::unavailable()
        })?;

    // The durable cohort map (`S-C13`), written **after** the session and **never** allowed to
    // fail the sign-in. A cohort is legibility metadata: it groups a physical device's
    // re-enrollments in the devices view and gates nothing. Refusing a sign-in because a
    // grouping aid could not be recorded would let an advisory value take down the one
    // operation an account cannot do without — which is the same reason a malformed cohort is
    // dropped rather than rejected.
    if let Some(cohort_hash) = cohort_hash.as_deref()
        && let Err(error) = auth.cohorts().observe(&user, cohort_hash, now).await
    {
        tracing::warn!(
            %error,
            user_id = %user,
            "the device-cohort map could not be updated; the sign-in stands"
        );
    }

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
        // **Carried forward, not reset.** A refresh proves possession of a refresh token, which
        // is not a credential presentation — resetting this would make every re-authentication
        // gate satisfiable by waiting fifteen minutes, which is the same as not having one.
        authenticated_at: record.authenticated_at,
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

/// A password, re-presented on a session that already exists.
#[derive(Schema, Serialize, Deserialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct ReauthenticateRequest {
    /// The account's password.
    pub password: String,
}

/// `Debug` is hand-written so a password never reaches a log line.
impl fmt::Debug for ReauthenticateRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ReauthenticateRequest")
            .field("password", &"<redacted>")
            .finish()
    }
}

/// When the re-authenticated session's freshness window last opened.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReauthenticateResponse {
    /// The moment the credential was accepted, RFC 3339.
    ///
    /// Returned so a client can decide locally whether a gated operation will be admitted,
    /// rather than discovering it from a `403` in the middle of a ceremony.
    pub authenticated_at: String,
}

/// Prove a credential again on the current session, without opening a new one.
///
/// **The only way to satisfy the freshness gate `S-C7` enforces**, and it exists because
/// without it the gate is unusable: `authenticated_at` is deliberately *not* reset by a refresh,
/// so a user signed in an hour ago would otherwise have to sign out entirely to add a device —
/// and the session they abandoned would linger in their own devices listing.
///
/// It does not mint tokens and does not rotate the session. The caller keeps the credential
/// they already hold; what changes is one timestamp on the record behind it.
///
/// # Errors
///
/// The same refusals as a sign-in, for the same reasons: a wrong password is
/// `401 error.auth.invalid_credentials`, a locked account is `403`, and the account directory
/// failing is `500`. A caller that guessed a password here learns exactly what it would learn
/// at `/v1/auth/login`, and no more.
#[kynos::post(
    "/v1/auth/reauthenticate",
    operation_id = "reauthenticate",
    tag = AuthTag
)]
pub async fn reauthenticate(
    Inject(auth): Inject<AuthContext>,
    Auth(credential): Auth<AccessToken>,
    Json(request): Json<ReauthenticateRequest>,
) -> Result<Json<ReauthenticateResponse>, LoginRejection> {
    let user = UserId::new(credential.user.as_str());

    // The account is the credential's, never a request field: this operation must not be usable
    // to test another account's password.
    let outcome = auth
        .accounts()
        .authenticate_user(&user, &request.password)
        .await
        .map_err(|error: DirectoryError| {
            tracing::error!(%error, %user, "the account directory could not answer a re-auth");
            LoginRejection::unavailable()
        })?;

    match outcome {
        Authentication::Granted(_) => {}
        Authentication::Locked => {
            tracing::warn!(%user, "a re-authentication was refused: the account is locked");
            return Err(LoginRejection::account_locked());
        }
        Authentication::Refused => {
            tracing::info!(%user, "a re-authentication was refused: the password did not match");
            return Err(LoginRejection::invalid_credentials());
        }
    }

    let now = auth.clock().now();
    let updated = auth
        .sessions()
        .mark_authenticated(&credential.session, now)
        .await
        .map_err(|error| {
            store_unavailable(&error, "mark a session re-authenticated");
            LoginRejection::unavailable()
        })?;

    // A token whose session record is gone verifies but names nothing. Answered as a refused
    // credential rather than as a server fault: from the caller's side the session is over.
    let Some(updated) = updated else {
        tracing::info!(%user, "a re-authentication found no live session");
        return Err(LoginRejection::invalid_credentials());
    };

    tracing::info!(%user, session_id = %credential.session, "a session re-authenticated");
    Ok(Json(ReauthenticateResponse {
        authenticated_at: updated.authenticated_at.to_string(),
    }))
}

/// The challenge a global sign-out is signed over.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RevokeChallengeResponse {
    /// The single-use token. Burned on the first attempt, successful or not.
    pub challenge: String,
    /// When it stops being redeemable, RFC 3339.
    pub expires_at: String,
}

/// A master-key proof over an issued challenge.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
#[serde(deny_unknown_fields)]
pub struct RevokeAllRequest {
    /// The challenge that was issued.
    pub challenge: String,
    /// The account identity key's hybrid signature over
    /// [`revoke_all_signing_bytes`](capsule_core::crypto::revoke::revoke_all_signing_bytes),
    /// canonical CBOR, base64.
    pub proof: String,
}

/// What a global sign-out closed.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RevokeAllResponse {
    /// How many sessions were closed — the caller's own among them.
    ///
    /// Counted from the records the store actually removed, never from a separately maintained
    /// index. The Salvo implementation read a per-user set that `revoke_session` did not clean
    /// up, so this number inflated by one for every prior refresh; `S-C29` made the record and
    /// its listing entry one fact, so there is nothing left to disagree.
    pub revoked: u64,
}

/// Why a global sign-out did not happen.
///
/// **Nothing partial happens on any of these.** A refused revoke closes no session, so a client
/// has nothing to clear locally and can retry the whole ceremony.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum RevokeAllRejection {
    /// No readable proof was presented.
    ///
    /// Separate from [`RevokeAllRejection::ProofInvalid`] because the client's remedy differs:
    /// this one is a malformed request, and telling a caller "your base64 is not base64" is not
    /// an oracle about anything.
    #[error("a readable master-key proof is required")]
    #[problem(status = 401, title = "Master-key proof required")]
    ProofRequired {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A proof was presented and did not establish the account.
    ///
    /// One answer for every reason: an unknown, spent or expired challenge; an account with no
    /// published directory to anchor an identity key; or a signature that does not verify. A
    /// caller learns only that it failed, which is what stops the endpoint being an oracle over
    /// which accounts have published a directory.
    #[error("the master-key proof did not verify")]
    #[problem(status = 401, title = "Master-key proof invalid")]
    ProofInvalid {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A store could not answer.
    #[error("the global sign-out could not be completed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Issue a single-use challenge for a global sign-out.
///
/// **Authenticated by a session token, unlike the revoke itself.** That is not a contradiction
/// of the ceremony's asymmetry: a challenge is worthless without the identity key, so handing
/// one to a stolen token costs nothing — while issuing them unauthenticated would make this an
/// oracle for whether an account exists. The account comes from the credential and never from a
/// request field, so a caller cannot ask for somebody else's challenge.
#[kynos::post(
    "/v1/auth/logout/all/challenge",
    operation_id = "revoke_all_challenge",
    tag = AuthTag
)]
pub async fn revoke_all_challenge(
    Inject(auth): Inject<AuthContext>,
    Auth(credential): Auth<AccessToken>,
) -> Result<Json<RevokeChallengeResponse>, RevokeAllRejection> {
    let user = UserId::new(credential.user.as_str());
    // Full-entropy and unguessable: a challenge that could be predicted would let an attacker
    // who has separately obtained a signature pre-compute a proof.
    let token = ChallengeToken::new(Uuid::new_v4().to_string());
    let issued_at = auth.clock().now();

    auth.challenges()
        .issue(
            &token,
            RevokeAllChallenge {
                user_id: user.clone(),
                issued_at,
            },
        )
        .await
        .map_err(|error| {
            store_unavailable(&error, "issue a revoke-all challenge");
            RevokeAllRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;

    tracing::info!(user_id = %user, "issued a revoke-all challenge");
    Ok(Json(RevokeChallengeResponse {
        challenge: token.as_str().to_owned(),
        expires_at: crate::store::deadline(issued_at, auth.challenges().ttl()).to_string(),
    }))
}

/// Close every session for the account the proof establishes.
///
/// **No `Auth`, deliberately.** design/authentication.md gates this on proof of master-key
/// possession *instead of* a session token, and the reason is the damage scenario: an attacker
/// holding a stolen token could otherwise invoke "log out of all devices" and lock the
/// legitimate user out of every device they own. Requiring the identity key means a stolen
/// token can revoke only itself. The account is established by the burned challenge, so there
/// is no account field for a caller to aim at either.
///
/// The caller's own session goes with the rest. That is the ceremony, not an oversight.
#[kynos::post("/v1/auth/logout/all", operation_id = "revoke_all", tag = AuthTag)]
pub async fn revoke_all(
    Inject(auth): Inject<AuthContext>,
    Inject(directories): Inject<crate::directory::DeviceDirectoryContext>,
    Json(request): Json<RevokeAllRequest>,
) -> Result<Json<RevokeAllResponse>, RevokeAllRejection> {
    let proof_bytes = BASE64.decode(request.proof.trim()).map_err(|error| {
        tracing::info!(%error, "a revoke-all proof was not base64");
        RevokeAllRejection::ProofRequired {
            code: error_codes::AUTH_REVOKE_PROOF_REQUIRED,
        }
    })?;
    let Ok(signature) =
        capsule_core::cbor::from_slice::<capsule_core::crypto::keys::HybridSignature>(&proof_bytes)
    else {
        tracing::info!("a revoke-all proof was not a decodable hybrid signature");
        return Err(RevokeAllRejection::ProofRequired {
            code: error_codes::AUTH_REVOKE_PROOF_REQUIRED,
        });
    };

    // Burned first, and burned whatever happens next. A challenge that survived a failed
    // attempt would let an attacker grind signatures against a live one; this costs a
    // legitimate user one extra round trip and is the whole reason `consume` has no read-only
    // sibling.
    let token = ChallengeToken::new(request.challenge.clone());
    let claimed = auth.challenges().consume(&token).await.map_err(|error| {
        store_unavailable(&error, "consume a revoke-all challenge");
        RevokeAllRejection::Unavailable {
            code: error_codes::AUTH_UNAVAILABLE,
        }
    })?;

    let Some(claimed) = claimed else {
        tracing::info!("a revoke-all presented an unknown, spent or expired challenge");
        return Err(RevokeAllRejection::invalid());
    };

    // The anchor, not a key the request supplied (`S-C42`). A proof checked against a
    // caller-supplied key would prove only that the caller can sign something, which is not a
    // fact about the account.
    let published = directories
        .store()
        .fetch(&claimed.user_id)
        .await
        .map_err(|error| {
            store_unavailable(&error, "read a device directory for a revoke-all");
            RevokeAllRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;

    let Some(published) = published else {
        // An account with no published directory has no anchor and therefore cannot use this
        // ceremony. Answered identically to a bad signature so the endpoint does not report
        // which accounts have published.
        tracing::info!(user_id = %claimed.user_id, "a revoke-all found no anchored directory");
        return Err(RevokeAllRejection::invalid());
    };

    let Ok(identity_key) =
        capsule_core::crypto::keys::HybridVerifyingKey::from_bytes(&published.identity_key)
    else {
        tracing::error!(
            user_id = %claimed.user_id,
            "a stored identity anchor could not be read; the account cannot revoke globally"
        );
        return Err(RevokeAllRejection::invalid());
    };

    if !capsule_core::crypto::revoke::verify_revoke_all_proof(
        &identity_key,
        &request.challenge,
        &signature,
    ) {
        tracing::warn!(
            user_id = %claimed.user_id,
            "a revoke-all proof did not verify under the account's identity anchor"
        );
        return Err(RevokeAllRejection::invalid());
    }

    let closed = auth
        .sessions()
        .close_all_for_user(&claimed.user_id)
        .await
        .map_err(|error| {
            store_unavailable(&error, "close every session for a revoke-all");
            RevokeAllRejection::Unavailable {
                code: error_codes::AUTH_UNAVAILABLE,
            }
        })?;

    tracing::info!(
        user_id = %claimed.user_id,
        revoked = closed.len(),
        "a global sign-out closed every session for an account"
    );
    Ok(Json(RevokeAllResponse {
        revoked: closed.len() as u64,
    }))
}

impl RevokeAllRejection {
    /// The one answer every failed proof gets.
    fn invalid() -> Self {
        Self::ProofInvalid {
            code: error_codes::AUTH_REVOKE_PROOF_INVALID,
        }
    }
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
