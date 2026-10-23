//! SDK-owned authentication: the session/token store and the auto-refresh engine.
//!
//! This is the "SDK owns the complete user flow" foundation: native apps call
//! [`AuthClient::login`], then hand every request through [`Session::execute`], then
//! [`Session::logout`] — they never juggle raw access tokens. The store keeps the access + refresh
//! token pair and:
//!
//! - runs a quick **asynchronous pre-flight expiry check** before each request and
//!   refreshes proactively when the access token is within the refresh skew of
//!   expiry (`Session::valid_access_token`);
//! - **single-flights** refreshes — concurrent callers that all see a stale token
//!   coalesce onto one network refresh (`Session::ensure_refreshed`);
//! - **retries once on `401`**: an authenticated request that the server rejects is
//!   refreshed once and replayed, so a token that expired between the pre-flight
//!   check and the wire is transparently recovered.
//!
//! It is hand-rolled over `reqwest` (rustls only) against the server's own
//! `/v1/auth/{register,login,refresh,logout}`. It does not
//! route through the generated client, but the reason is no longer that spargen is
//! parked — spargen ships and generates the typed surface today. What lives here
//! is token *orchestration*: the pre-flight refresh, the `401`-retry-once replay, and
//! the session store they mutate. That is deliberately outside generated code, which
//! owns parsing and serialization and nothing else. Auth requests are
//! [`crate::net::RetryClass::Interactive`]; the full backoff ladder lives in [`crate::net`], but
//! the `401`-retry-once and pre-flight
//! refresh here are the parts the session store owns.
//!
//! The OIDC legs ([`AuthClient::begin_oidc_login`] and
//! [`AuthClient::complete_oidc_login`]) are **not** hand-rolled: neither is token
//! orchestration, so the exemption above does not cover them, and they call the generated
//! [`rest::Client`] — every body and every response parsed by generated code, with only the
//! mapping into [`LoginOutcome`] and [`AuthError`] written here. The browser leg between the
//! two is the platform's: a loopback listener on the CLI, `ASWebAuthenticationSession` on iOS.
//! The ceremony is bound to the client that began it: the begin leg mints a secret the
//! [`OidcAuthorization`] keeps and sends only its hash, and the complete leg presents the secret
//! and refuses a redirect carrying a `state` it did not begin (issue #517).
//!
//! The device-grant legs (RFC 8628, issue #635: [`AuthClient::begin_oidc_device_login`] and
//! [`AuthClient::poll_oidc_device_login`]) are the same kind of thing: two generated calls, the
//! begin binding the ceremony to this client the same way, and the poll answering
//! [`OidcDevicePoll`] — keep polling, slow down, or the [`LoginOutcome`] a password login gives.
//! The wait between polls is the caller's; these send one request each.
//!
//! ## Testing
//!
//! The wire flows (login/refresh/logout, `401` recovery, error mapping) are proven
//! against a focused in-process mock HTTP server rather than a booted server: booting one
//! from this crate would invert the dependency — the *server* dev-depends on this crate, not
//! the other way round — and a mock keeps every wire case deterministic. The round
//! trip against the real router, over a real socket, lives where the dependency points:
//! `capsule-server/tests/sdk_client.rs`. The pre-flight and single-flight guarantees are proven with an injected
//! [`Clock`] — no sleeps, no wall-clock dependence.

use std::sync::Arc;

use capsule_core::crypto::primitives::PROTOCOL_VERSION;
use capsule_i18n::error_codes;
use jiff::Timestamp;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tracing::instrument;

use crate::net::negotiation::{self, UpgradeRequired};
use crate::rest;

/// Every request this module sends goes through the protocol window (issue #674): every
/// response's window is latched, and a `426` is [`AuthError::UpgradeRequired`] rather than a
/// status a caller has to recognise.
///
/// Two strengths, by what the request is for:
///
/// - [`send_negotiated`](NegotiatedSend::send_negotiated) — the data requests a [`Session`]
///   executes on behalf of the upload, album, verify and upgrade paths: a write the latched
///   window refuses also never leaves.
/// - [`send_screened`](NegotiatedSend::send_screened) — the session's own upkeep (sign-in,
///   second factor, registration, refresh, sign-out): never refused here, only by the server's
///   own `426`. Reads keep working against any server (Versioning — Update Policy), and a read
///   needs a session; refusing a refresh because the advisory build floor is above this build
///   would make every read fail the moment its token expired.
trait NegotiatedSend {
    /// [`reqwest::RequestBuilder::send`], pre-flighted against the latched window and screened.
    async fn send_negotiated(self) -> Result<reqwest::Response, AuthError>;
    /// [`reqwest::RequestBuilder::send`], screened only.
    async fn send_screened(self) -> Result<reqwest::Response, AuthError>;
}

