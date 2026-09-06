//! SDK-owned authentication: the session/token store and the auto-refresh engine.
//!
//! This is the "SDK owns the complete user flow" foundation (slice `S-D7` in the
//! repo-root `SLICES.md`): native apps call [`AuthClient::login`], then hand every
//! request through [`Session::execute`], then [`Session::logout`] — they never
//! juggle raw access tokens. The store keeps the access + refresh token pair and:
//!
//! - runs a quick **asynchronous pre-flight expiry check** before each request and
//!   refreshes proactively when the access token is within the refresh skew of
//!   expiry ([`Session::valid_access_token`]);
//! - **single-flights** refreshes — concurrent callers that all see a stale token
//!   coalesce onto one network refresh ([`Session::ensure_refreshed`]);
//! - **retries once on `401`**: an authenticated request that the server rejects is
//!   refreshed once and replayed, so a token that expired between the pre-flight
//!   check and the wire is transparently recovered.
//!
//! It is hand-rolled over `reqwest` (rustls only) against the server's own
//! `/v1/auth/{register,login,refresh,logout}`. It does not
//! route through the generated client, but the reason is no longer that spargen is
//! parked — spargen ships and `S-D8` generates the typed surface today. What lives here
//! is token *orchestration*: the pre-flight refresh, the `401`-retry-once replay, and
//! the session store they mutate. That is deliberately outside generated code, which
//! owns parsing and serialization and nothing else. Auth requests are [`crate::net::RetryClass::Interactive`]; the full backoff
//! ladder lands with `S-D10`, but the `401`-retry-once and pre-flight refresh here
//! are the parts the session store owns.
//!
//! ## Testing
//!
//! The wire flows (login/refresh/logout, `401` recovery, error mapping) are proven
//! against a focused in-process mock HTTP server rather than a booted server: booting one
//! from this crate would invert the dependency — the *server* dev-depends on this crate, not
//! the other way round (`S-D28`) — and a mock keeps every wire case deterministic. The round
//! trip against the real router, over a real socket, lives where the dependency points:
//! `capsule-server/tests/sdk_client.rs`. The pre-flight and single-flight guarantees are proven with an injected
//! [`Clock`] — no sleeps, no wall-clock dependence.

use std::sync::Arc;

use capsule_i18n::error_codes;
use jiff::Timestamp;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tracing::instrument;

/// Default pre-flight refresh window: refresh once the access token is within this
/// many seconds of expiry, so an in-flight request never races the boundary.
const DEFAULT_REFRESH_SKEW_SECS: i64 = 30;

// ─── Clock seam ──────────────────────────────────────────────────────────────

/// Wall-clock seam so token-expiry logic is deterministically testable.
///
/// Production uses [`SystemClock`]; tests inject a controllable clock to exercise
/// pre-flight refresh and single-flight without sleeping.
pub trait Clock: Send + Sync {
    /// The current wall-clock instant.
    fn now(&self) -> Timestamp;
}

/// The production [`Clock`], backed by the system wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

// ─── Errors ──────────────────────────────────────────────────────────────────

/// Everything the auth flows can fail with. Callers switch on the typed variant
/// (never a bare HTTP status); [`AuthError::error_code`] yields the stable
/// `error.*` catalog code for client localization where one applies.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// Transport-level failure (DNS, TLS, connection, timeout).
    #[error("network transport error: {0}")]
    Transport(#[from] reqwest::Error),
    /// The configured base URL could not be parsed.
    #[error("invalid base URL {url:?}: {reason}")]
    InvalidBaseUrl {
        /// The offending URL.
        url: String,
        /// Why it was rejected.
        reason: String,
    },
    /// Login was rejected: the credentials were wrong.
    #[error("invalid credentials")]
    InvalidCredentials,
    /// Login was rejected: the account is temporarily locked.
    #[error("account locked after repeated failed logins")]
    AccountLocked,
    /// The server asked the client to back off.
    #[error("rate limited; retry after {retry_after_secs}s")]
    RateLimited {
        /// Seconds to wait before retrying, from the `Retry-After` header.
        retry_after_secs: u64,
    },
    /// The session is gone (refresh token expired or revoked). The user must
    /// re-authenticate interactively.
    #[error("session expired or revoked; interactive re-authentication required")]
    SessionExpired,
    /// An authenticated operation was attempted with no active session.
    #[error("no active session; call login() first")]
    NotAuthenticated,

    /// A code is needed and this caller cannot ask for one.
    ///
    /// Only produced by [`LoginOutcome::into_session`]: the login itself answers a
    /// [`LoginOutcome`], because a second factor is the system working rather than a failure.
    #[error("this account requires a second factor; complete the sign-in with a code")]
    SecondFactorRequired,
    /// A server response the client does not model.
    #[error("unexpected {status} response from {endpoint}: {detail}")]
    Unexpected {
        /// HTTP status code.
        status: u16,
        /// Which auth endpoint produced it.
        endpoint: &'static str,
        /// English detail message from the server body, if any.
        detail: String,
        /// Stable `error.*` catalog code from the server body, if any.
        code: Option<String>,
    },
    /// A success response whose body did not match the token contract.
    #[error("malformed server response from {endpoint}: {reason}")]
    MalformedResponse {
        /// Which auth endpoint produced it.
        endpoint: &'static str,
        /// What was wrong with the body.
        reason: String,
    },
}

impl AuthError {
    /// The stable `error.*` catalog code a client localizes, when one applies.
    ///
    /// The English [`Display`](std::fmt::Display) form is the developer/log detail;
    /// clients render a localized high-level message from this code (mirrors the
    /// server's `ApiError { error, code }` contract).
    pub fn error_code(&self) -> Option<&str> {
        match self {
            Self::InvalidCredentials => Some(error_codes::AUTH_INVALID_CREDENTIALS),
            Self::RateLimited { .. } => Some(error_codes::AUTH_RATE_LIMITED),
            Self::Unexpected { code, .. } => code.as_deref(),
            _ => None,
        }
    }
}

/// Which auth endpoint a wire error came from — drives status→variant mapping.
#[derive(Clone, Copy)]
enum Endpoint {
    Register,
    Login,
    VerifyTotp,
    Refresh,
    Logout,
}

impl Endpoint {
    fn name(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::Login => "login",
            Self::VerifyTotp => "login/verify-totp",
            Self::Refresh => "refresh",
            Self::Logout => "logout",
        }
    }

    /// What a `401` from this endpoint means.
    fn unauthorized_error(self) -> AuthError {
        match self {
            // Registration does not authenticate an existing session, so a `401` from
            // it is not a real ceremony outcome; treat it as a credential rejection.
            Self::Register | Self::Login => AuthError::InvalidCredentials,
            // `VerifyTotp` is here rather than beside `Login`, and it is not an oversight that
            // it reads the same as a refresh: a `401` completing a second factor means the
            // challenge expired or the code did not verify, and both send the caller back to
            // the password — which is what `SessionExpired` says. Neither is a *credential*
            // rejection, because the password already verified to get this far.
            Self::VerifyTotp | Self::Refresh | Self::Logout => AuthError::SessionExpired,
        }
    }
}

