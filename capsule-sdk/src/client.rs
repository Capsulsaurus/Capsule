//! The session-composed typed REST client.
//!
//! [`AuthenticatedClient`] revives the shape the parked wrapper in the crate root promised —
//! a thin skin over the `spargen`-generated [`rest::Client`] that owns
//! the base URL, swaps base URL/credentials without callers rebuilding anything, and
//! `Deref`s to the typed client so every generated operation is called directly. What the
//! parked sketch could not express, this composition does: instead of a static bearer string,
//! the client is wired to the SDK's [`Session`] through spargen's async
//! token-provider seam, so **every** typed call transparently gets a valid access token, with
//! the session's proactive pre-flight refresh and single-flight coalescing — callers never
//! touch a raw token. The refresh/expiry/single-flight logic is reused wholesale from
//! [`crate::auth`]; nothing is duplicated here.
//!
//! # Both halves of the refresh contract
//!
//! The token provider is the **proactive** half: it refreshes when the stored token is within
//! its skew of expiry, before the request leaves. That cannot cover a token the server stops
//! honouring early — a revocation mid-flight, or a clock the two ends disagree about — so
//! `RefreshOn401` is the **reactive** half: an [`rest::HttpBackend`] wrapping
//! [`rest::ReqwestBackend`] that, on a `401` whose problem code says the access token was
//! refused (`error.request.unauthenticated`), refreshes once through the session and replays
//! the request exactly once. A `401` carrying any other code is a refused *credential* — a
//! wrong password or second-factor code — and is surfaced untouched. The two are
//! complementary and neither duplicates the other; the refresh itself is still `auth`'s
//! single-flight gate.
//!
//! It sits at the transport seam rather than in each caller, so **every** generated operation
//! is covered by one layer that survives regeneration — no generated code is touched, and
//! there is no per-call retry loop to keep in step.
//!
//! Scope: this covers the plain request/response REST surfaces the OpenAPI schema declares
//! (auth/session, quota, storage-verify, receipts, devices, escrow, …). The stateful upload
//! protocol ([`crate::upload`]) stays hand-written. The sync feed ([`crate::sync`]) *is* a
//! generated operation (`GET /v1/sync`), but [`crate::sync::SyncConsumer`] drives it under its
//! own cursor/anti-rewind state machine and builds its own client, because it also serves a
//! static-token mode that has no session to refresh.

use std::ops::Deref;
use std::sync::Arc;

use reqwest::header::{AUTHORIZATION, HeaderValue};
use secrecy::ExposeSecret;

use crate::auth::Session;
use crate::net::negotiation;
use crate::rest::{self, Client, Credential};

/// The security-scheme key the server declares for its bearer JWT (see
/// `capsule_api::create_openapi_spec`); the generated client attaches the registered
/// credential to every operation whose `security` names it.
const BEARER_SCHEME: &str = "bearer";

/// Failure constructing an [`AuthenticatedClient`] — today only a malformed base URL. Wire
/// failures surface later, per operation, through the generated client's own typed error.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The configured base URL could not be parsed.
    #[error("invalid base URL {url:?}: {reason}")]
    InvalidBaseUrl {
        /// The offending URL.
        url: String,
        /// Why the generated client rejected it.
        reason: String,
    },
}

/// A typed REST client that authenticates itself from an SDK [`Session`].
///
/// Cheap to build; holds one [`rest::Client`] whose bearer credential is
/// an async provider backed by the session. Because the provider is consulted per request,
/// token rotation (refresh) is picked up with no rebuild. Deref-transparent: call any
/// generated operation directly, e.g. `client.get_quota(PROTOCOL_VERSION, None).await` — every
/// gated operation takes the protocol date as its first argument, because the document
/// declares `X-Capsule-Protocol` required there (issue #404); the transport sends the same value
/// as a default header regardless.
pub struct AuthenticatedClient {
    base_url: String,
    session: Session,
    client: Client,
}