impl NegotiatedSend for reqwest::RequestBuilder {
    async fn send_negotiated(self) -> Result<reqwest::Response, AuthError> {
        Ok(negotiation::send_negotiated(self).await?)
    }

    async fn send_screened(self) -> Result<reqwest::Response, AuthError> {
        Ok(negotiation::send_screened(self).await?)
    }
}

impl From<negotiation::SendError> for AuthError {
    fn from(error: negotiation::SendError) -> Self {
        match error {
            negotiation::SendError::Upgrade(refusal) => refusal.into(),
            negotiation::SendError::Transport(error) => Self::Transport(error),
        }
    }
}

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
    /// The server will not take this client's writes: it answered `426`, or the window it
    /// advertised already refuses this build (issue #674). Update the app.
    #[error(transparent)]
    UpgradeRequired(Box<UpgradeRequired>),
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

    /// The server has no identity provider (`error.auth.oidc_not_configured`).
    ///
    /// A client that read `auth.oidc: null` from `server-info` never sees this; one that offered
    /// the option anyway does.
    #[error("single sign-on is not configured on this server")]
    OidcNotConfigured,
    /// The redirect URI this client asked for is not one the server admits
    /// (`error.auth.oidc_redirect_invalid`). A client or deployment misconfiguration.
    #[error("the server will not send a person back to this redirect URI")]
    OidcRedirectInvalid,
    /// The callback was refused: the ceremony expired or was replayed, the provider refused the
    /// exchange, or the ID token failed a check. Every one means "start the sign-in again", so
    /// they are one variant; `code` says which for a log line.
    #[error("the sign-in through the identity provider was refused ({})", code.as_deref().unwrap_or("no code"))]
    OidcRejected {
        /// The server's `error.auth.oidc_*` code, when it sent one.
        code: Option<String>,
    },
    /// The identity provider asserted an address that already has a local account
    /// (`error.auth.oidc_address_taken`). Never linked: the person signs in with that account's
    /// password instead.
    #[error("an account with that address already exists; sign in with its password")]
    OidcAddressTaken,
    /// The server's identity provider does not offer the device authorization grant
    /// (`error.auth.oidc_device_unsupported`): sign in with the browser flow instead.
    #[error("the identity provider does not offer signing in without a browser")]
    OidcDeviceUnsupported,
    /// The person declined the device sign-in at the identity provider
    /// (`error.auth.oidc_device_denied`).
    #[error("the sign-in was declined at the identity provider")]
    OidcDeviceDenied,
    /// The device sign-in is over: expired, already used, or not this client's
    /// (`error.auth.oidc_device_expired`). Start again.
    #[error("the sign-in code expired or was already used")]
    OidcDeviceExpired,
    /// The generated client could not reach the server, or could not build the request.
    ///
    /// The generated client classifies its transport failures itself (DNS, connection, TLS,
    /// timeout, redirect policy) and they are not `reqwest::Error`s, so they cannot ride
    /// [`AuthError::Transport`]; the class and the endpoint are what a caller acts on.
    #[error("could not reach {endpoint}: {detail}")]
    Network {
        /// Which auth endpoint was being reached.
        endpoint: &'static str,
        /// The generated client's own description.
        detail: String,
    },
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

impl From<UpgradeRequired> for AuthError {
    fn from(refusal: UpgradeRequired) -> Self {
        Self::UpgradeRequired(Box::new(refusal))
    }
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
            Self::OidcNotConfigured => Some(error_codes::AUTH_OIDC_NOT_CONFIGURED),
            Self::OidcRedirectInvalid => Some(error_codes::AUTH_OIDC_REDIRECT_INVALID),
            Self::OidcAddressTaken => Some(error_codes::AUTH_OIDC_ADDRESS_TAKEN),
            Self::OidcDeviceUnsupported => Some(error_codes::AUTH_OIDC_DEVICE_UNSUPPORTED),
            Self::OidcDeviceDenied => Some(error_codes::AUTH_OIDC_DEVICE_DENIED),
            Self::OidcDeviceExpired => Some(error_codes::AUTH_OIDC_DEVICE_EXPIRED),
            Self::OidcRejected { code } | Self::Unexpected { code, .. } => code.as_deref(),
            Self::UpgradeRequired(refusal) => Some(refusal.error_code()),
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
    OidcAuthorize,
    OidcCallback,
    OidcDevice,
    OidcDevicePoll,
}

