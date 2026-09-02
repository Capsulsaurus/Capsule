//! The session-composed typed REST client (slice `S-D8`).
//!
//! [`AuthenticatedClient`] revives the shape the parked wrapper in the crate root promised —
//! a thin skin over the `spargen`-generated [`rest::Client`](crate::rest::Client) that owns
//! the base URL, swaps base URL/credentials without callers rebuilding anything, and
//! `Deref`s to the typed client so every generated operation is called directly. What the
//! parked sketch could not express, this composition does: instead of a static bearer string,
//! the client is wired to S-D7's [`Session`](crate::auth::Session) through spargen's async
//! token-provider seam, so **every** typed call transparently gets a valid access token, with
//! the session's proactive pre-flight refresh and single-flight coalescing — callers never
//! touch a raw token. The refresh/expiry/single-flight logic is reused wholesale from
//! [`crate::auth`]; nothing is duplicated here.
//!
//! # Both halves of the refresh contract (slice `S-D17`)
//!
//! The token provider is the **proactive** half: it refreshes when the stored token is within
//! its skew of expiry, before the request leaves. That cannot cover a token the server stops
//! honouring early — a revocation mid-flight, or a clock the two ends disagree about — so
//! [`RefreshOn401`] is the **reactive** half: an [`rest::HttpBackend`] wrapping
//! [`rest::ReqwestBackend`] that, on a `401`, refreshes once through the session and replays
//! the request exactly once. The two are complementary and neither duplicates the other; the
//! refresh itself is still `auth`'s single-flight gate.
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

/// A typed REST client that authenticates itself from an S-D7 [`Session`].
///
/// Cheap to build; holds one [`rest::Client`](crate::rest::Client) whose bearer credential is
/// an async provider backed by the session. Because the provider is consulted per request,
/// token rotation (refresh) is picked up with no rebuild. Deref-transparent: call any
/// generated operation directly, e.g. `client.get_quota().await`.
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

/// The reactive half of the refresh contract (slice `S-D17`): an [`rest::HttpBackend`] that,
/// on a `401`, refreshes the session once and replays the request exactly once.
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

            let fresh = match session.refresh_rejected(&stale).await {
                Ok(fresh) => fresh,
                // Deliberately the *original* `401`, not a synthetic transport error: the
                // generated operation then maps it to its typed `Status401` and the caller
                // reads the server's own `error.*` code — which matters because an unreadable
                // revocation ledger is also rendered as `401`, and only that code separates an
                // outage from an expiry.
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
    let backend: Arc<dyn rest::HttpBackend> = Arc::new(RefreshOn401 {
        inner: Arc::new(rest::ReqwestBackend::new(reqwest_client())),
        session,
    });
    let client = Client::with_backend(backend, base_url)
        .map_err(|e| ClientError::InvalidBaseUrl {
            url: base_url.to_string(),
            reason: e.to_string(),
        })?
        .with_credential(BEARER_SCHEME, Credential::Provider(provider));
    Ok(client)
}