impl AuthenticatedClient {
    /// Build a client for `base_url` (the API root the generated operation paths hang off,
    /// e.g. `https://api.example.com`), authenticated by `session`.
    pub fn new(base_url: &str, session: Session) -> Result<Self, ClientError> {
        let client = build_client(base_url, session.clone())?;
        Ok(Self {
            base_url: base_url.to_string(),
            session,
            client,
        })
    }

    /// The API root this client is currently pointed at.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Whether this build is outside the window this client's server last advertised — the
    /// read-side signal: reads keep being served, writes are refused with a `426`, and this is
    /// what a UI asks to tell the user to update. `None` before any response, and inside it.
    #[must_use]
    pub fn upgrade_required(&self) -> Option<negotiation::UpgradeRequired> {
        let url = reqwest::Url::parse(&self.base_url).ok()?;
        negotiation::upgrade_required_for(&url)
    }

    /// The session backing this client's bearer credential.
    #[must_use]
    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Repoint the client at a new API root, keeping the same session. Rebuilds the inner
    /// generated client (base URL is fixed at its construction).
    pub fn with_base_url(&mut self, base_url: &str) -> Result<&mut Self, ClientError> {
        self.client = build_client(base_url, self.session.clone())?;
        self.base_url = base_url.to_string();
        Ok(self)
    }

    /// Swap the backing session (e.g. after re-authentication), rebuilding the credential.
    pub fn with_session(&mut self, session: Session) -> Result<&mut Self, ClientError> {
        self.client = build_client(&self.base_url, session.clone())?;
        self.session = session;
        Ok(self)
    }
}

impl Deref for AuthenticatedClient {
    type Target = Client;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}

/// The bearer prefix the `Authorization` header carries, and the only credential shape
/// [`RefreshOn401`] recognises as a token it can refresh.
const BEARER_PREFIX: &str = "Bearer ";

/// The reactive half of the refresh contract: an [`rest::HttpBackend`] that,
/// on a `401` coded [`EXPIRED_TOKEN_CODE`], refreshes the session once and replays the request
/// exactly once. Any other `401` is returned as the server sent it.
///
/// # Why the transport seam and not [`rest::Middleware`]
///
/// A middleware receives a `Next`, and `Next::run` takes `self` by value while `Next` is
/// neither `Clone` nor constructible outside the generated runtime. A middleware therefore
/// *cannot* send a second time, which is the one thing this layer must do. The backend seam
/// has no such constraint, and spargen's own `RetryBackend` is the precedent — including the
/// `Request::try_clone()`-returns-`None` rule for one-shot bodies.
///
/// # Exactly once, by construction
///
/// The replay is straight-line code, not a loop with a counter: one send, one refresh, one
/// replay, and whatever the replay answers is returned as-is. A second `401` is surfaced.
struct RefreshOn401 {
    inner: Arc<dyn rest::HttpBackend>,
    session: Session,
}