impl Endpoint {
    fn name(self) -> &'static str {
        match self {
            Self::Register => "register",
            Self::Login => "login",
            Self::VerifyTotp => "login/verify-totp",
            Self::Refresh => "refresh",
            Self::Logout => "logout",
            Self::OidcAuthorize => "oidc/authorize",
            Self::OidcCallback => "oidc/callback",
            Self::OidcDevice => "oidc/device",
            Self::OidcDevicePoll => "oidc/device/poll",
        }
    }

    /// What a `401` from this endpoint means.
    fn unauthorized_error(self, code: Option<String>) -> AuthError {
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
            // The callback's three `401`s — a spent state, a refused exchange, a refused token —
            // all mean "start again"; the code is kept for the log. The authorize declares no
            // `401`, so one from it is a server this client does not model.
            // The poll's remaining `401`s — a refused exchange, a refused token — are the
            // callback's and mean the same; its denial and expiry are matched by code first.
            Self::OidcCallback | Self::OidcDevicePoll => AuthError::OidcRejected { code },
            Self::OidcAuthorize | Self::OidcDevice => AuthError::Unexpected {
                status: 401,
                endpoint: self.name(),
                detail: String::new(),
                code,
            },
        }
    }

    /// The typed refusals only the OIDC endpoints make, matched on the catalog code.
    fn oidc_refusal(self, status: u16, code: Option<&str>) -> Option<AuthError> {
        match (self, status, code) {
            (
                Self::OidcAuthorize | Self::OidcDevice,
                404,
                Some(error_codes::AUTH_OIDC_NOT_CONFIGURED),
            ) => Some(AuthError::OidcNotConfigured),
            (Self::OidcAuthorize, 400, Some(error_codes::AUTH_OIDC_REDIRECT_INVALID)) => {
                Some(AuthError::OidcRedirectInvalid)
            }
            (
                Self::OidcCallback | Self::OidcDevicePoll,
                409,
                Some(error_codes::AUTH_OIDC_ADDRESS_TAKEN),
            ) => Some(AuthError::OidcAddressTaken),
            (Self::OidcDevice, 404, Some(error_codes::AUTH_OIDC_DEVICE_UNSUPPORTED)) => {
                Some(AuthError::OidcDeviceUnsupported)
            }
            (Self::OidcDevicePoll, 401, Some(error_codes::AUTH_OIDC_DEVICE_DENIED)) => {
                Some(AuthError::OidcDeviceDenied)
            }
            (Self::OidcDevicePoll, 401, Some(error_codes::AUTH_OIDC_DEVICE_EXPIRED)) => {
                Some(AuthError::OidcDeviceExpired)
            }
            _ => None,
        }
    }
}

// ─── Wire types ──────────────────────────────────────────────────────────────

#[derive(Serialize)]
struct LoginRequestBody<'a> {
    email: &'a str,
    password: &'a str,
    /// Advisory device-cohort hash. Rides the session-creation body
    /// only; **omitted entirely when absent** so "absent stays legal" is literal on
    /// the wire (no `null` field). Never read by any authorization decision.
    #[serde(skip_serializing_if = "Option::is_none")]
    cohort_hash: Option<&'a str>,
}

/// The registration body: an address and a password, and nothing else.
///
/// It carries no username, display name or advisory cohort hash: the server
/// takes none of them and its body is **strict**, so a field kept for old times' sake would be a
/// `422` rather than a value quietly ignored.
#[derive(Serialize)]
struct RegisterRequestBody<'a> {
    email: &'a str,
    password: &'a str,
}

/// The body that completes a sign-in with a code.
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
    /// The server root the generated client is built on, for the operations that go through
    /// it: the auth base with its `/v1/auth` suffix removed, or the base itself when it carries
    /// none (the in-crate mock serves the generated paths at its root).
    server_root: String,
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
            server_root: trimmed
                .strip_suffix("/v1/auth")
                .unwrap_or(trimmed)
                .to_owned(),
        })
    }
}

/// A begun sign-in through the identity provider.
///
/// The platform sends the person to `authorization_url`, receives the provider's redirect at
/// the `redirect_uri` it named, and hands this value with the redirect's `state` and `code` to
/// [`AuthClient::complete_oidc_login`]. Good once, and until `expires_by`.
///
/// It is the client's half of the ceremony, and the client must keep it — in memory, or in the
/// platform's secure storage where the browser leg can outlive the process — because the
/// callback is refused without its `binding` (issue #517). Never send any of it anywhere but
/// back to [`AuthClient::complete_oidc_login`].
///
/// No `Debug`: the state is the key to the pending ceremony, the URL carries it, and the binding
/// is the secret that proves this client began it.
pub struct OidcAuthorization {
    /// Where to send the person. Carries the whole authorization request in its query.
    pub authorization_url: String,
    /// The `state` the provider's redirect will echo.
    pub state: SecretString,
    /// The secret binding the ceremony to this client: 256 random bits, unpadded base64url.
    ///
    /// Only its S256 hash went to the server, at the authorize; the callback presents the
    /// secret itself, so a `state` and `code` that some other client began — an attacker's,
    /// carried to this one — cannot be completed here.
    pub binding: SecretString,
    /// The absolute Unix-seconds instant the ceremony stops being redeemable.
    pub expires_by: u64,
}