// ─── Wire types ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct LoginRequestBody<'a> {
    email: &'a str,
    password: &'a str,
    /// Advisory device-cohort hash (slice `S-D11`). Rides the session-creation body
    /// only; **omitted entirely when absent** so "absent stays legal" is literal on
    /// the wire (no `null` field). Never read by any authorization decision (S-C13).
    #[serde(skip_serializing_if = "Option::is_none")]
    cohort_hash: Option<&'a str>,
}

/// The registration body: an address and a password, and nothing else.
///
/// It carried a username, a display name and the advisory cohort hash until `S-C53`. The server
/// takes none of them and its body is **strict**, so a field kept for old times' sake would be a
/// `422` rather than a value quietly ignored.
#[derive(Serialize)]
struct RegisterRequestBody<'a> {
    email: &'a str,
    password: &'a str,
}

/// The body that completes a sign-in with a code (`S-C55`).
///
/// The advisory cohort rides **here** rather than on the login, because this is the request that
/// opens the session: a second-factor sign-in that sent its cohort on the first leg would attach
/// it to a session that does not exist yet, and would land in the devices view ungrouped.
#[derive(Serialize)]
struct VerifyTotpRequestBody<'a> {
    mfa_token: &'a str,
    totp_code: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    cohort_hash: Option<&'a str>,
}

/// The `202 Accepted` body: a half-finished sign-in.
#[derive(Deserialize)]
struct SecondFactorChallengeBody {
    mfa_token: String,
    expires_by: u64,
}

#[derive(Serialize)]
struct RefreshRequestBody<'a> {
    refresh_token: &'a str,
}

/// The server's `TokenResponse`. `token_type` and any other
/// fields are ignored; `expires_by` is the **absolute** Unix-seconds expiry of the
/// access token.
#[derive(Deserialize)]
struct TokenResponseBody {
    access_token: String,
    refresh_token: String,
    expires_by: u64,
}

/// The server's `ApiError` shape.
#[derive(Deserialize)]
struct ApiErrorBody {
    #[serde(default)]
    error: String,
    #[serde(default)]
    code: Option<String>,
}

// ─── Token store ─────────────────────────────────────────────────────────────

/// The stored token pair for an active session.
#[derive(Clone)]
struct TokenSet {
    access_token: SecretString,
    refresh_token: SecretString,
    /// Absolute instant the access token expires (server `expires_by`).
    access_expires_at: Timestamp,
}

impl TokenSet {
    fn from_wire(endpoint: Endpoint, body: TokenResponseBody) -> Result<Self, AuthError> {
        let access_expires_at = Timestamp::from_second(body.expires_by as i64).map_err(|e| {
            AuthError::MalformedResponse {
                endpoint: endpoint.name(),
                reason: format!("invalid expires_by {}: {e}", body.expires_by),
            }
        })?;
        Ok(Self {
            access_token: body.access_token.into(),
            refresh_token: body.refresh_token.into(),
            access_expires_at,
        })
    }
}

/// A session's token material for handing to the platform's secure storage so a
/// session survives process restarts. The SDK owns the store and refresh logic;
/// the platform owns only the at-rest bytes.
#[derive(Clone)]
pub struct PersistedSession {
    /// The current access token.
    pub access_token: SecretString,
    /// The current refresh token.
    pub refresh_token: SecretString,
    /// Absolute access-token expiry, Unix seconds (server `expires_by`).
    pub access_expires_at_unix: i64,
}

// ─── Endpoints ───────────────────────────────────────────────────────────────

/// Precomputed absolute endpoint URLs derived from the auth base URL.
struct AuthEndpoints {
    register: String,
    login: String,
    verify_totp: String,
    refresh: String,
    logout: String,
}

impl AuthEndpoints {
    fn from_base(base_url: &str) -> Result<Self, AuthError> {
        // Validate scheme/host up front so a bad base fails at construction, not at
        // first request; we then build paths by concatenation for predictability.
        reqwest::Url::parse(base_url).map_err(|e| AuthError::InvalidBaseUrl {
            url: base_url.to_string(),
            reason: e.to_string(),
        })?;
        let trimmed = base_url.trim_end_matches('/');
        Ok(Self {
            register: format!("{trimmed}/register"),
            login: format!("{trimmed}/login"),
            verify_totp: format!("{trimmed}/login/verify-totp"),
            refresh: format!("{trimmed}/refresh"),
            logout: format!("{trimmed}/logout"),
        })
    }
}

/// What a password login answered with (`S-C55`).
///
/// Two variants because the server has two outcomes and says so with a status: `200` with a
/// token pair, `202` with a challenge. Neither is a failure — an account having a second factor
/// is the system working — which is why this is a value rather than an `AuthError` variant.
///
/// A caller that only supports passwords should match on this and say so, rather than treating
/// the challenge as an error it cannot describe.
///
/// No `Debug`. [`Session`] holds live tokens and deliberately has none, and the challenge here is
/// a credential in its own right — deriving one would put half a sign-in in any log line that
/// formatted the value.
pub enum LoginOutcome {
    /// The account has no second factor, and this is its session.
    Session(Session),
    /// The password verified and a code is still needed.
    SecondFactorRequired {
        /// The challenge to present to
        /// [`verify_second_factor`](AuthClient::verify_second_factor). Good once, and for five
        /// minutes.
        mfa_token: SecretString,
        /// The absolute Unix-seconds instant the challenge stops being honoured.
        expires_by: u64,
    },
}

impl LoginOutcome {
    /// The session, if the sign-in finished.
    ///
    /// A convenience for the callers that genuinely cannot prompt — an automated one, or a test
    /// against an account known to have no second factor. It returns an error rather than
    /// panicking, because "this account needs a code and I cannot ask for one" is a runtime
    /// condition rather than a programming mistake.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::SecondFactorRequired`] when a code is needed.
    pub fn into_session(self) -> Result<Session, AuthError> {
        match self {
            Self::Session(session) => Ok(session),
            Self::SecondFactorRequired { .. } => Err(AuthError::SecondFactorRequired),
        }
    }
}

// ─── AuthClient (entry point) ────────────────────────────────────────────────

/// The unauthenticated entry point: turns credentials (or a persisted session)
/// into an authenticated [`Session`].
#[derive(Clone)]
pub struct AuthClient {
    http: reqwest::Client,
    base: Arc<AuthEndpoints>,
    clock: Arc<dyn Clock>,
    refresh_skew_secs: i64,
    /// The advisory device-cohort hash to ride every session-creation request
    /// (slice `S-D11`). `None` ⇒ nothing is sent — the server behaves identically
    /// (S-C13's advisory-only invariant). Computed by the platform app from its
    /// primary identifier via [`crate::cohort`] and set with
    /// [`with_cohort_hash`](AuthClient::with_cohort_hash).
    cohort_hash: Option<Arc<str>>,
}