// `Session` is not `Debug` (it holds token material), and `HttpBackend` requires `Debug` so the
// generated `ClientCore` stays printable. The manual impl names the layer and the backend under
// it, and shows nothing of the session.
impl std::fmt::Debug for RefreshOn401 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RefreshOn401")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl rest::HttpBackend for RefreshOn401 {
    fn execute(&self, request: reqwest::Request) -> rest::ExecuteFuture<'_> {
        // Own clones of the `Arc`/`Session` so the returned future is self-contained, matching
        // the seam's expectations (and `RetryBackend`'s shape).
        let inner = self.inner.clone();
        let session = self.session.clone();
        Box::pin(async move {
            // A one-shot streaming body cannot be resent intact. Execute the original once and
            // return: half a body on the wire twice is worse than a `401` the caller can see.
            let Some(mut replay) = request.try_clone() else {
                tracing::debug!("request body is not replayable; a 401 will not be retried");
                return inner.execute(request).await;
            };

            let response = inner.execute(request).await?;
            if response.status() != reqwest::StatusCode::UNAUTHORIZED {
                return Ok(response);
            }

            // An operation carrying no bearer has nothing to refresh — that `401` is the
            // server's answer about the request, not about a stale token.
            let Some(stale) = bearer_of(&replay) else {
                tracing::debug!("a 401 arrived on a request carrying no bearer; not retrying");
                return Ok(response);
            };

            // Only the bearer scheme's own refusal is a stale token. A `401` an operation
            // answers about its *payload* — a wrong password on `reauthenticate`, a wrong code
            // on `totp_disable` — is a refused credential: replaying it charges the lockout
            // twice and spends the refresh token for nothing (issue #511).
            let (code, response) = problem_code_of(response).await?;
            if code.as_deref() != Some(EXPIRED_TOKEN_CODE) {
                tracing::debug!(
                    code = code.as_deref().unwrap_or("<none>"),
                    "a 401 that is not an expired token; not retrying"
                );
                return Ok(response);
            }

            let fresh = match session.refresh_rejected(&stale).await {
                Ok(fresh) => fresh,
                // Deliberately the *original* `401`, not a synthetic transport error: the
                // generated operation then maps it to its typed `Status401` and the caller
                // reads the server's own problem — the `error.request.unauthenticated` the
                // operation answered, not whatever the failed refresh answered — instead of a
                // transport error that carries no code at all.
                Err(error) => {
                    tracing::warn!(%error, "a 401 could not be recovered; surfacing it");
                    return Ok(response);
                }
            };
            let Ok(header) =
                HeaderValue::from_str(&format!("{BEARER_PREFIX}{}", fresh.expose_secret()))
            else {
                tracing::warn!(
                    "the refreshed token is not a valid header value; surfacing the 401"
                );
                return Ok(response);
            };
            replay.headers_mut().insert(AUTHORIZATION, header);
            tracing::info!("the typed client's 401 was refreshed; replaying once");
            inner.execute(replay).await
        })
    }
}

/// The one `401` problem code [`RefreshOn401`] refreshes and replays: the code the server's
/// coded-problem interceptor gives the bearer scheme's own refusal — a token that does not
/// verify, has expired, or names a session the ledger no longer holds
/// (`capsule_server::problem::code_for`). Every route-level `401` carries its own `error.*`
/// code, and none of those is a stale token.
const EXPIRED_TOKEN_CODE: &str = capsule_i18n::error_codes::REQUEST_UNAUTHENTICATED;

/// The `code` member of an RFC 9457 problem body; every other member is the generated
/// operation's to parse.
#[derive(serde::Deserialize)]
struct ProblemCode {
    code: Option<String>,
}

/// Read `response`'s problem `code`, then rebuild the response from the bytes read so the
/// generated operation still maps it to its typed error. A body that is not a problem yields
/// `None`, which [`RefreshOn401`] treats as "not an expired token": it fails closed, never
/// replaying a `401` it cannot classify.
async fn problem_code_of(
    response: reqwest::Response,
) -> Result<(Option<String>, reqwest::Response), rest::TransportError> {
    let status = response.status();
    let version = response.version();
    let headers = response.headers().clone();
    let body = response.bytes().await.map_err(rest::TransportError::new)?;
    let code = serde_json::from_slice::<ProblemCode>(&body)
        .ok()
        .and_then(|problem| problem.code);

    let mut rebuilt = http::Response::new(body);
    *rebuilt.status_mut() = status;
    *rebuilt.version_mut() = version;
    *rebuilt.headers_mut() = headers;
    Ok((code, reqwest::Response::from(rebuilt)))
}