/// How much a `slow_down` raises a device sign-in's polling interval by (RFC 8628 §3.5). The
/// server raises its own copy by the same amount, which is why the new interval is never sent.
pub const DEVICE_SLOW_DOWN_STEP_SECS: u64 = 5;

/// A begun device sign-in through the identity provider (RFC 8628, issue #635).
///
/// The platform shows the person `user_code` and `verification_uri` — or renders
/// `verification_uri_complete` as a link or QR code — and polls
/// [`AuthClient::poll_oidc_device_login`] with this value, waiting `interval_secs` before the
/// first poll and between every poll, until it answers something other than
/// [`OidcDevicePoll::Pending`] or `expires_by` passes. On [`OidcDevicePoll::SlowDown`], call
/// [`slow_down`](Self::slow_down) before waiting again.
///
/// Keep it as an [`OidcAuthorization`] is kept: the `binding` is what the server checks on every
/// poll, and only its hash was sent. No `Debug`: the handle and the binding are a live credential.
pub struct OidcDeviceAuthorization {
    /// The handle the server polls the provider under. The server's own; not the device code.
    pub handle: SecretString,
    /// The secret binding the ceremony to this client: 256 random bits, unpadded base64url.
    pub binding: SecretString,
    /// The code the person types at the verification page. Show it exactly.
    pub user_code: String,
    /// Where the person goes, on any device with a browser, to type the code.
    pub verification_uri: String,
    /// The same address with the code already in it, when the provider offers one.
    pub verification_uri_complete: Option<String>,
    /// How many seconds to wait before each poll, raised by [`slow_down`](Self::slow_down).
    pub interval_secs: u64,
    /// The absolute Unix-seconds instant the ceremony stops being honoured.
    pub expires_by: u64,
}

impl OidcDeviceAuthorization {
    /// Raise the interval by [`DEVICE_SLOW_DOWN_STEP_SECS`], as RFC 8628 §3.5 requires on every
    /// `slow_down` — the server did the same, so the two stay in step.
    pub fn slow_down(&mut self) {
        self.interval_secs = self
            .interval_secs
            .saturating_add(DEVICE_SLOW_DOWN_STEP_SECS);
    }
}

/// What one device poll answered.
pub enum OidcDevicePoll {
    /// The person has not finished at the provider. Wait the interval and poll again.
    Pending,
    /// Polled too soon, or the provider asked to slow down. Call
    /// [`OidcDeviceAuthorization::slow_down`], wait, and poll again.
    SlowDown,
    /// The sign-in finished: exactly what [`AuthClient::login`] answers.
    Complete(LoginOutcome),
}

