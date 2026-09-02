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
//! Scope: this covers the plain request/response REST surfaces the OpenAPI schema declares
//! (auth/session, quota, storage-verify, receipts, devices, escrow, …). The stateful upload
//! protocol ([`crate::upload`]) and the gRPC sync feed ([`crate::sync`]) stay hand-written —
//! they are deliberately *not* routed through the generated client.

use std::ops::Deref;
use std::sync::Arc;

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

/// Wire a generated client to `base_url` with a bearer credential that pulls a fresh access
/// token from `session` on demand (pre-flight refresh + single-flight live in the session).
fn build_client(base_url: &str, session: Session) -> Result<Client, ClientError> {
    let provider: rest::TokenProvider = Arc::new(move || {
        let session = session.clone();
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

    let client = Client::with_client(reqwest_client(), base_url)
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
fn reqwest_client() -> reqwest::Client {
    crate::net::http_client().expect("a default rustls reqwest client is always constructible")
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
        protocol: Option<String>,
        crypto_suite: Option<String>,
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
            protocol: headers.get("x-capsule-protocol").cloned(),
            crypto_suite: headers.get("x-capsule-crypto-suite").cloned(),
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

    /// Every request the typed client sends carries the protocol handshake (issue #404) —
    /// proving the transport-level default reaches the wire through the generated operation
    /// with no argument at the call site, on an operation the server does not even gate.
    #[tokio::test]
    async fn every_request_carries_the_protocol_handshake() {
        let handler: Handler = Arc::new(|_| {
            Box::pin(async move {
                MockResponse {
                    status: 200,
                    body: r#"{"name":"capsule-api","version":"9.9.9"}"#.to_string(),
                }
            })
        });
        let server = start_mock(handler).await;
        let session = session_with(&server.base_url, "access-1", "refresh-1", far_future());
        let client = AuthenticatedClient::new(&server.base_url, session).unwrap();

        client.get_version().await.unwrap();

        let requests = server.requests.lock().unwrap();
        let version = requests
            .iter()
            .find(|r| r.path == "/v1/version")
            .expect("version endpoint was hit");
        assert_eq!(
            version.protocol.as_deref(),
            Some(capsule_core::crypto::primitives::PROTOCOL_VERSION),
            "the protocol date this build speaks must ride every request"
        );
        assert_eq!(
            version.crypto_suite.as_deref(),
            Some(
                capsule_core::crypto::primitives::CRYPTO_SUITE_ID
                    .to_string()
                    .as_str()
            ),
            "and so must the suite it seals under"
        );
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

        let quota = client
            .get_quota(capsule_core::crypto::primitives::PROTOCOL_VERSION, None)
            .await
            .unwrap()
            .into_inner();
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

        let quota = client
            .get_quota(capsule_core::crypto::primitives::PROTOCOL_VERSION, None)
            .await
            .unwrap()
            .into_inner();
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