impl AuthClient {
    /// Build a client against the auth base URL (e.g. `https://api.example.com/auth`).
    pub fn new(base_url: &str) -> Result<Self, AuthError> {
        // The SDK's one HTTP client: every request this client sends — and every request a
        // `Session` built from it executes on behalf of the upload, album and verify paths —
        // carries the protocol handshake the server's gate requires.
        let http = crate::net::http_client().map_err(AuthError::Transport)?;
        Self::from_parts(
            base_url,
            Arc::new(SystemClock),
            http,
            DEFAULT_REFRESH_SKEW_SECS,
        )
    }

    /// Assemble a client from explicit parts (clock + HTTP client + skew). Used by
    /// [`AuthClient::new`] and by tests that inject a controllable clock.
    ///
    /// `http` **must** come from [`crate::net::http_builder`] or [`crate::net::http_client`]: a
    /// client built any other way sends no protocol handshake, and every gated route refuses it.
    fn from_parts(
        base_url: &str,
        clock: Arc<dyn Clock>,
        http: reqwest::Client,
        refresh_skew_secs: i64,
    ) -> Result<Self, AuthError> {
        Ok(Self {
            http,
            base: Arc::new(AuthEndpoints::from_base(base_url)?),
            clock,
            refresh_skew_secs,
            cohort_hash: None,
        })
    }

    /// Attach the advisory device-cohort hash that rides every subsequent
    /// session-creation request (login/register) from this client (slice `S-D11`).
    ///
    /// The value is a lowercase-hex SHA-256 digest computed by the platform app from
    /// its primary identifier and the account's `user_id`
    /// ([`crate::cohort::compute_cohort_hash`]). It is a **grouping aid only** — the
    /// server never reads it for any authorization decision (S-C13). Not setting it
    /// (the default) is fully legal: nothing is sent and the server behaves
    /// identically.
    #[must_use]
    pub fn with_cohort_hash(mut self, cohort_hash: String) -> Self {
        self.cohort_hash = Some(Arc::from(cohort_hash));
        self
    }

    /// The advisory cohort hash this client rides, if configured.
    fn cohort(&self) -> Option<&str> {
        self.cohort_hash.as_deref()
    }

    /// Authenticate with email + password, returning an authenticated [`Session`].
    ///
    /// If a cohort hash is configured ([`with_cohort_hash`](AuthClient::with_cohort_hash))
    /// it rides the request body; otherwise the field is omitted entirely.
    #[instrument(skip_all)]
    pub async fn login(&self, email: &str, password: &str) -> Result<LoginOutcome, AuthError> {
        tracing::info!(
            cohort_emitted = self.cohort().is_some(),
            "authenticating via password login"
        );
        let response = self
            .http
            .post(&self.base.login)
            .json(&LoginRequestBody {
                email,
                password,
                cohort_hash: self.cohort(),
            })
            .send()
            .await?;

        // `202 Accepted` — the password verified and the sign-in is not finished. Read from the
        // **status**, which is where the server puts the distinction; a body flag would be a
        // second place for the two to disagree. Before `S-C63` this fell through to
        // `read_tokens` and surfaced as `MalformedResponse`, which told a user with a second
        // factor that their server was broken.
        if response.status() == reqwest::StatusCode::ACCEPTED {
            let challenge: SecondFactorChallengeBody =
                response
                    .json()
                    .await
                    .map_err(|e| AuthError::MalformedResponse {
                        endpoint: Endpoint::Login.name(),
                        reason: e.to_string(),
                    })?;
            tracing::info!("login needs a second factor");
            return Ok(LoginOutcome::SecondFactorRequired {
                mfa_token: SecretString::from(challenge.mfa_token),
                expires_by: challenge.expires_by,
            });
        }

        let tokens = read_tokens(Endpoint::Login, response).await?;
        tracing::info!("login succeeded; session established");
        Ok(LoginOutcome::Session(self.session_with_tokens(tokens)))
    }

    /// Complete a sign-in with the code an authenticator app is showing (`S-C55`).
    ///
    /// `mfa_token` is the challenge [`LoginOutcome::SecondFactorRequired`] carried. It is good
    /// once and for five minutes; a code is good once, full stop, so a retry after a wrong code
    /// re-uses the same challenge and a retry after a *right* one has to start from the password.
    #[instrument(skip_all)]
    pub async fn verify_second_factor(
        &self,
        mfa_token: &SecretString,
        totp_code: &str,
    ) -> Result<Session, AuthError> {
        let response = self
            .http
            .post(&self.base.verify_totp)
            .json(&VerifyTotpRequestBody {
                mfa_token: mfa_token.expose_secret(),
                totp_code,
                cohort_hash: self.cohort(),
            })
            .send()
            .await?;
        let tokens = read_tokens(Endpoint::VerifyTotp, response).await?;
        tracing::info!("second factor accepted; session established");
        Ok(self.session_with_tokens(tokens))
    }

    /// Create a new account and return an authenticated [`Session`] (the server
    /// issues tokens on registration). The configured cohort hash rides the body
    /// under the same advisory contract as [`login`](AuthClient::login).
    #[instrument(skip_all)]
    pub async fn register(&self, email: &str, password: &str) -> Result<Session, AuthError> {
        tracing::info!(
            cohort_emitted = self.cohort().is_some(),
            "registering a new account"
        );
        let response = self
            .http
            .post(&self.base.register)
            .json(&RegisterRequestBody { email, password })
            .send()
            .await?;
        let tokens = read_tokens(Endpoint::Register, response).await?;
        tracing::info!("registration succeeded; session established");
        Ok(self.session_with_tokens(tokens))
    }

    /// Rebuild a [`Session`] from a persisted token set (e.g. loaded from the
    /// platform keychain at launch). The next request pre-flight-refreshes if the
    /// restored access token is already stale.
    pub fn resume(&self, persisted: PersistedSession) -> Result<Session, AuthError> {
        let access_expires_at =
            Timestamp::from_second(persisted.access_expires_at_unix).map_err(|e| {
                AuthError::MalformedResponse {
                    endpoint: "resume",
                    reason: e.to_string(),
                }
            })?;
        Ok(self.session_with_tokens(TokenSet {
            access_token: persisted.access_token,
            refresh_token: persisted.refresh_token,
            access_expires_at,
        }))
    }

    fn session_with_tokens(&self, tokens: TokenSet) -> Session {
        Session {
            http: self.http.clone(),
            base: self.base.clone(),
            clock: self.clock.clone(),
            refresh_skew_secs: self.refresh_skew_secs,
            inner: Arc::new(SessionInner {
                tokens: RwLock::new(Some(tokens)),
                refresh_gate: Mutex::new(()),
            }),
        }
    }
}