/// A fresh client binding for one OIDC ceremony: 256 random bits, unpadded base64url.
fn fresh_oidc_binding() -> SecretString {
    use base64::Engine as _;
    let bytes = capsule_core::crypto::rng::random_array::<32>();
    SecretString::from(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

/// The S256 hash of `binding` — unpadded base64url of its SHA-256 (RFC 7636 §4.2's derivation),
/// the `binding_challenge` the authorize carries.
fn oidc_binding_challenge(binding: &SecretString) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(binding.expose_secret().as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// What a password login answered with.
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
    /// The generated client over the same transport, for the operations that are not token
    /// orchestration (the OIDC legs).
    rest: Arc<rest::Client>,
    clock: Arc<dyn Clock>,
    refresh_skew_secs: i64,
    /// The advisory device-cohort hash to ride every session-creation request. `None` ⇒ nothing is
    /// sent — the server behaves identically
    /// (the cohort hash is advisory only). Computed by the platform app from its
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
        let base = AuthEndpoints::from_base(base_url)?;
        // Executed on `http` (and so carrying its handshake headers), latching the window like
        // the typed client (issue #674). The OIDC legs are session upkeep, so only the server's
        // own `426` refuses them — see `NegotiatedSend`.
        let backend: Arc<dyn rest::HttpBackend> = Arc::new(crate::client::Negotiate::observing(
            Arc::new(rest::ReqwestBackend::new(http.clone())),
        ));
        let rest = rest::Client::with_backend(backend, &base.server_root).map_err(|e| {
            AuthError::InvalidBaseUrl {
                url: base_url.to_string(),
                reason: e.to_string(),
            }
        })?;
        Ok(Self {
            http,
            base: Arc::new(base),
            rest: Arc::new(rest),
            clock,
            refresh_skew_secs,
            cohort_hash: None,
        })
    }

    /// Attach the advisory device-cohort hash that rides every subsequent
    /// session-creation request (login/register) from this client.
    ///
    /// The value is a lowercase-hex SHA-256 digest computed by the platform app from
    /// its primary identifier and the account's `user_id`
    /// ([`crate::cohort::compute_cohort_hash`]). It is a **grouping aid only** — the
    /// server never reads it for any authorization decision. Not setting it
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
            .send_screened()
            .await?;

        let outcome = self.read_login_outcome(Endpoint::Login, response).await?;
        tracing::info!("login answered");
        Ok(outcome)
    }

    /// Begin a sign-in through the server's identity provider.
    ///
    /// `redirect_uri` is where the provider will send the person back — this client's own
    /// callback, which the server admits if it is the deployment's configured one or a loopback
    /// IP literal on any port (the shape a CLI's or desktop app's listener has). What comes back
    /// is where to send the person, the `state` the redirect will echo, and the secret `binding`
    /// this client keeps: a fresh one per ceremony, of which only the hash is sent.
    ///
    /// # Errors
    ///
    /// [`AuthError::OidcNotConfigured`] when the server has no provider,
    /// [`AuthError::OidcRedirectInvalid`] when it refuses the redirect.
    #[instrument(skip_all)]
    pub async fn begin_oidc_login(
        &self,
        redirect_uri: &str,
    ) -> Result<OidcAuthorization, AuthError> {
        tracing::info!("beginning a sign-in through the identity provider");
        let binding = fresh_oidc_binding();
        let body = self
            .rest
            .begin_oidc_login(
                PROTOCOL_VERSION.to_owned(),
                None,
                &rest::types::OidcAuthorizeRequest {
                    redirect_uri: redirect_uri.to_owned(),
                    binding_challenge: oidc_binding_challenge(&binding),
                },
            )
            .await
            .map_err(|error| {
                map_rest_error(Endpoint::OidcAuthorize, error, |refused| match refused {
                    rest::BeginOidcLoginError::Status400(problem)
                    | rest::BeginOidcLoginError::Status404(problem)
                    | rest::BeginOidcLoginError::Status415(problem)
                    | rest::BeginOidcLoginError::Status422(problem)
                    | rest::BeginOidcLoginError::Status426(problem)
                    | rest::BeginOidcLoginError::Status429(problem)
                    | rest::BeginOidcLoginError::Status500(problem)
                    | rest::BeginOidcLoginError::Status503(problem)
                    | rest::BeginOidcLoginError::Status413(problem) => Some(*problem),
                })
            })?
            .into_inner();
        Ok(OidcAuthorization {
            authorization_url: body.authorization_url,
            state: SecretString::from(body.state),
            binding,
            expires_by: u64::try_from(body.expires_by).unwrap_or(0),
        })
    }

    /// Finish a sign-in through the identity provider with what its redirect carried.
    ///
    /// Answers a [`LoginOutcome`] exactly as [`login`](AuthClient::login) does — a session, or a
    /// second-factor challenge for an account that enrolled one — and the configured cohort hash
    /// rides this request, because this is the one that opens the session.
    ///
    /// `pending` is what [`begin_oidc_login`](AuthClient::begin_oidc_login) answered;
    /// `redirect_state` and `code` are what the provider's redirect carried. A redirect whose
    /// `state` is not the one this client began is refused here, before anything is sent: it is
    /// someone else's ceremony arriving at this client (RFC 6749 §10.12). The server makes the
    /// same refusal on the `binding`, which is what holds when a client skips this check.
    ///
    /// # Errors
    ///
    /// [`AuthError::OidcRejected`] for a redirect carrying another ceremony's `state`, a spent or
    /// expired `state`, a refused exchange or a refused ID token (start again);
    /// [`AuthError::OidcAddressTaken`] when the provider asserted an address that already has a
    /// local account.
    #[instrument(skip_all)]
    pub async fn complete_oidc_login(
        &self,
        pending: &OidcAuthorization,
        redirect_state: &str,
        code: &str,
    ) -> Result<LoginOutcome, AuthError> {
        tracing::info!(
            cohort_emitted = self.cohort().is_some(),
            "completing a sign-in through the identity provider"
        );
        if redirect_state != pending.state.expose_secret() {
            // The code the server gives a `state` it does not hold for this client: the remedy
            // is the same, and a client localizes one code rather than two.
            tracing::warn!(
                "an identity provider redirect carried a state this client did not begin; refused"
            );
            return Err(AuthError::OidcRejected {
                code: Some(error_codes::AUTH_OIDC_STATE_INVALID.to_owned()),
            });
        }
        let answer = self
            .rest
            .complete_oidc_login(
                PROTOCOL_VERSION.to_owned(),
                None,
                &rest::types::OidcCallbackRequest {
                    state: pending.state.expose_secret().to_owned(),
                    code: code.to_owned(),
                    binding_verifier: pending.binding.expose_secret().to_owned(),
                    cohort_hash: self.cohort().map(str::to_owned),
                    device_id: None,
                },
            )
            .await
            .map_err(|error| {
                map_rest_error(Endpoint::OidcCallback, error, |refused| match refused {
                    rest::CompleteOidcLoginError::Status400(problem)
                    | rest::CompleteOidcLoginError::Status401(problem)
                    | rest::CompleteOidcLoginError::Status409(problem)
                    | rest::CompleteOidcLoginError::Status415(problem)
                    | rest::CompleteOidcLoginError::Status422(problem)
                    | rest::CompleteOidcLoginError::Status426(problem)
                    | rest::CompleteOidcLoginError::Status500(problem)
                    | rest::CompleteOidcLoginError::Status413(problem) => Some(*problem),
                })
            })?
            .into_inner();
        // The status is the discriminator, as on the password login; the generated enum is
        // exactly that status made a type.
        let outcome = match answer {
            rest::CompleteOidcLoginResponse::Status202(challenge) => {
                tracing::info!("the identity provider sign-in needs a second factor");
                LoginOutcome::SecondFactorRequired {
                    mfa_token: SecretString::from(challenge.mfa_token),
                    expires_by: u64::try_from(challenge.expires_by).unwrap_or(0),
                }
            }
            rest::CompleteOidcLoginResponse::Status200(pair) => {
                let tokens = TokenSet::from_wire(
                    Endpoint::OidcCallback,
                    TokenResponseBody {
                        access_token: pair.access_token,
                        refresh_token: pair.refresh_token,
                        expires_by: u64::try_from(pair.expires_by).unwrap_or(0),
                    },
                )?;
                tracing::info!("the identity provider sign-in succeeded; session established");
                LoginOutcome::Session(self.session_with_tokens(tokens))
            }
        };
        Ok(outcome)
    }

    /// Begin a sign-in through the server's identity provider for a client with no browser of
    /// its own (RFC 8628, issue #635).
    ///
    /// What comes back is what to show the person and the handle to poll with, bound to this
    /// client by a fresh secret of which only the hash is sent.
    ///
    /// # Errors
    ///
    /// [`AuthError::OidcNotConfigured`] when the server has no provider,
    /// [`AuthError::OidcDeviceUnsupported`] when its provider does not offer the grant,
    /// [`AuthError::RateLimited`] when too many were begun.
    #[instrument(skip_all)]
    pub async fn begin_oidc_device_login(&self) -> Result<OidcDeviceAuthorization, AuthError> {
        tracing::info!("beginning a device sign-in through the identity provider");
        let binding = fresh_oidc_binding();
        let body = self
            .rest
            .begin_oidc_device_login(
                PROTOCOL_VERSION.to_owned(),
                None,
                &rest::types::OidcDeviceRequest {
                    binding_challenge: oidc_binding_challenge(&binding),
                },
            )
            .await
            .map_err(|error| {
                map_rest_error(Endpoint::OidcDevice, error, |refused| match refused {
                    rest::BeginOidcDeviceLoginError::Status400(problem)
                    | rest::BeginOidcDeviceLoginError::Status404(problem)
                    | rest::BeginOidcDeviceLoginError::Status415(problem)
                    | rest::BeginOidcDeviceLoginError::Status422(problem)
                    | rest::BeginOidcDeviceLoginError::Status426(problem)
                    | rest::BeginOidcDeviceLoginError::Status429(problem)
                    | rest::BeginOidcDeviceLoginError::Status500(problem)
                    | rest::BeginOidcDeviceLoginError::Status503(problem)
                    | rest::BeginOidcDeviceLoginError::Status413(problem) => Some(*problem),
                })
            })?
            .into_inner();
        tracing::info!(
            interval_secs = body.interval,
            "the device sign-in began; waiting for the person"
        );
        Ok(OidcDeviceAuthorization {
            handle: SecretString::from(body.handle),
            binding,
            user_code: body.user_code,
            verification_uri: body.verification_uri,
            verification_uri_complete: body.verification_uri_complete,
            interval_secs: u64::try_from(body.interval).unwrap_or(0),
            expires_by: u64::try_from(body.expires_by).unwrap_or(0),
        })
    }

    /// Poll a device sign-in once (RFC 8628, issue #635).
    ///
    /// [`OidcDevicePoll::Pending`] and [`OidcDevicePoll::SlowDown`] are RFC 8628's "keep
    /// polling"; [`OidcDevicePoll::Complete`] is exactly what [`login`](AuthClient::login)
    /// answers — a session, or a second-factor challenge — and the configured cohort hash rides
    /// this request, because it is the one that opens the session. Waiting is the caller's: this
    /// sends one request and returns.
    ///
    /// # Errors
    ///
    /// [`AuthError::OidcDeviceDenied`] when the person declined, [`AuthError::OidcDeviceExpired`]
    /// when the ceremony is over, [`AuthError::OidcRejected`] for a refused exchange or ID
    /// token, [`AuthError::OidcAddressTaken`] as on the callback.
    #[instrument(skip_all)]
    pub async fn poll_oidc_device_login(
        &self,
        pending: &OidcDeviceAuthorization,
    ) -> Result<OidcDevicePoll, AuthError> {
        let answer = self
            .rest
            .poll_oidc_device_login(
                PROTOCOL_VERSION.to_owned(),
                None,
                &rest::types::OidcDevicePollRequest {
                    handle: pending.handle.expose_secret().to_owned(),
                    binding_verifier: pending.binding.expose_secret().to_owned(),
                    cohort_hash: self.cohort().map(str::to_owned),
                    device_id: None,
                },
            )
            .await;
        let answer =
            match answer {
                Ok(answer) => answer.into_inner(),
                Err(rest::Error::Api(refused)) if keeps_polling(refused.inner()).is_some() => {
                    let poll = keeps_polling(refused.inner()).unwrap_or(OidcDevicePoll::Pending);
                    tracing::debug!(
                        slow_down = matches!(poll, OidcDevicePoll::SlowDown),
                        "the device sign-in is not finished"
                    );
                    return Ok(poll);
                }
                Err(error) => {
                    return Err(map_rest_error(Endpoint::OidcDevicePoll, error, |refused| {
                        match refused {
                            rest::PollOidcDeviceLoginError::Status400(problem)
                            | rest::PollOidcDeviceLoginError::Status401(problem)
                            | rest::PollOidcDeviceLoginError::Status409(problem)
                            | rest::PollOidcDeviceLoginError::Status415(problem)
                            | rest::PollOidcDeviceLoginError::Status422(problem)
                            | rest::PollOidcDeviceLoginError::Status426(problem)
                            | rest::PollOidcDeviceLoginError::Status429(problem)
                            | rest::PollOidcDeviceLoginError::Status500(problem)
                            | rest::PollOidcDeviceLoginError::Status413(problem) => Some(*problem),
                        }
                    }));
                }
            };
        let outcome = match answer {
            rest::PollOidcDeviceLoginResponse::Status202(challenge) => {
                tracing::info!("the device sign-in needs a second factor");
                LoginOutcome::SecondFactorRequired {
                    mfa_token: SecretString::from(challenge.mfa_token),
                    expires_by: u64::try_from(challenge.expires_by).unwrap_or(0),
                }
            }
            rest::PollOidcDeviceLoginResponse::Status200(pair) => {
                let tokens = TokenSet::from_wire(
                    Endpoint::OidcDevicePoll,
                    TokenResponseBody {
                        access_token: pair.access_token,
                        refresh_token: pair.refresh_token,
                        expires_by: u64::try_from(pair.expires_by).unwrap_or(0),
                    },
                )?;
                tracing::info!("the device sign-in succeeded; session established");
                LoginOutcome::Session(self.session_with_tokens(tokens))
            }
        };
        Ok(OidcDevicePoll::Complete(outcome))
    }

    /// A `200` is a session and a `202` is a challenge, on every route that opens a session.
    ///
    /// Read from the **status**, which is where the server puts the distinction; a body flag
    /// would be a second place for the two to disagree. Treating the `202` as a token response
    /// would send it through `read_tokens` and surface as `MalformedResponse`, which would tell a
    /// user with a second factor that their server was broken.
    async fn read_login_outcome(
        &self,
        endpoint: Endpoint,
        response: reqwest::Response,
    ) -> Result<LoginOutcome, AuthError> {
        if response.status() == reqwest::StatusCode::ACCEPTED {
            let challenge: SecondFactorChallengeBody =
                response
                    .json()
                    .await
                    .map_err(|e| AuthError::MalformedResponse {
                        endpoint: endpoint.name(),
                        reason: e.to_string(),
                    })?;
            tracing::info!("the sign-in needs a second factor");
            return Ok(LoginOutcome::SecondFactorRequired {
                mfa_token: SecretString::from(challenge.mfa_token),
                expires_by: challenge.expires_by,
            });
        }
        let tokens = read_tokens(endpoint, response).await?;
        tracing::info!("the sign-in succeeded; session established");
        Ok(LoginOutcome::Session(self.session_with_tokens(tokens)))
    }

    /// Complete a sign-in with the code an authenticator app is showing.
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
            .send_screened()
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
            .send_screened()
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
            .send_negotiated()
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
            .send_negotiated()
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

    /// Refresh because the server **rejected** `stale`, and return the token that replaced
    /// it (single-flight).
    ///
    /// The reactive counterpart to [`bearer`](Session::bearer)'s pre-flight refresh, and the
    /// primitive [`crate::client::AuthenticatedClient`]'s `401` layer drives. Passing the
    /// exact token the server refused is the whole point: `ensure_refreshed`'s `Rejected`
    /// arm compares it against the store, so a caller whose stale token has *already* been
    /// rotated by a concurrent refresh gets the fresh token back with no second network
    /// call. [`refresh`](Session::refresh) cannot serve this — it re-reads the *current*
    /// token and would therefore refresh again on top of that rotation, spending a
    /// single-use refresh token the server has already closed.
    #[instrument(skip_all)]
    pub(crate) async fn refresh_rejected(&self, stale: &str) -> Result<SecretString, AuthError> {
        self.ensure_refreshed(RefreshTrigger::Rejected(stale.to_owned()))
            .await
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
        let sent = self
            .http
            .post(&self.base.logout)
            .bearer_auth(token.expose_secret())
            .send_screened()
            .await;
        // Any answer clears the local store, a `426` included: an out-of-window client can
        // still sign out on this device, and the error says the server-side revoke did not land.
        if matches!(sent, Ok(_) | Err(AuthError::UpgradeRequired(_))) {
            *self.inner.tokens.write().await = None;
        }
        let response = sent?;
        let status = response.status();
        if status.is_success() || status == reqwest::StatusCode::UNAUTHORIZED {
            tracing::info!("session logged out; local tokens cleared");
            Ok(())
        } else {
            Err(error_from_response(Endpoint::Logout, response).await)
        }
    }

    /// A currently-valid **bearer access token** for injecting into a request the
    /// SDK does not build with [`Session::execute`] — notably the generated REST client's
    /// token-provider seam ([`crate::client::AuthenticatedClient`]), where the token is
    /// attached by the client rather than by this module. Pre-flight-refreshes exactly like
    /// [`Session::execute`]; the reactive half — a `401` the pre-flight check could not
    /// foresee — is `refresh_rejected`'s.
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
            .send_screened()
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
    let status = response.status().as_u16();
    let retry_after = retry_after_of(response.headers());
    let api_error = response.json::<ApiErrorBody>().await.ok();
    let code = api_error.as_ref().and_then(|body| body.code.clone());
    let detail = api_error.map_or_else(String::new, |body| body.error);
    error_for(endpoint, status, code, detail, retry_after)
}