/// The generated client's transport: rustls only (the SDK's `reqwest` has no default features
/// and only `rustls-tls`), matching the rest of the SDK's network stack.
///
/// **One per process, shared.** A `reqwest::Client` owns a connection pool, and cloning it
/// shares that pool; building a new one throws the pool away. The FFI's escrow verbs construct
/// a fresh [`AuthenticatedClient`] per call (the API root is a per-call argument), so a
/// per-client transport would mean a fresh TLS handshake for every escrow read on a device
/// that does several during one cadence prompt. Nothing here is configured per instance, so
/// there is nothing to vary: the same client serves them all.
///
/// **What that does not fix.** `Client::with_backend` still builds its *own* default
/// `reqwest::Client` internally, one per `AuthenticatedClient`. That one only assembles
/// requests — every byte is executed through the backend below, and therefore through this
/// shared client — so it opens no connection and costs nothing on the wire; what it costs is
/// one throwaway allocation per construction. Removing even that needs a
/// `with_client_and_backend` constructor spargen does not expose, which is generator work of
/// exactly the same kind as the `application/cbor` gap, and lands where that lands.
fn reqwest_client() -> reqwest::Client {
    static SHARED: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    SHARED
        .get_or_init(|| {
            reqwest::Client::builder()
                .build()
                .expect("a default rustls reqwest client is always constructible")
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use jiff::Timestamp;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    use super::*;
    use crate::auth::{AuthClient, PersistedSession};

    // ── Minimal in-process HTTP/1.1 mock (real reqwest over TCP; no booted server) ──────

    struct Recorded {
        path: String,
        authorization: Option<String>,
    }

    struct MockResponse {
        status: u16,
        body: String,
    }

    type BoxFut = Pin<Box<dyn Future<Output = MockResponse> + Send>>;
    type Handler = Arc<dyn Fn(String) -> BoxFut + Send + Sync>;

    struct MockServer {
        base_url: String,
        requests: Arc<Mutex<Vec<Recorded>>>,
    }

    async fn start_mock(handler: Handler) -> MockServer {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let requests_srv = requests.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    break;
                };
                let handler = handler.clone();
                let requests = requests_srv.clone();
                tokio::spawn(async move {
                    let _ = serve_conn(&mut socket, handler, requests).await;
                });
            }
        });
        MockServer {
            base_url: format!("http://{addr}"),
            requests,
        }
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    async fn serve_conn(
        socket: &mut TcpStream,
        handler: Handler,
        requests: Arc<Mutex<Vec<Recorded>>>,
    ) -> std::io::Result<()> {
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
            if let Some((k, v)) = line.split_once(':') {
                let k = k.trim().to_lowercase();
                let v = v.trim().to_string();
                if k == "content-length" {
                    content_length = v.parse().unwrap_or(0);
                }
                headers.insert(k, v);
            }
        }
        // Drain the body so keep-alive clients don't wedge (we do not inspect it here).
        let mut body = buf[header_end..].to_vec();
        while body.len() < content_length {
            let n = socket.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }

        requests.lock().unwrap().push(Recorded {
            path: path.clone(),
            authorization: headers.get("authorization").cloned(),
        });

        let response = handler(path).await;
        let payload = format!(
            "HTTP/1.1 {} STATUS\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            response.status,
            response.body.len(),
            response.body
        );
        socket.write_all(payload.as_bytes()).await?;
        socket.flush().await?;
        Ok(())
    }

    fn token_json(access: &str, refresh: &str, expires_by: i64) -> String {
        serde_json::json!({
            "access_token": access,
            "refresh_token": refresh,
            "token_type": "Bearer",
            "expires_by": expires_by,
        })
        .to_string()
    }

    /// Build a session directly from a persisted token pair (no login round-trip), pointed at
    /// `base_url` for its refresh endpoint.
    fn session_with(base_url: &str, access: &str, refresh: &str, expires_at_unix: i64) -> Session {
        AuthClient::new(base_url)
            .unwrap()
            .resume(PersistedSession {
                access_token: access.into(),
                refresh_token: refresh.into(),
                access_expires_at_unix: expires_at_unix,
            })
            .unwrap()
    }

    fn far_future() -> i64 {
        Timestamp::now().as_second() + 3600
    }

    // ── Tests ───────────────────────────────────────────────────────────────────────────

    /// A typed 200 response deserializes into the generated model — proving the generated
    /// client + embedded runtime round-trips a real body over the wire. `get_version` is
    /// unauthenticated, isolating the decode path.
    #[tokio::test]
    async fn typed_response_deserializes() {
        let handler: Handler = Arc::new(|path| {
            Box::pin(async move {
                match path.as_str() {
                    "/v1/version" => MockResponse {
                        status: 200,
                        body: r#"{"name":"capsule-api","version":"9.9.9"}"#.to_string(),
                    },
                    _ => MockResponse {
                        status: 404,
                        body: "{}".to_string(),
                    },
                }
            })
        });
        let server = start_mock(handler).await;
        let session = session_with(&server.base_url, "access-1", "refresh-1", far_future());
        let client = AuthenticatedClient::new(&server.base_url, session).unwrap();

        // Called straight through the Deref to the generated client.
        let version = client.get_version().await.unwrap().into_inner();
        assert_eq!(version.name.as_str(), "capsule-api");
        assert_eq!(version.version.as_str(), "9.9.9");
    }

    /// An authenticated operation carries the session's access token as a bearer header —
    /// proving the token-provider seam attaches the credential the schema's `security`
    /// requirement names.
    #[tokio::test]
    async fn authenticated_call_flows_bearer_header() {
        let handler: Handler = Arc::new(|path| {
            Box::pin(async move {
                match path.as_str() {
                    "/v1/quota" => MockResponse {
                        status: 200,
                        body: r#"{"state":"ok","used":0}"#.to_string(),
                    },
                    _ => MockResponse {
                        status: 404,
                        body: "{}".to_string(),
                    },
                }
            })
        });
        let server = start_mock(handler).await;
        let session = session_with(&server.base_url, "access-1", "refresh-1", far_future());
        let client = AuthenticatedClient::new(&server.base_url, session).unwrap();

        let quota = client.get_quota().await.unwrap().into_inner();
        assert_eq!(quota.used, 0);

        let requests = server.requests.lock().unwrap();
        let quota_req = requests
            .iter()
            .find(|r| r.path == "/v1/quota")
            .expect("quota endpoint was hit");
        assert_eq!(
            quota_req.authorization.as_deref(),
            Some("Bearer access-1"),
            "the session's access token must ride as a bearer credential"
        );
    }

    /// An RFC 9457 problem body shaped as the generated `CodedProblem`, so a documented
    /// non-success status parses into the operation's typed error rather than a decode
    /// failure.
    fn problem(status: u16, code: &str) -> String {
        serde_json::json!({
            "type": "about:blank",
            "title": "Unauthorized",
            "status": status,
            "detail": "the access token was refused",
            "code": code,
        })
        .to_string()
    }

    /// How many requests the mock saw for `path`.
    fn hits(server: &MockServer, path: &str) -> usize {
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path == path)
            .count()
    }

    /// The bearer each request for `path` carried, in order.
    fn bearers(server: &MockServer, path: &str) -> Vec<Option<String>> {
        server
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.path == path)
            .map(|r| r.authorization.clone())
            .collect()
    }

    // ── S-D17: the reactive half ────────────────────────────────────────────────────────

    /// **The slice's Done-when.** A token that is valid as far as the *client* can tell and
    /// refused by the server — the race the pre-flight check cannot close — is refreshed once
    /// and the call replayed once, and the replay carries the rotated token.
    #[tokio::test]
    async fn a_401_is_refreshed_once_and_the_call_replayed() {
        let seen = Arc::new(AtomicUsize::new(0));
        let quota_calls = seen.clone();
        let handler: Handler = Arc::new(move |path| {
            let quota_calls = quota_calls.clone();
            Box::pin(async move {
                match path.as_str() {
                    "/refresh" => MockResponse {
                        status: 200,
                        body: token_json("access-2", "refresh-2", far_future()),
                    },
                    // The first attempt is refused; the replay is honoured. A server that
                    // revoked the session mid-flight looks exactly like this.
                    "/v1/quota" => {
                        if quota_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                            MockResponse {
                                status: 401,
                                body: problem(
                                    401,
                                    capsule_i18n::error_codes::REQUEST_UNAUTHENTICATED,
                                ),
                            }
                        } else {
                            MockResponse {
                                status: 200,
                                body: r#"{"state":"ok","used":5}"#.to_string(),
                            }
                        }
                    }
                    _ => MockResponse {
                        status: 404,
                        body: "{}".to_string(),
                    },
                }
            })
        });
        let server = start_mock(handler).await;
        // Far-future expiry: the pre-flight check is satisfied, so the *only* thing that can
        // rescue this call is the reactive layer.
        let session = session_with(&server.base_url, "access-1", "refresh-1", far_future());
        let client = AuthenticatedClient::new(&server.base_url, session).unwrap();

        let quota = client.get_quota().await.unwrap().into_inner();
        assert_eq!(
            quota.used, 5,
            "the replayed call is the one the caller sees"
        );

        assert_eq!(hits(&server, "/refresh"), 1, "exactly one refresh");
        assert_eq!(
            hits(&server, "/v1/quota"),
            2,
            "one attempt, one replay — no loop"
        );
        assert_eq!(
            bearers(&server, "/v1/quota"),
            vec![
                Some("Bearer access-1".to_string()),
                Some("Bearer access-2".to_string()),
            ],
            "the replay must carry the rotated token, not the one the server just refused"
        );
    }

    /// A server that refuses every credential is answered with exactly one replay, and the
    /// second `401` reaches the caller as the operation's typed error carrying the server's
    /// own `error.*` code. `exactly once` is the property: not twice, not a loop.
    #[tokio::test]
    async fn a_persistent_401_is_surfaced_after_exactly_one_replay() {
        let handler: Handler = Arc::new(|path| {
            Box::pin(async move {
                match path.as_str() {
                    "/refresh" => MockResponse {
                        status: 200,
                        body: token_json("access-2", "refresh-2", far_future()),
                    },
                    "/v1/quota" => MockResponse {
                        status: 401,
                        body: problem(401, capsule_i18n::error_codes::REQUEST_UNAUTHENTICATED),
                    },
                    _ => MockResponse {
                        status: 404,
                        body: "{}".to_string(),
                    },
                }
            })
        });
        let server = start_mock(handler).await;
        let session = session_with(&server.base_url, "access-1", "refresh-1", far_future());
        let client = AuthenticatedClient::new(&server.base_url, session).unwrap();

        let error = client
            .get_quota()
            .await
            .expect_err("a credential the server never honours must fail");
        let rest::Error::Api(response) = &error else {
            panic!("expected the operation's typed API error, got {error:?}");
        };
        let rest::GetQuotaError::Status401(problem) = response.inner() else {
            panic!("expected a typed 401, got {:?}", response.inner());
        };
        assert_eq!(
            problem.code,
            capsule_i18n::error_codes::REQUEST_UNAUTHENTICATED
        );

        assert_eq!(hits(&server, "/refresh"), 1);
        assert_eq!(
            hits(&server, "/v1/quota"),
            2,
            "exactly one replay — a retry loop would keep going"
        );
    }

    /// When the refresh itself fails, the caller gets the **server's** `401` back rather than
    /// a synthesized transport error — so the typed `Status401` mapping still fires and the
    /// `error.*` code survives. That code is the only thing separating an expired token from
    /// an unreadable revocation ledger, which the server also renders as `401`.
    #[tokio::test]
    async fn a_401_whose_refresh_fails_keeps_the_servers_own_401() {
        let handler: Handler = Arc::new(|path| {
            Box::pin(async move {
                match path.as_str() {
                    // The refresh token is gone too: nothing here can be rescued.
                    "/refresh" => MockResponse {
                        status: 401,
                        body: problem(401, capsule_i18n::error_codes::AUTH_SESSION_EXPIRED),
                    },
                    "/v1/quota" => MockResponse {
                        status: 401,
                        body: problem(401, capsule_i18n::error_codes::AUTH_UNAVAILABLE),
                    },
                    _ => MockResponse {
                        status: 404,
                        body: "{}".to_string(),
                    },
                }
            })
        });
        let server = start_mock(handler).await;
        let session = session_with(&server.base_url, "access-1", "refresh-1", far_future());
        let client = AuthenticatedClient::new(&server.base_url, session).unwrap();

        let error = client
            .get_quota()
            .await
            .expect_err("nothing can rescue this");
        let rest::Error::Api(response) = &error else {
            panic!("a failed refresh must not mask the 401 as a transport error: {error:?}");
        };
        let rest::GetQuotaError::Status401(problem) = response.inner() else {
            panic!("expected a typed 401, got {:?}", response.inner());
        };
        assert_eq!(
            problem.code,
            capsule_i18n::error_codes::AUTH_UNAVAILABLE,
            "the code the caller reads is the one the *operation* answered, not the refresh's"
        );
        assert_eq!(
            hits(&server, "/v1/quota"),
            1,
            "a refresh that failed produces nothing worth replaying"
        );
    }

    /// An unauthenticated operation's `401` is the server's answer about the request, not
    /// about a stale token: there is no bearer to refresh, so nothing is refreshed and
    /// nothing is replayed.
    #[tokio::test]
    async fn an_unauthenticated_401_is_never_retried() {
        let handler: Handler = Arc::new(|path| {
            Box::pin(async move {
                match path.as_str() {
                    "/refresh" => MockResponse {
                        status: 200,
                        body: token_json("access-2", "refresh-2", far_future()),
                    },
                    _ => MockResponse {
                        status: 401,
                        body: problem(401, capsule_i18n::error_codes::REQUEST_UNAUTHENTICATED),
                    },
                }
            })
        });
        let server = start_mock(handler).await;
        let session = session_with(&server.base_url, "access-1", "refresh-1", far_future());
        let client = AuthenticatedClient::new(&server.base_url, session).unwrap();

        // `get_version` declares no security requirement, so the generated client attaches no
        // credential at all.
        client
            .get_version()
            .await
            .expect_err("the mock refuses everything");
        assert_eq!(hits(&server, "/v1/version"), 1, "one request, no replay");
        assert_eq!(hits(&server, "/refresh"), 0, "and no refresh");
        assert_eq!(bearers(&server, "/v1/version"), vec![None]);
    }

    /// The session's pre-flight refresh fires *through the revived client*: with a stored
    /// access token already past expiry, the first typed call refreshes once (via the token
    /// provider), and the API request carries the rotated token — not the stale one.
    #[tokio::test]
    async fn preflight_refresh_fires_through_client() {
        let refresh_calls = Arc::new(AtomicUsize::new(0));
        let rc = refresh_calls.clone();
        let handler: Handler = Arc::new(move |path| {
            let rc = rc.clone();
            Box::pin(async move {
                match path.as_str() {
                    "/refresh" => {
                        rc.fetch_add(1, Ordering::SeqCst);
                        MockResponse {
                            status: 200,
                            body: token_json("access-2", "refresh-2", far_future()),
                        }
                    }
                    "/v1/quota" => MockResponse {
                        status: 200,
                        body: r#"{"state":"ok","used":7}"#.to_string(),
                    },
                    _ => MockResponse {
                        status: 404,
                        body: "{}".to_string(),
                    },
                }
            })
        });
        let server = start_mock(handler).await;
        // access-1 expired an hour ago → the pre-flight expiry check must refresh before the
        // request leaves.
        let session = session_with(
            &server.base_url,
            "access-1",
            "refresh-1",
            Timestamp::now().as_second() - 3600,
        );
        let client = AuthenticatedClient::new(&server.base_url, session).unwrap();

        let quota = client.get_quota().await.unwrap().into_inner();
        assert_eq!(quota.used, 7);

        assert_eq!(
            refresh_calls.load(Ordering::SeqCst),
            1,
            "exactly one pre-flight refresh must fire"
        );
        let requests = server.requests.lock().unwrap();
        let quota_req = requests
            .iter()
            .find(|r| r.path == "/v1/quota")
            .expect("quota endpoint was hit");
        assert_eq!(
            quota_req.authorization.as_deref(),
            Some("Bearer access-2"),
            "the refreshed token — not the stale one — must ride the typed call"
        );
    }
}