/// The bearer token a prepared request carries, if any. `None` for an unauthenticated
/// operation, and for any credential shape this layer cannot refresh.
fn bearer_of(request: &reqwest::Request) -> Option<String> {
    request
        .headers()
        .get(AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix(BEARER_PREFIX)
        .map(str::to_owned)
}

/// Wire a generated client to `base_url` with a bearer credential that pulls a fresh access
/// token from `session` on demand (the proactive half), executing through [`RefreshOn401`]
/// (the reactive half).
fn build_client(base_url: &str, session: Session) -> Result<Client, ClientError> {
    let provider_session = session.clone();
    let provider: rest::TokenProvider = Arc::new(move || {
        let session = provider_session.clone();
        // The session yields a currently-valid bearer, refreshing pre-flight if the stored
        // token is within its refresh skew of expiry; the failure is mapped into spargen's
        // provider-error type so a dead session is a request-construction error, not a 401.
        Box::pin(async move {
            session
                .bearer()
                .await
                .map_err(|e| rest::AuthError::new(e.to_string()))
        })
    });

    // `with_backend` builds requests on a default `reqwest::Client` and *executes* them
    // through the backend, so the executing client — and with it the TLS stack, the redirect
    // policy and the timeouts — is still `reqwest_client()`, one layer down.
    let backend: Arc<dyn rest::HttpBackend> = Arc::new(Negotiate::over(Arc::new(RefreshOn401 {
        inner: Arc::new(rest::ReqwestBackend::new(reqwest_client())),
        session,
    })));
    let client = Client::with_backend(backend, base_url)
        .map_err(|e| ClientError::InvalidBaseUrl {
            url: base_url.to_string(),
            reason: e.to_string(),
        })?
        .with_credential(BEARER_SCHEME, Credential::Provider(provider));
    Ok(client)
}

/// The generated client's transport: the SDK's one HTTP client
/// ([`crate::net::http_client`]) — rustls only, carrying the protocol handshake on every request
/// it sends, the generated operations included.
///
/// **One per process, shared.** A `reqwest::Client` owns a connection pool, and cloning it
/// shares that pool; building a new one throws the pool away. The FFI's escrow verbs construct
/// a fresh [`AuthenticatedClient`] per call (the API root is a per-call argument), so a
/// per-client transport would mean a fresh TLS handshake for every escrow read on a device
/// that does several during one cadence prompt. Nothing here is configured per instance —
/// `http_client` takes no arguments and its protocol headers are the same on every request —
/// so there is nothing to vary: the same client serves them all.
///
/// **What that does not fix.** `Client::with_backend` still builds its *own* default
/// `reqwest::Client` internally, one per [`AuthenticatedClient`]. That one only assembles
/// requests — every byte is executed through the backend below, and therefore through this
/// shared client — so it opens no connection and costs nothing on the wire; what it costs is
/// one throwaway allocation per construction. Removing even that needs a
/// `with_client_and_backend` constructor spargen does not expose, which is generator work of
/// exactly the same kind as the `application/cbor` gap, and lands where that lands.
fn reqwest_client() -> reqwest::Client {
    static SHARED: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    SHARED
        .get_or_init(|| {
            crate::net::http_client()
                .expect("a default rustls reqwest client is always constructible")
        })
        .clone()
}

/// The protocol window on the generated client's transport (issue #674): every response's
/// advertised window is latched ([`negotiation::observe`]), and a write the latched window
/// already refuses is answered here, before it leaves, with a local `426`.
///
/// # Why a synthesized `426` and not an error
///
/// The backend seam's error type wraps a `reqwest::Error` and nothing else, so a typed refusal
/// cannot cross it. A `426` can: the generated operation maps it exactly as it maps the
/// server's own, and [`upgrade_required_of`] turns either into the one
/// [`UpgradeRequired`](negotiation::UpgradeRequired). The synthesized response carries the
/// latched window headers and a coded problem, so it is indistinguishable at the caller from
/// the refusal the server would have sent — minus the request the server never sees.
#[derive(Debug)]
pub(crate) struct Negotiate {
    inner: Arc<dyn rest::HttpBackend>,
    /// Whether a write the latched window refuses is answered locally. Off for the auth
    /// client's session-upkeep operations, which only the server's own `426` refuses
    /// (see `auth::NegotiatedSend`).
    preflight: bool,
}

impl Negotiate {
    /// Put the window screen over `inner`: latch every window, refuse a write it refuses.
    pub(crate) fn over(inner: Arc<dyn rest::HttpBackend>) -> Self {
        Self {
            inner,
            preflight: true,
        }
    }

    /// Latch every window `inner`'s responses advertise, and refuse nothing.
    pub(crate) fn observing(inner: Arc<dyn rest::HttpBackend>) -> Self {
        Self {
            inner,
            preflight: false,
        }
    }
}

impl rest::HttpBackend for Negotiate {
    fn execute(&self, request: reqwest::Request) -> rest::ExecuteFuture<'_> {
        let inner = self.inner.clone();
        let preflight = self.preflight;
        Box::pin(async move {
            let protocol = negotiation::protocol_of(request.headers());
            if preflight
                && let Err(refusal) =
                    negotiation::preflight(request.method(), request.url(), &protocol)
            {
                return Ok(local_upgrade_required(&refusal));
            }
            let response = inner.execute(request).await?;
            negotiation::observe(response.url(), response.headers());
            if response.status() == reqwest::StatusCode::PAYLOAD_TOO_LARGE {
                return coded_too_large(response).await;
            }
            Ok(response)
        })
    }
}