/// The `Retry-After` seconds a response carries, if it carries one.
fn retry_after_of(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    headers
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|raw| raw.trim().parse::<u64>().ok())
}

/// One status → variant mapping for both the hand-rolled and the generated paths.
fn error_for(
    endpoint: Endpoint,
    status: u16,
    code: Option<String>,
    detail: String,
    retry_after: Option<u64>,
) -> AuthError {
    if let Some(refusal) = endpoint.oidc_refusal(status, code.as_deref()) {
        return refusal;
    }
    match status {
        401 => endpoint.unauthorized_error(code),
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

/// Whether a device poll's refusal is RFC 8628's "keep polling", matched on the catalog code:
/// `400 error.auth.oidc_device_pending` or `429 error.auth.oidc_device_slow_down`.
fn keeps_polling(refused: &rest::PollOidcDeviceLoginError) -> Option<OidcDevicePoll> {
    match refused {
        rest::PollOidcDeviceLoginError::Status400(problem)
            if problem.code == error_codes::AUTH_OIDC_DEVICE_PENDING =>
        {
            Some(OidcDevicePoll::Pending)
        }
        rest::PollOidcDeviceLoginError::Status429(problem)
            if problem.code == error_codes::AUTH_OIDC_DEVICE_SLOW_DOWN =>
        {
            Some(OidcDevicePoll::SlowDown)
        }
        _ => None,
    }
}

/// Map a generated-client failure to a typed [`AuthError`].
///
/// `problem` extracts the coded problem a documented refusal carries, so the status → variant
/// mapping is the one the hand-rolled path uses; the generated client's own classes — transport,
/// timeout, protocol, redirect, construction — become [`AuthError::Network`], an undocumented
/// status [`AuthError::Unexpected`], and a body that did not decode [`AuthError::MalformedResponse`].
fn map_rest_error<E: std::error::Error + 'static>(
    endpoint: Endpoint,
    error: rest::Error<E>,
    problem: impl FnOnce(E) -> Option<rest::types::CodedProblem>,
) -> AuthError {
    if let Some(refusal) = crate::client::upgrade_required_of(&error) {
        return refusal.into();
    }
    match error {
        rest::Error::Api(refused) => {
            let status = refused.status().as_u16();
            let retry_after = retry_after_of(refused.headers());
            match problem(refused.into_inner()) {
                Some(problem) => error_for(
                    endpoint,
                    status,
                    Some(problem.code),
                    problem.detail.unwrap_or_default(),
                    retry_after,
                ),
                None => error_for(endpoint, status, None, String::new(), retry_after),
            }
        }
        rest::Error::UnexpectedStatus { status, body, .. } => AuthError::Unexpected {
            status: status.as_u16(),
            endpoint: endpoint.name(),
            detail: String::from_utf8_lossy(&body).into_owned(),
            code: None,
        },
        rest::Error::Decode { path, .. } => AuthError::MalformedResponse {
            endpoint: endpoint.name(),
            reason: path,
        },
        other => AuthError::Network {
            endpoint: endpoint.name(),
            detail: other.to_string(),
        },
    }
}

#[cfg(test)]
mod tests;