// ─── Session (authenticated handle) ──────────────────────────────────────────

/// Shared mutable session state. `tokens` is the store; `refresh_gate` serializes
/// refreshes so concurrent callers single-flight.
struct SessionInner {
    tokens: RwLock<Option<TokenSet>>,
    refresh_gate: Mutex<()>,
}

/// What prompted a refresh, and therefore how to decide (under the single-flight
/// gate) whether the refresh is still needed or a concurrent caller already did it.
enum RefreshTrigger {
    /// The pre-flight expiry check fired; skip if the stored token is now fresh.
    PreFlight,
    /// A request was rejected with `401` carrying this (now-stale) access token;
    /// skip if the stored token has since been rotated to a different one.
    Rejected(String),
}

impl RefreshTrigger {
    fn label(&self) -> &'static str {
        match self {
            Self::PreFlight => "pre_flight",
            Self::Rejected(_) => "unauthorized",
        }
    }
}

/// An authenticated session. Cheaply cloneable — every clone shares one token
/// store and one single-flight gate, so refreshes coalesce across clones and tasks.
#[derive(Clone)]
pub struct Session {
    http: reqwest::Client,
    base: Arc<AuthEndpoints>,
    clock: Arc<dyn Clock>,
    refresh_skew_secs: i64,
    inner: Arc<SessionInner>,
}

impl Session {
    /// Whether the session currently holds tokens.
    pub async fn is_authenticated(&self) -> bool {
        self.inner.tokens.read().await.is_some()
    }

    /// Snapshot the token material for the platform's secure storage. `None` if the
    /// session has been logged out.
    pub async fn export(&self) -> Option<PersistedSession> {
        let guard = self.inner.tokens.read().await;
        guard.as_ref().map(|tokens| PersistedSession {
            access_token: tokens.access_token.clone(),
            refresh_token: tokens.refresh_token.clone(),
            access_expires_at_unix: tokens.access_expires_at.as_second(),
        })
    }

    /// Run an authenticated request, injecting the bearer token and recovering from
    /// a `401` by refreshing once and replaying.
    ///
    /// `build` is the caller's un-authenticated request (path, method, body); the
    /// SDK adds the `Authorization` header, so callers never see the token. `build`
    /// is invoked again for the retry, so the request is reconstructed with the
    /// fresh token (no body-clone requirement).
    #[instrument(skip_all)]
    pub async fn execute<F>(&self, build: F) -> Result<reqwest::Response, AuthError>
    where
        F: Fn(&reqwest::Client) -> reqwest::RequestBuilder,
    {
        let token = self.valid_access_token().await?;
        let response = build(&self.http)
            .bearer_auth(token.expose_secret())
            .send()
            .await?;
        if response.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(response);
        }

        tracing::info!("authenticated request returned 401; refreshing once and retrying");
        let stale = token.expose_secret().to_string();
        let fresh = self
            .ensure_refreshed(RefreshTrigger::Rejected(stale))
            .await?;
        let retried = build(&self.http)
            .bearer_auth(fresh.expose_secret())
            .send()
            .await?;
        Ok(retried)
    }

    /// Force a refresh now (single-flight). A no-op on the wire if a concurrent
    /// caller already rotated the token.
    #[instrument(skip_all)]
    pub async fn refresh(&self) -> Result<(), AuthError> {
        let current = {
            let guard = self.inner.tokens.read().await;
            guard
                .as_ref()
                .ok_or(AuthError::NotAuthenticated)?
                .access_token
                .expose_secret()
                .to_string()
        };
        self.ensure_refreshed(RefreshTrigger::Rejected(current))
            .await?;
        Ok(())
    }

    /// Revoke the session server-side and clear the local store. Idempotent: a
    /// server that no longer honors the token (or an already-empty store) still
    /// resolves to a cleared, logged-out session.
    #[instrument(skip_all)]
    pub async fn logout(&self) -> Result<(), AuthError> {
        let token = match self.valid_access_token().await {
            Ok(token) => token,
            Err(AuthError::SessionExpired | AuthError::NotAuthenticated) => {
                // Nothing the server can still revoke; drop local state and succeed.
                *self.inner.tokens.write().await = None;
                return Ok(());
            }
            Err(other) => return Err(other),
        };
        let response = self
            .http
            .post(&self.base.logout)
            .bearer_auth(token.expose_secret())
            .send()
            .await?;
        *self.inner.tokens.write().await = None;
        let status = response.status();
        if status.is_success() || status == reqwest::StatusCode::UNAUTHORIZED {
            tracing::info!("session logged out; local tokens cleared");
            Ok(())
        } else {
            Err(error_from_response(Endpoint::Logout, response).await)
        }
    }

    /// A currently-valid **bearer access token** for injecting into a request the
    /// SDK does not build with [`Session::execute`] — notably the sync feed's gRPC
    /// call metadata, where the token rides `authorization` metadata rather than a
    /// `reqwest` header. Pre-flight-refreshes exactly like [`Session::execute`];
    /// callers that get an `Unauthenticated`/`401` back re-[`refresh`](Session::refresh)
    /// and read a fresh token once.
    #[instrument(skip_all)]
    pub async fn bearer(&self) -> Result<SecretString, AuthError> {
        self.valid_access_token().await
    }

    /// Return a currently-valid access token, pre-flight-refreshing if the stored
    /// one is within the refresh skew of expiry. The fast path is a shared read; the
    /// refresh path single-flights.
    #[instrument(skip_all)]
    async fn valid_access_token(&self) -> Result<SecretString, AuthError> {
        {
            let guard = self.inner.tokens.read().await;
            let tokens = guard.as_ref().ok_or(AuthError::NotAuthenticated)?;
            if !self.needs_refresh(tokens) {
                return Ok(tokens.access_token.clone());
            }
        }
        tracing::debug!("access token within refresh skew; pre-flight refreshing");
        self.ensure_refreshed(RefreshTrigger::PreFlight).await
    }

    fn needs_refresh(&self, tokens: &TokenSet) -> bool {
        let now = self.clock.now().as_second();
        now + self.refresh_skew_secs >= tokens.access_expires_at.as_second()
    }

    /// Single-flight refresh. The gate serializes callers; the double-check under
    /// the gate coalesces everyone who piled up behind the one caller that hit the
    /// network, so N concurrent stale-token callers produce exactly one refresh.
    #[instrument(skip_all, fields(trigger = trigger.label()))]
    async fn ensure_refreshed(&self, trigger: RefreshTrigger) -> Result<SecretString, AuthError> {
        let _gate = self.inner.refresh_gate.lock().await;

        let refresh_token = {
            let guard = self.inner.tokens.read().await;
            let tokens = guard.as_ref().ok_or(AuthError::NotAuthenticated)?;
            let still_stale = match &trigger {
                RefreshTrigger::PreFlight => self.needs_refresh(tokens),
                RefreshTrigger::Rejected(stale) => tokens.access_token.expose_secret() == stale,
            };
            if !still_stale {
                tracing::debug!("refresh coalesced with a concurrent single-flight refresh");
                return Ok(tokens.access_token.clone());
            }
            tokens.refresh_token.clone()
        };

        tracing::info!("refreshing access token via session refresh token");
        let new_tokens = self.do_refresh(&refresh_token).await?;
        let access_token = new_tokens.access_token.clone();
        *self.inner.tokens.write().await = Some(new_tokens);
        Ok(access_token)
    }

    #[instrument(skip_all)]
    async fn do_refresh(&self, refresh_token: &SecretString) -> Result<TokenSet, AuthError> {
        let response = self
            .http
            .post(&self.base.refresh)
            .json(&RefreshRequestBody {
                refresh_token: refresh_token.expose_secret(),
            })
            .send()
            .await?;
        read_tokens(Endpoint::Refresh, response).await
    }
}

