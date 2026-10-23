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
    crate::net::negotiation::forget_addr(listener.local_addr().unwrap());
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

// ── the reactive half ────────────────────────────────────────────────────────

/// **The acceptance case.** A token that is valid as far as the *client* can tell and
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
                            body: problem(401, capsule_i18n::error_codes::REQUEST_UNAUTHENTICATED),
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

    let quota = client
        .get_quota(capsule_core::crypto::primitives::PROTOCOL_VERSION, None)
        .await
        .unwrap()
        .into_inner();
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
        .get_quota(capsule_core::crypto::primitives::PROTOCOL_VERSION, None)
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
/// `error.*` code the *operation* answered survives, not the refresh's.
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
        .get_quota(capsule_core::crypto::primitives::PROTOCOL_VERSION, None)
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
        capsule_i18n::error_codes::REQUEST_UNAUTHENTICATED,
        "the code the caller reads is the one the *operation* answered, not the refresh's"
    );
    assert_eq!(
        hits(&server, "/refresh"),
        1,
        "the expired token was refreshed"
    );
    assert_eq!(
        hits(&server, "/v1/quota"),
        1,
        "a refresh that failed produces nothing worth replaying"
    );
}

/// Serve `/v1/quota` a `401` with `body` and a refresh that would succeed, call it once,
/// and return the server so the caller can count what the layer did.
async fn quota_answering_401(body: String) -> (MockServer, rest::Error<rest::GetQuotaError>) {
    let handler: Handler = Arc::new(move |path| {
        let body = body.clone();
        Box::pin(async move {
            match path.as_str() {
                "/refresh" => MockResponse {
                    status: 200,
                    body: token_json("access-2", "refresh-2", far_future()),
                },
                "/v1/quota" => MockResponse { status: 401, body },
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
        .get_quota(capsule_core::crypto::primitives::PROTOCOL_VERSION, None)
        .await
        .expect_err("the mock refuses the operation");
    (server, error)
}

/// **Issue #511's Done-when.** A `401` an operation answers about the credential in its
/// payload — a wrong password on `reauthenticate`, a wrong code on `totp_disable` — is not
/// an expired token. Replaying it would charge the lockout twice and spend the refresh
/// token, so it reaches the caller untouched, as the operation's typed `401` carrying the
/// server's own code, after exactly one request and no refresh. The ledger-outage code is
/// in the set for the same reason: refreshing cannot cure an outage.
#[tokio::test]
async fn a_credential_401_is_neither_refreshed_nor_replayed() {
    for code in [
        capsule_i18n::error_codes::AUTH_INVALID_CREDENTIALS,
        capsule_i18n::error_codes::AUTH_SESSION_EXPIRED,
        capsule_i18n::error_codes::AUTH_UNAVAILABLE,
    ] {
        let (server, error) = quota_answering_401(problem(401, code)).await;
        let rest::Error::Api(response) = &error else {
            panic!("{code}: expected the operation's typed API error, got {error:?}");
        };
        let rest::GetQuotaError::Status401(problem) = response.inner() else {
            panic!("{code}: expected a typed 401, got {:?}", response.inner());
        };
        assert_eq!(
            problem.code, code,
            "the server's own code survives the read"
        );
        assert_eq!(hits(&server, "/v1/quota"), 1, "{code}: no replay");
        assert_eq!(hits(&server, "/refresh"), 0, "{code}: no refresh");
    }
}

/// A `401` whose body carries no problem code cannot be classified as an expired token,
/// so the layer fails closed: no refresh, no replay.
#[tokio::test]
async fn an_uncoded_401_is_neither_refreshed_nor_replayed() {
    for body in [
        String::new(),
        "not json".to_string(),
        r#"{"type":"about:blank","status":401}"#.to_string(),
    ] {
        let (server, _) = quota_answering_401(body.clone()).await;
        assert_eq!(hits(&server, "/v1/quota"), 1, "{body:?}: no replay");
        assert_eq!(hits(&server, "/refresh"), 0, "{body:?}: no refresh");
    }
}

/// Serve `/v1/quota` a `413` with `body` and return what the typed call reports.
async fn quota_answering_413(body: String) -> rest::Error<rest::GetQuotaError> {
    let handler: Handler = Arc::new(move |_| {
        let body = body.clone();
        Box::pin(async move { MockResponse { status: 413, body } })
    });
    let server = start_mock(handler).await;
    let session = session_with(&server.base_url, "access-1", "refresh-1", far_future());
    let client = AuthenticatedClient::new(&server.base_url, session).unwrap();
    client
        .get_quota(capsule_core::crypto::primitives::PROTOCOL_VERSION, None)
        .await
        .expect_err("the mock refuses the operation")
}

/// A `413` without a coded problem body — a server from before the contract coded it, or a
/// proxy's own `text/html` refusal — still reaches the caller as the operation's typed
/// `413`, coded `error.request.too_large`, rather than as a decode failure.
#[tokio::test]
async fn an_uncoded_413_is_still_the_typed_refusal() {
    for body in [
        String::new(),
        "<html><body>413 Request Entity Too Large</body></html>".to_string(),
        r#"{"type":"about:blank","status":413}"#.to_string(),
    ] {
        let error = quota_answering_413(body.clone()).await;
        let rest::Error::Api(response) = &error else {
            panic!("{body:?}: expected the typed API error, got {error:?}");
        };
        let rest::GetQuotaError::Status413(problem) = response.inner() else {
            panic!("{body:?}: expected a typed 413, got {:?}", response.inner());
        };
        assert_eq!(
            problem.code,
            capsule_i18n::error_codes::REQUEST_TOO_LARGE,
            "{body:?}"
        );
    }
}

/// A `413` that already carries a coded problem passes through with its own code.
#[tokio::test]
async fn a_coded_413_keeps_its_own_code() {
    let error = quota_answering_413(problem(413, "error.upload.chunk_too_large")).await;
    let rest::Error::Api(response) = &error else {
        panic!("expected the typed API error, got {error:?}");
    };
    let rest::GetQuotaError::Status413(problem) = response.inner() else {
        panic!("expected a typed 413, got {:?}", response.inner());
    };
    assert_eq!(problem.code, "error.upload.chunk_too_large");
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