/// A `413` as the generated operations can read it, whoever answered it.
///
/// The contract declares every `413` a coded problem, and this server sends one. A server from
/// before that (its body-size backstop answered with no body), or a reverse proxy refusing the
/// body before it reached the server (nginx's `client_max_body_size` answers `413 text/html`),
/// does not. The generated client then fails to decode the documented body and reports a
/// wire failure instead of the typed refusal, which tells the caller to retry the same bytes.
/// Native clients stay lenient towards older and intermediary peers, so a `413` whose body is
/// not a coded problem is answered here with one coded `error.request.too_large`, keeping the
/// status and headers. A `413` that already carries a code passes through unchanged.
async fn coded_too_large(
    response: reqwest::Response,
) -> Result<reqwest::Response, rest::TransportError> {
    let status = response.status();
    let version = response.version();
    let mut headers = response.headers().clone();
    let body = response.bytes().await.map_err(rest::TransportError::new)?;
    let coded = serde_json::from_slice::<ProblemCode>(&body)
        .ok()
        .and_then(|problem| problem.code)
        .is_some();
    let body = if coded {
        body
    } else {
        tracing::debug!(
            bytes = body.len(),
            "a 413 without a coded problem body was coded error.request.too_large"
        );
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        headers.remove(reqwest::header::CONTENT_LENGTH);
        serde_json::json!({
            "type": "about:blank",
            "title": "Payload Too Large",
            "status": 413,
            "detail": "the request body is larger than the server or a proxy in front of it accepts",
            "code": capsule_i18n::error_codes::REQUEST_TOO_LARGE,
        })
        .to_string()
        .into()
    };
    let mut rebuilt = http::Response::new(body);
    *rebuilt.status_mut() = status;
    *rebuilt.version_mut() = version;
    *rebuilt.headers_mut() = headers;
    Ok(reqwest::Response::from(rebuilt))
}

/// A generated client for `base_url` that executes on `http` under the window screen — the
/// one way this crate builds a generated client outside [`AuthenticatedClient`], so a write
/// through any of them is pre-flighted and a read through any of them latches the window.
///
/// The error is the generated client's description of a `base_url` it cannot parse.
pub(crate) fn negotiated_client(http: reqwest::Client, base_url: &str) -> Result<Client, String> {
    Client::with_backend(
        Arc::new(Negotiate::over(Arc::new(rest::ReqwestBackend::new(http)))),
        base_url,
    )
    .map_err(|error| error.to_string())
}