// ─── Shared response parsing ─────────────────────────────────────────────────

/// Parse a `TokenResponse` on success, or map the status/body to a typed error.
async fn read_tokens(
    endpoint: Endpoint,
    response: reqwest::Response,
) -> Result<TokenSet, AuthError> {
    if !response.status().is_success() {
        return Err(error_from_response(endpoint, response).await);
    }
    let body: TokenResponseBody =
        response
            .json()
            .await
            .map_err(|e| AuthError::MalformedResponse {
                endpoint: endpoint.name(),
                reason: e.to_string(),
            })?;
    TokenSet::from_wire(endpoint, body)
}

/// Map a non-success response to a typed [`AuthError`], capturing the server's
/// `error.*` code and `Retry-After` where present.
async fn error_from_response(endpoint: Endpoint, response: reqwest::Response) -> AuthError {
    let status = response.status();
    let retry_after = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.trim().parse::<u64>().ok());
    let api_error = response.json::<ApiErrorBody>().await.ok();
    let code = api_error.as_ref().and_then(|body| body.code.clone());
    let detail = api_error.map_or_else(String::new, |body| body.error);

    match status.as_u16() {
        401 => endpoint.unauthorized_error(),
        423 => AuthError::AccountLocked,
        429 => AuthError::RateLimited {
            retry_after_secs: retry_after.unwrap_or(0),
        },
        other => AuthError::Unexpected {
            status: other,
            endpoint: endpoint.name(),
            detail,
            code,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fmt::Write as _;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Notify;

    use super::*;

    // ── Mock clock ────────────────────────────────────────────────────────────

    struct MockClock {
        now: std::sync::Mutex<i64>,
    }

    impl MockClock {
        fn new(base: i64) -> Arc<Self> {
            Arc::new(Self {
                now: std::sync::Mutex::new(base),
            })
        }
        fn advance(&self, secs: i64) {
            *self.now.lock().unwrap() += secs;
        }
    }

    impl Clock for MockClock {
        fn now(&self) -> Timestamp {
            Timestamp::from_second(*self.now.lock().unwrap()).unwrap()
        }
    }

    // ── Mock HTTP server ──────────────────────────────────────────────────────

    struct MockRequest {
        path: String,
        headers: HashMap<String, String>,
        body: String,
    }

    struct MockResponse {
        status: u16,
        headers: Vec<(String, String)>,
        body: String,
    }

    impl MockResponse {
        fn json(status: u16, body: impl Into<String>) -> Self {
            Self {
                status,
                headers: Vec::new(),
                body: body.into(),
            }
        }
        fn header(mut self, key: &str, value: &str) -> Self {
            self.headers.push((key.to_string(), value.to_string()));
            self
        }
    }

    type BoxFut = Pin<Box<dyn Future<Output = MockResponse> + Send>>;
    type Handler = Arc<dyn Fn(MockRequest) -> BoxFut + Send + Sync>;

    struct MockServer {
        base_url: String,
    }

    async fn start_mock(handler: Handler) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let handler = handler.clone();
                tokio::spawn(async move {
                    let _ = serve_conn(&mut socket, handler).await;
                });
            }
        });
        MockServer {
            base_url: format!("http://{addr}"),
        }
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }

    async fn serve_conn(socket: &mut TcpStream, handler: Handler) -> std::io::Result<()> {
        let mut buf = Vec::new();
        let mut tmp = [0u8; 1024];
        let header_end = loop {
            if let Some(pos) = find_subsequence(&buf, b"\r\n\r\n") {
                break pos + 4;
            }
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&tmp[..n]);
        };

        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let mut lines = head.split("\r\n");
        let request_line = lines.next().unwrap_or_default();
        let path = request_line
            .split_whitespace()
            .nth(1)
            .unwrap_or_default()
            .to_string();

        let mut headers = HashMap::new();
        let mut content_length = 0usize;
        for line in lines {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_lowercase();
                let value = value.trim().to_string();
                if key == "content-length" {
                    content_length = value.parse().unwrap_or(0);
                }
                headers.insert(key, value);
            }
        }

        let mut body = buf[header_end..].to_vec();
        while body.len() < content_length {
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }

        let body_str = String::from_utf8_lossy(&body).to_string();
        let response = handler(MockRequest {
            path,
            headers,
            body: body_str,
        })
        .await;
        let mut payload = format!(
            "HTTP/1.1 {} STATUS\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
            response.status,
            response.body.len()
        );
        for (key, value) in &response.headers {
            let _ = write!(payload, "{key}: {value}\r\n");
        }
        payload.push_str("\r\n");
        payload.push_str(&response.body);
        socket.write_all(payload.as_bytes()).await?;
        socket.flush().await?;
        Ok(())
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    fn token_json(access: &str, refresh: &str, expires_by: i64) -> String {
        serde_json::json!({
            "access_token": access,
            "refresh_token": refresh,
            "token_type": "Bearer",
            "expires_by": expires_by,
        })
        .to_string()
    }

    fn far_future() -> i64 {
        Timestamp::now().as_second() + 3600
    }

    /// Extract the error from a login result whose `Ok` type ([`Session`]) is not
    /// `Debug`, so `unwrap_err` cannot be used directly.
    fn expect_login_err(result: Result<LoginOutcome, AuthError>) -> AuthError {
        match result {
            Ok(_) => panic!("expected login to fail"),
            Err(error) => error,
        }
    }

    /// The session from a login that finished, for the cases that are not about the second
    /// factor. Panics rather than returning a `Result`: a test whose fixture answers `202` when
    /// it meant to answer `200` has nothing left to assert.
    fn finished(outcome: LoginOutcome) -> Session {
        match outcome {
            LoginOutcome::Session(session) => session,
            LoginOutcome::SecondFactorRequired { .. } => {
                panic!("the fixture answered a second-factor challenge")
            }
        }
    }

    async fn stored_access(session: &Session) -> String {
        session
            .inner
            .tokens
            .read()
            .await
            .as_ref()
            .unwrap()
            .access_token
            .expose_secret()
            .to_string()
    }

    // ── Tests ─────────────────────────────────────────────────────────────────

    /// Wire round-trip: login stores the pair, an explicit refresh rotates it, and
    /// logout revokes + clears local state.
    #[tokio::test]
    async fn login_refresh_logout_round_trip() {
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let logout_calls = Arc::new(AtomicUsize::new(0));
        let rc = refresh_calls.clone();
        let lc = logout_calls.clone();
        let handler: Handler = Arc::new(move |req| {
            let rc = rc.clone();
            let lc = lc.clone();
            Box::pin(async move {
                let far = far_future();
                match req.path.as_str() {
                    "/login" => MockResponse::json(200, token_json("access-1", "refresh-1", far)),
                    "/refresh" => {
                        rc.fetch_add(1, Ordering::SeqCst);
                        MockResponse::json(200, token_json("access-2", "refresh-2", far))
                    }
                    "/logout" => {
                        assert_eq!(
                            req.headers.get("authorization").map(String::as_str),
                            Some("Bearer access-2")
                        );
                        lc.fetch_add(1, Ordering::SeqCst);
                        MockResponse::json(200, r#"{"error":"Logout successful"}"#)
                    }
                    other => MockResponse::json(404, format!(r#"{{"error":"no {other}"}}"#)),
                }
            })
        });

        let server = start_mock(handler).await;
        let client = AuthClient::new(&server.base_url).unwrap();
        let session = finished(client.login("a@example.com", "pw").await.unwrap());

        assert!(session.is_authenticated().await);
        assert_eq!(stored_access(&session).await, "access-1");

        session.refresh().await.unwrap();
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
        assert_eq!(stored_access(&session).await, "access-2");

        session.logout().await.unwrap();
        assert_eq!(logout_calls.load(Ordering::SeqCst), 1);
        assert!(!session.is_authenticated().await);
    }

    /// Pre-flight refresh: with a mocked clock, no refresh happens while the token
    /// is comfortably valid, and exactly one fires the instant the clock crosses
    /// `expiry - skew`. Deterministic, no sleeps.
    #[tokio::test]
    async fn preflight_refresh_before_expiry_with_mock_clock() {
        const BASE: i64 = 1_000_000_000;
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let rc = refresh_calls.clone();
        let handler: Handler = Arc::new(move |req| {
            let rc = rc.clone();
            Box::pin(async move {
                match req.path.as_str() {
                    "/login" => {
                        MockResponse::json(200, token_json("access-1", "refresh-1", BASE + 100))
                    }
                    "/refresh" => {
                        rc.fetch_add(1, Ordering::SeqCst);
                        MockResponse::json(200, token_json("access-2", "refresh-2", BASE + 1000))
                    }
                    _ => MockResponse::json(404, r#"{"error":"x"}"#),
                }
            })
        });

        let server = start_mock(handler).await;
        let clock = MockClock::new(BASE);
        let dyn_clock: Arc<dyn Clock> = clock.clone();
        let http = reqwest::Client::builder().build().unwrap();
        let client = AuthClient::from_parts(&server.base_url, dyn_clock, http, 30).unwrap();
        let session = finished(client.login("a@example.com", "pw").await.unwrap());

        // now = BASE, expiry = BASE+100, skew 30 → BASE+30 < BASE+100 → no refresh.
        assert_eq!(
            session.valid_access_token().await.unwrap().expose_secret(),
            "access-1"
        );
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 0);

        // Advance to BASE+80 → BASE+110 >= BASE+100 → pre-flight refresh fires once.
        clock.advance(80);
        assert_eq!(
            session.valid_access_token().await.unwrap().expose_secret(),
            "access-2"
        );
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);

        // access-2 expires at BASE+1000 → still fresh → no further refresh.
        assert_eq!(
            session.valid_access_token().await.unwrap().expose_secret(),
            "access-2"
        );
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
    }

    /// Single-flight: eight concurrent callers all see the token as expired, but the
    /// gate + double-check coalesce them onto exactly one network refresh, and all
    /// eight observe the same rotated token. The mock holds the one in-flight
    /// refresh until released, so the concurrency is real, not incidental.
    #[tokio::test]
    async fn single_flight_coalesces_concurrent_refreshes() {
        const BASE: i64 = 2_000_000_000;
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let started = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        let rc = refresh_calls.clone();
        let started_h = started.clone();
        let release_h = release.clone();
        let handler: Handler = Arc::new(move |req| {
            let rc = rc.clone();
            let started = started_h.clone();
            let release = release_h.clone();
            Box::pin(async move {
                match req.path.as_str() {
                    // access-1 expires exactly at BASE → stale at clock=BASE.
                    "/login" => MockResponse::json(200, token_json("access-1", "refresh-1", BASE)),
                    "/refresh" => {
                        rc.fetch_add(1, Ordering::SeqCst);
                        started.notify_one();
                        release.notified().await;
                        MockResponse::json(200, token_json("access-2", "refresh-2", BASE + 1000))
                    }
                    _ => MockResponse::json(404, r#"{"error":"x"}"#),
                }
            })
        });

        let server = start_mock(handler).await;
        let clock = MockClock::new(BASE);
        let dyn_clock: Arc<dyn Clock> = clock.clone();
        let http = reqwest::Client::builder().build().unwrap();
        let client = AuthClient::from_parts(&server.base_url, dyn_clock, http, 30).unwrap();
        let session = finished(client.login("a@example.com", "pw").await.unwrap());

        let mut handles = Vec::new();
        for _ in 0..8 {
            let s = session.clone();
            handles.push(tokio::spawn(async move { s.valid_access_token().await }));
        }

        // Exactly one refresh reaches the network; wait for it, then release it.
        started.notified().await;
        release.notify_one();

        let mut tokens = Vec::new();
        for handle in handles {
            tokens.push(handle.await.unwrap().unwrap());
        }

        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
        for token in &tokens {
            assert_eq!(token.expose_secret(), "access-2");
        }
    }

    /// A `401` on an authenticated request refreshes once and replays with the new
    /// token: the protected endpoint is hit twice, the token refreshed once.
    #[tokio::test]
    async fn unauthorized_triggers_single_refresh_and_retry() {
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let protected_calls = Arc::new(AtomicUsize::new(0));
        let rc = refresh_calls.clone();
        let pc = protected_calls.clone();
        let handler: Handler = Arc::new(move |req| {
            let rc = rc.clone();
            let pc = pc.clone();
            Box::pin(async move {
                let far = far_future();
                match req.path.as_str() {
                    "/login" => MockResponse::json(200, token_json("access-1", "refresh-1", far)),
                    "/refresh" => {
                        rc.fetch_add(1, Ordering::SeqCst);
                        MockResponse::json(200, token_json("access-2", "refresh-2", far))
                    }
                    "/protected" => {
                        pc.fetch_add(1, Ordering::SeqCst);
                        let auth = req
                            .headers
                            .get("authorization")
                            .cloned()
                            .unwrap_or_default();
                        if auth == "Bearer access-1" {
                            MockResponse::json(401, r#"{"error":"expired"}"#)
                        } else {
                            MockResponse::json(200, r#"{"ok":true}"#)
                        }
                    }
                    _ => MockResponse::json(404, r#"{"error":"x"}"#),
                }
            })
        });

        let server = start_mock(handler).await;
        let client = AuthClient::new(&server.base_url).unwrap();
        let session = finished(client.login("a@example.com", "pw").await.unwrap());

        let base = server.base_url.clone();
        let response = session
            .execute(|http| http.get(format!("{base}/protected")))
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::OK);
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
        assert_eq!(protected_calls.load(Ordering::SeqCst), 2);
    }

    /// The `401` recovery retries at most once: a permanently-401 endpoint yields
    /// the `401` back to the caller after a single refresh + replay.
    #[tokio::test]
    async fn unauthorized_retries_at_most_once() {
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let protected_calls = Arc::new(AtomicUsize::new(0));
        let rc = refresh_calls.clone();
        let pc = protected_calls.clone();
        let handler: Handler = Arc::new(move |req| {
            let rc = rc.clone();
            let pc = pc.clone();
            Box::pin(async move {
                let far = far_future();
                match req.path.as_str() {
                    "/login" => MockResponse::json(200, token_json("access-1", "refresh-1", far)),
                    "/refresh" => {
                        rc.fetch_add(1, Ordering::SeqCst);
                        MockResponse::json(200, token_json("access-2", "refresh-2", far))
                    }
                    "/protected" => {
                        pc.fetch_add(1, Ordering::SeqCst);
                        MockResponse::json(401, r#"{"error":"nope"}"#)
                    }
                    _ => MockResponse::json(404, r#"{"error":"x"}"#),
                }
            })
        });

        let server = start_mock(handler).await;
        let client = AuthClient::new(&server.base_url).unwrap();
        let session = finished(client.login("a@example.com", "pw").await.unwrap());

        let base = server.base_url.clone();
        let response = session
            .execute(|http| http.get(format!("{base}/protected")))
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1);
        assert_eq!(protected_calls.load(Ordering::SeqCst), 2);
    }

    /// A `202` is a second-factor challenge, not a malformed token pair (`S-C55`, `S-C63`).
    ///
    /// Before this, `read_tokens` treated every 2xx as a pair and the challenge body failed to
    /// deserialize — so a user with a second factor was told their server had sent a malformed
    /// response.
    #[tokio::test]
    async fn a_202_login_is_a_second_factor_challenge() {
        let handler: Handler = Arc::new(move |req| {
            Box::pin(async move {
                match req.path.as_str() {
                    "/login" => MockResponse::json(
                        202,
                        r#"{"mfa_token":"challenge-1","expires_by":1893456000}"#,
                    ),
                    _ => MockResponse::json(404, r#"{"error":"x"}"#),
                }
            })
        });

        let server = start_mock(handler).await;
        let client = AuthClient::new(&server.base_url).unwrap();
        let outcome = client.login("a@example.com", "pw").await.unwrap();

        let LoginOutcome::SecondFactorRequired {
            mfa_token,
            expires_by,
        } = outcome
        else {
            panic!("a 202 is a challenge, not a session");
        };
        assert_eq!(mfa_token.expose_secret(), "challenge-1");
        assert_eq!(expires_by, 1_893_456_000);
    }

    /// The challenge and a code complete the sign-in, and the cohort rides *this* request.
    #[tokio::test]
    async fn a_code_completes_the_sign_in_and_carries_the_cohort() {
        let bodies = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen = Arc::clone(&bodies);
        let handler: Handler = Arc::new(move |req| {
            let seen = Arc::clone(&seen);
            Box::pin(async move {
                match req.path.as_str() {
                    "/login" => MockResponse::json(
                        202,
                        r#"{"mfa_token":"challenge-1","expires_by":1893456000}"#,
                    ),
                    "/login/verify-totp" => {
                        seen.lock().await.push(req.body.clone());
                        MockResponse::json(200, token_json("access-1", "refresh-1", 2_000_000_000))
                    }
                    _ => MockResponse::json(404, r#"{"error":"x"}"#),
                }
            })
        });

        let server = start_mock(handler).await;
        let client = AuthClient::new(&server.base_url)
            .unwrap()
            .with_cohort_hash("a-particular-machine".to_owned());

        let LoginOutcome::SecondFactorRequired { mfa_token, .. } =
            client.login("a@example.com", "pw").await.unwrap()
        else {
            panic!("expected a challenge");
        };
        let session = client
            .verify_second_factor(&mfa_token, "123456")
            .await
            .unwrap();
        assert!(session.is_authenticated().await);

        // The session is opened by the *completing* request, so the advisory cohort belongs
        // there — sent on the first leg it would describe a session that does not exist.
        let body = bodies.lock().await[0].clone();
        assert!(body.contains("a-particular-machine"), "{body}");
        assert!(body.contains("challenge-1"), "{body}");
    }

    /// A caller that cannot prompt gets a typed refusal rather than a confusing one.
    #[tokio::test]
    async fn into_session_refuses_a_challenge_it_cannot_answer() {
        let handler: Handler = Arc::new(move |req| {
            Box::pin(async move {
                match req.path.as_str() {
                    "/login" => MockResponse::json(
                        202,
                        r#"{"mfa_token":"challenge-1","expires_by":1893456000}"#,
                    ),
                    _ => MockResponse::json(404, r#"{"error":"x"}"#),
                }
            })
        });

        let server = start_mock(handler).await;
        let client = AuthClient::new(&server.base_url).unwrap();
        // `expect_err` needs `Debug` on the ok arm, and `Session` deliberately has none — it
        // holds live tokens.
        let error = match client
            .login("a@example.com", "pw")
            .await
            .unwrap()
            .into_session()
        {
            Ok(_) => panic!("a challenge is not a session"),
            Err(error) => error,
        };

        assert!(matches!(error, AuthError::SecondFactorRequired));
    }

    /// Login `401` maps to a typed error carrying the localizable catalog code.
    #[tokio::test]
    async fn login_invalid_credentials_maps_typed_error_and_code() {
        let handler: Handler = Arc::new(move |req| {
            Box::pin(async move {
                match req.path.as_str() {
                    "/login" => MockResponse::json(401, r#"{"error":"Invalid credentials"}"#),
                    _ => MockResponse::json(404, r#"{"error":"x"}"#),
                }
            })
        });

        let server = start_mock(handler).await;
        let client = AuthClient::new(&server.base_url).unwrap();
        let error = expect_login_err(client.login("a@example.com", "bad").await);

        assert!(matches!(error, AuthError::InvalidCredentials));
        assert_eq!(
            error.error_code(),
            Some(error_codes::AUTH_INVALID_CREDENTIALS)
        );
    }

    /// Login `429` maps to `RateLimited` with the `Retry-After` seconds and the
    /// catalog code.
    #[tokio::test]
    async fn login_rate_limited_maps_retry_after() {
        let handler: Handler = Arc::new(move |req| {
            Box::pin(async move {
                match req.path.as_str() {
                    "/login" => MockResponse::json(
                        429,
                        r#"{"error":"Too many requests","code":"error.auth.rate_limited"}"#,
                    )
                    .header("retry-after", "42"),
                    _ => MockResponse::json(404, r#"{"error":"x"}"#),
                }
            })
        });

        let server = start_mock(handler).await;
        let client = AuthClient::new(&server.base_url).unwrap();
        let error = expect_login_err(client.login("a@example.com", "pw").await);

        assert!(matches!(
            error,
            AuthError::RateLimited {
                retry_after_secs: 42
            }
        ));
        assert_eq!(error.error_code(), Some(error_codes::AUTH_RATE_LIMITED));
    }

    /// A refresh rejected with `401` surfaces `SessionExpired` (re-auth required).
    #[tokio::test]
    async fn refresh_rejected_maps_to_session_expired() {
        let handler: Handler = Arc::new(move |req| {
            Box::pin(async move {
                match req.path.as_str() {
                    "/login" => {
                        MockResponse::json(200, token_json("access-1", "refresh-1", far_future()))
                    }
                    "/refresh" => {
                        MockResponse::json(401, r#"{"error":"Invalid or expired token"}"#)
                    }
                    _ => MockResponse::json(404, r#"{"error":"x"}"#),
                }
            })
        });

        let server = start_mock(handler).await;
        let client = AuthClient::new(&server.base_url).unwrap();
        let session = finished(client.login("a@example.com", "pw").await.unwrap());
        let error = session.refresh().await.unwrap_err();

        assert!(matches!(error, AuthError::SessionExpired));
    }

    /// Export → resume rebuilds an authenticated session from persisted tokens.
    #[tokio::test]
    async fn export_and_resume_roundtrip_session() {
        let handler: Handler = Arc::new(move |req| {
            Box::pin(async move {
                match req.path.as_str() {
                    "/login" => {
                        MockResponse::json(200, token_json("access-1", "refresh-1", far_future()))
                    }
                    _ => MockResponse::json(404, r#"{"error":"x"}"#),
                }
            })
        });

        let server = start_mock(handler).await;
        let client = AuthClient::new(&server.base_url).unwrap();
        let session = finished(client.login("a@example.com", "pw").await.unwrap());

        let persisted = session.export().await.unwrap();
        assert_eq!(persisted.access_token.expose_secret(), "access-1");

        let resumed = client.resume(persisted).unwrap();
        assert!(resumed.is_authenticated().await);
        assert_eq!(stored_access(&resumed).await, "access-1");
    }

    /// An operation on an empty session reports `NotAuthenticated`.
    #[tokio::test]
    async fn refresh_without_session_is_not_authenticated() {
        let handler: Handler = Arc::new(move |_req| {
            Box::pin(async move { MockResponse::json(404, r#"{"error":"x"}"#) })
        });
        let server = start_mock(handler).await;
        let client = AuthClient::new(&server.base_url).unwrap();
        // A resumed-then-logged-out session has an empty store.
        let session = client
            .resume(PersistedSession {
                access_token: "a".into(),
                refresh_token: "r".into(),
                access_expires_at_unix: far_future(),
            })
            .unwrap();
        *session.inner.tokens.write().await = None;
        let error = session.refresh().await.unwrap_err();
        assert!(matches!(error, AuthError::NotAuthenticated));
    }

    #[test]
    fn rejects_invalid_base_url() {
        assert!(matches!(
            AuthClient::new("not a url"),
            Err(AuthError::InvalidBaseUrl { .. })
        ));
    }

    // ── Cohort emission (S-D11) ───────────────────────────────────────────────

    /// Capture the JSON body a session-creation endpoint received, so a test can
    /// assert what rode the wire.
    fn capturing_handler(captured: Arc<std::sync::Mutex<Option<serde_json::Value>>>) -> Handler {
        Arc::new(move |req: MockRequest| {
            let captured = captured.clone();
            Box::pin(async move {
                if matches!(req.path.as_str(), "/login" | "/register") {
                    *captured.lock().unwrap() = serde_json::from_str(&req.body).ok();
                }
                MockResponse::json(200, token_json("access-1", "refresh-1", far_future()))
            })
        })
    }

    /// The configured cohort hash rides the login body verbatim and lands server-side.
    #[tokio::test]
    async fn cohort_hash_rides_login_body() {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let server = start_mock(capturing_handler(captured.clone())).await;

        let client = AuthClient::new(&server.base_url)
            .unwrap()
            .with_cohort_hash("deadbeef".to_string());
        finished(client.login("a@example.com", "pw").await.unwrap());

        let body = captured
            .lock()
            .unwrap()
            .clone()
            .expect("login body captured");
        assert_eq!(body["email"], "a@example.com");
        assert_eq!(
            body["cohort_hash"], "deadbeef",
            "the advisory cohort must ride the session-creation body"
        );
    }

    /// Registration sends an address and a password, and **nothing else**.
    ///
    /// It used to send a username, a display name and the advisory cohort hash. The server takes
    /// none of them (`S-C53`), and its body is strict — so a field this client added for
    /// old times' sake would be a `422` rather than a value quietly ignored. Asserted as an
    /// exact key set, because the failure mode here is a field creeping back in.
    #[tokio::test]
    async fn registration_sends_only_an_address_and_a_password() {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let server = start_mock(capturing_handler(captured.clone())).await;

        let client = AuthClient::new(&server.base_url)
            .unwrap()
            .with_cohort_hash("cafef00d".to_string());
        client.register("john@example.com", "pw").await.unwrap();

        let body = captured
            .lock()
            .unwrap()
            .clone()
            .expect("register body captured");
        let keys: Vec<&str> = body
            .as_object()
            .expect("a JSON object")
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["email", "password"]);
    }

    /// Absent stays legal: with no cohort configured, the field is **omitted entirely**
    /// (no `null`), so the server behaves identically to a valid one (S-C13 invariant).
    #[tokio::test]
    async fn absent_cohort_is_omitted_from_the_body() {
        let captured = Arc::new(std::sync::Mutex::new(None));
        let server = start_mock(capturing_handler(captured.clone())).await;

        let client = AuthClient::new(&server.base_url).unwrap();
        finished(client.login("a@example.com", "pw").await.unwrap());

        let body = captured
            .lock()
            .unwrap()
            .clone()
            .expect("login body captured");
        assert!(
            body.get("cohort_hash").is_none(),
            "an unset cohort must not appear on the wire, not even as null"
        );
    }
}