/// The `426` [`Negotiate`] answers a refused write with: the window it was refused under, as
/// headers, and an RFC 9457 problem coded `error.protocol.version_unsupported`.
fn local_upgrade_required(refusal: &negotiation::UpgradeRequired) -> reqwest::Response {
    let problem = serde_json::json!({
        "type": "about:blank",
        "title": "Upgrade Required",
        "status": 426,
        "detail": refusal.message,
        "code": refusal.error_code(),
    });
    let mut response = http::Response::new(problem.to_string());
    *response.status_mut() = reqwest::StatusCode::UPGRADE_REQUIRED;
    let headers = response.headers_mut();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_static("application/problem+json"),
    );
    for (name, value) in [
        (
            negotiation::PROTOCOL_MIN_HEADER,
            &refusal.window.protocol_min,
        ),
        (
            negotiation::PROTOCOL_MAX_HEADER,
            &refusal.window.protocol_max,
        ),
        (
            negotiation::MIN_CLIENT_BUILD_HEADER,
            &refusal.window.min_client_build,
        ),
    ] {
        if let Some(value) = value.as_deref().and_then(|v| HeaderValue::from_str(v).ok()) {
            headers.insert(name, value);
        }
    }
    reqwest::Response::from(response)
}

/// The one typed [`UpgradeRequired`](negotiation::UpgradeRequired) a generated-client failure
/// carries, if it is one: a `426` — the server's, or the typed transport's local refusal of a
/// write the latched window refuses — documented or not. `None` for every other failure.
pub fn upgrade_required_of<E>(error: &rest::Error<E>) -> Option<negotiation::UpgradeRequired>
where
    E: std::error::Error + 'static,
{
    let (headers, message) = match error {
        rest::Error::Api(response)
            if response.status() == reqwest::StatusCode::UPGRADE_REQUIRED =>
        {
            (response.headers(), error.to_string())
        }
        rest::Error::UnexpectedStatus {
            status,
            headers,
            body,
        } if *status == reqwest::StatusCode::UPGRADE_REQUIRED => (
            headers,
            negotiation::problem_detail(&String::from_utf8_lossy(body)),
        ),
        _ => return None,
    };
    Some(negotiation::UpgradeRequired::refused(
        headers,
        negotiation::PROTOCOL_VERSION,
        message,
    ))
}

/// Whether a generated-client failure is the session failing to produce a bearer.
///
/// `RequestConstruction` is **not** a pre-flight-only class. reqwest builds every failure of
/// the request it executes with `error::request(..)`, so connection-refused, DNS and TLS
/// failures land there too. The *source* discriminates them: the token provider
/// [`build_client`] registers boxes its failure as the generated runtime's own
/// [`rest::AuthError`], and nothing else on this path is. A dead session must reach a caller
/// as an auth failure rather than as a transport blip, because the two have opposite remedies.
pub(crate) fn is_token_failure<E>(error: &rest::Error<E>) -> bool
where
    E: std::error::Error + 'static,
{
    matches!(error, rest::Error::RequestConstruction(inner)
        if std::error::Error::source(inner)
            .and_then(|source| source.downcast_ref::<rest::AuthError>())
            .is_some())
}

/// Render a generated-client failure together with its source chain. The taxonomy's own
/// `Display` names the class and, for the wire classes, little else (`"transport failed"`,
/// `"request construction failed"`); the source chain is where the reqwest/hyper reason a log
/// reader needs actually lives.
pub(crate) fn describe_error<E>(error: &rest::Error<E>) -> String
where
    E: std::error::Error + 'static,
{
    let mut rendered = error.to_string();
    let mut source = std::error::Error::source(error);
    while let Some(cause) = source {
        rendered.push_str(": ");
        rendered.push_str(&cause.to_string());
        source = cause.source();
    }
    rendered
}

#[cfg(test)]
mod tests;
