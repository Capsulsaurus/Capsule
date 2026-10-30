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
    crate::net::negotiation::forget_addr(listener.local_addr().unwrap());
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

/// A `202` is a second-factor challenge, not a malformed token pair.
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
    let Err(error) = client
        .login("a@example.com", "pw")
        .await
        .unwrap()
        .into_session()
    else {
        panic!("a challenge is not a session")
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
            retry_after_secs: 42,
            ..
        }
    ));
    assert_eq!(error.error_code(), Some(error_codes::AUTH_RATE_LIMITED));
}

/// A registration `429` keeps the server's code: a full limiter is
/// `error.auth.registration_at_capacity`, not the spent-budget code, so the client renders
/// the message the server chose.
#[tokio::test]
async fn a_full_registration_limiter_keeps_its_at_capacity_code() {
    let handler: Handler = Arc::new(move |req| {
        Box::pin(async move {
            match req.path.as_str() {
                "/register" => MockResponse::json(
                    429,
                    r#"{"error":"Too many registrations","code":"error.auth.registration_at_capacity"}"#,
                )
                .header("retry-after", "7"),
                _ => MockResponse::json(404, r#"{"error":"x"}"#),
            }
        })
    });

    let server = start_mock(handler).await;
    let client = AuthClient::new(&server.base_url).unwrap();
    let error = client
        .register("a@example.com", "pw")
        .await
        .err()
        .expect("refused");

    assert!(matches!(
        error,
        AuthError::RateLimited {
            retry_after_secs: 7,
            ..
        }
    ));
    assert_eq!(
        error.error_code(),
        Some(error_codes::AUTH_REGISTRATION_AT_CAPACITY)
    );
}

/// A `429` without a code still names the spent-budget code, so a client always has one to
/// render.
#[test]
fn an_uncoded_429_falls_back_to_the_rate_limited_code() {
    let error = AuthError::RateLimited {
        retry_after_secs: 1,
        code: None,
    };
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
                "/refresh" => MockResponse::json(401, r#"{"error":"Invalid or expired token"}"#),
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

/// A session moved onto another HTTP client keeps the one token store: clearing it through
/// either handle clears it for both.
#[tokio::test]
async fn a_session_on_another_client_shares_its_tokens() {
    let handler: Handler =
        Arc::new(move |_req| Box::pin(async move { MockResponse::json(404, r#"{"error":"x"}"#) }));
    let server = start_mock(handler).await;
    let client = AuthClient::new(&server.base_url).unwrap();
    let session = client
        .resume(PersistedSession {
            access_token: "a".into(),
            refresh_token: "r".into(),
            access_expires_at_unix: far_future(),
        })
        .unwrap();
    let moved = session.with_http_client(crate::net::http_client().unwrap());
    assert_eq!(stored_access(&moved).await, "a");

    *session.inner.tokens.write().await = None;
    assert!(!moved.is_authenticated().await);
}

/// An operation on an empty session reports `NotAuthenticated`.
#[tokio::test]
async fn refresh_without_session_is_not_authenticated() {
    let handler: Handler =
        Arc::new(move |_req| Box::pin(async move { MockResponse::json(404, r#"{"error":"x"}"#) }));
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

// ── OIDC ───────────────────────────────────────────────────────────

/// An RFC 9457 problem carrying the stable code, as the server's coded-problem interceptor
/// renders one — the generated client parses refusals into this shape.
fn problem(status: u16, code: &str) -> MockResponse {
    MockResponse::json(
        status,
        serde_json::json!({
            "type": "about:blank",
            "title": "refused",
            "status": status,
            "detail": "the double refuses on purpose",
            "code": code,
        })
        .to_string(),
    )
}

/// A server with an identity provider, answering the two OIDC routes and recording what
/// the callback received.
fn oidc_handler(captured: Arc<std::sync::Mutex<Vec<serde_json::Value>>>) -> Handler {
    Arc::new(move |req: MockRequest| {
        let captured = captured.clone();
        Box::pin(async move {
            if let Ok(body) = serde_json::from_str(&req.body) {
                captured.lock().unwrap().push(body);
            }
            match req.path.as_str() {
                "/v1/auth/oidc/authorize" => MockResponse::json(
                    200,
                    serde_json::json!({
                        "authorization_url": "https://idp.test/authorize?state=state-1",
                        "state": "state-1",
                        "expires_by": 1_893_456_000,
                    })
                    .to_string(),
                ),
                "/v1/auth/oidc/callback" => {
                    // The generated client sends the handshake header on every request.
                    assert_eq!(
                        req.headers.get("x-capsule-protocol").map(String::as_str),
                        Some(PROTOCOL_VERSION)
                    );
                    MockResponse::json(200, token_json("access-1", "refresh-1", far_future()))
                }
                _ => MockResponse::json(404, r#"{"error":"x"}"#),
            }
        })
    })
}

/// A begun ceremony as a test that never ran the begin leg would hold it.
fn pending(state: &str) -> OidcAuthorization {
    OidcAuthorization {
        authorization_url: "https://idp.test/authorize".to_owned(),
        state: SecretString::from(state.to_owned()),
        binding: fresh_oidc_binding(),
        expires_by: 1_893_456_000,
    }
}

/// The two legs round-trip, the cohort rides the callback rather than the authorize, and the
/// callback presents the secret whose hash the authorize carried (issue #517).
#[tokio::test]
async fn an_oidc_login_begins_completes_and_carries_the_cohort_on_the_callback() {
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = start_mock(oidc_handler(captured.clone())).await;
    let client = AuthClient::new(&server.base_url)
        .unwrap()
        .with_cohort_hash("a-particular-machine".to_owned());

    let begun = client
        .begin_oidc_login("http://127.0.0.1:4242/callback")
        .await
        .unwrap();
    assert_eq!(
        begun.authorization_url,
        "https://idp.test/authorize?state=state-1"
    );
    assert_eq!(begun.state.expose_secret(), "state-1");
    assert_eq!(begun.expires_by, 1_893_456_000);

    let session = finished(
        client
            .complete_oidc_login(&begun, "state-1", "code-1")
            .await
            .unwrap(),
    );
    assert!(session.is_authenticated().await);

    let bodies = captured.lock().unwrap().clone();
    let [authorize, callback] = bodies.as_slice() else {
        panic!("one authorize and one callback body, got {bodies:?}");
    };
    // Only the hash left on the authorize; the secret itself rides only the callback.
    let binding = begun.binding.expose_secret();
    assert_eq!(binding.len(), 43, "256 bits, unpadded base64url");
    assert_eq!(
        authorize["binding_challenge"],
        oidc_binding_challenge(&begun.binding)
    );
    assert!(!authorize.to_string().contains(binding));
    assert_eq!(callback["binding_verifier"], binding);
    assert_eq!(callback["state"], "state-1");
    assert_eq!(callback["code"], "code-1");
    assert_eq!(callback["cohort_hash"], "a-particular-machine");
    assert!(authorize.get("cohort_hash").is_none());

    // A second ceremony gets a binding of its own.
    let again = client
        .begin_oidc_login("http://127.0.0.1:4242/callback")
        .await
        .unwrap();
    assert_ne!(again.binding.expose_secret(), binding);
}

/// The binding challenge is the S256 of the binding: RFC 7636 Appendix B's vector.
#[test]
fn the_binding_challenge_is_the_s256_of_the_binding() {
    assert_eq!(
        oidc_binding_challenge(&SecretString::from(
            "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".to_owned()
        )),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

/// A redirect carrying a `state` this client did not begin — another client's ceremony, a
/// login-CSRF — is refused before anything reaches the server.
#[tokio::test]
async fn a_redirect_for_another_ceremony_is_refused_without_a_request() {
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = start_mock(oidc_handler(captured.clone())).await;
    let client = AuthClient::new(&server.base_url).unwrap();

    let error = expect_login_err(
        client
            .complete_oidc_login(&pending("state-1"), "the-attackers-state", "code-1")
            .await,
    );
    assert!(matches!(error, AuthError::OidcRejected { .. }), "{error:?}");
    assert_eq!(
        error.error_code(),
        Some(error_codes::AUTH_OIDC_STATE_INVALID)
    );
    assert!(captured.lock().unwrap().is_empty(), "nothing was sent");
}

/// The callback's `202` is a second-factor challenge, as the password login's is.
#[tokio::test]
async fn an_oidc_callback_can_answer_a_second_factor_challenge() {
    let handler: Handler = Arc::new(move |req| {
        Box::pin(async move {
            match req.path.as_str() {
                "/v1/auth/oidc/callback" => MockResponse::json(
                    202,
                    r#"{"mfa_token":"challenge-1","expires_by":1893456000}"#,
                ),
                _ => MockResponse::json(404, r#"{"error":"x"}"#),
            }
        })
    });
    let server = start_mock(handler).await;
    let client = AuthClient::new(&server.base_url).unwrap();
    let outcome = client
        .complete_oidc_login(&pending("state-1"), "state-1", "code-1")
        .await
        .unwrap();
    assert!(matches!(outcome, LoginOutcome::SecondFactorRequired { .. }));
}

/// Each OIDC refusal maps to its typed variant on the catalog code, and the `401`s to one.
#[tokio::test]
async fn oidc_refusals_map_to_typed_errors_on_their_codes() {
    let handler: Handler = Arc::new(move |req| {
        Box::pin(async move {
            match (req.path.as_str(), req.body.contains("evil")) {
                ("/v1/auth/oidc/authorize", true) => {
                    problem(400, "error.auth.oidc_redirect_invalid")
                }
                ("/v1/auth/oidc/authorize", false) => {
                    problem(404, "error.auth.oidc_not_configured")
                }
                ("/v1/auth/oidc/callback", _) if req.body.contains("taken") => {
                    problem(409, "error.auth.oidc_address_taken")
                }
                ("/v1/auth/oidc/callback", _) => problem(401, "error.auth.oidc_state_invalid"),
                _ => MockResponse::json(404, r#"{"error":"x"}"#),
            }
        })
    });
    let server = start_mock(handler).await;
    let client = AuthClient::new(&server.base_url).unwrap();

    let error = client
        .begin_oidc_login("https://evil.example.test/cb")
        .await
        .err()
        .expect("refused");
    assert!(matches!(error, AuthError::OidcRedirectInvalid), "{error:?}");
    assert_eq!(
        error.error_code(),
        Some(error_codes::AUTH_OIDC_REDIRECT_INVALID)
    );

    let error = client
        .begin_oidc_login("http://127.0.0.1:4242/cb")
        .await
        .err()
        .expect("refused");
    assert!(matches!(error, AuthError::OidcNotConfigured), "{error:?}");
    assert_eq!(
        error.error_code(),
        Some(error_codes::AUTH_OIDC_NOT_CONFIGURED)
    );

    let error = expect_login_err(
        client
            .complete_oidc_login(&pending("state-1"), "state-1", "taken")
            .await,
    );
    assert!(matches!(error, AuthError::OidcAddressTaken), "{error:?}");
    assert_eq!(
        error.error_code(),
        Some(error_codes::AUTH_OIDC_ADDRESS_TAKEN)
    );

    let error = expect_login_err(
        client
            .complete_oidc_login(&pending("state-1"), "state-1", "code-1")
            .await,
    );
    assert!(matches!(error, AuthError::OidcRejected { .. }), "{error:?}");
    assert_eq!(
        error.error_code(),
        Some(error_codes::AUTH_OIDC_STATE_INVALID)
    );
}

/// A begun device sign-in as a test that never ran the begin leg would hold it.
fn device_pending(handle: &str) -> OidcDeviceAuthorization {
    OidcDeviceAuthorization {
        handle: SecretString::from(handle.to_owned()),
        binding: fresh_oidc_binding(),
        user_code: "WDJB-MJHT".to_owned(),
        verification_uri: "https://idp.test/device".to_owned(),
        verification_uri_complete: None,
        interval_secs: 5,
        expires_by: 1_893_456_000,
    }
}

/// The device poll's answer, for the cases that expect it to have finished.
fn completed(poll: OidcDevicePoll) -> LoginOutcome {
    match poll {
        OidcDevicePoll::Complete(outcome) => outcome,
        OidcDevicePoll::Pending | OidcDevicePoll::SlowDown => {
            panic!("the fixture answered keep-polling")
        }
    }
}

/// The device legs round-trip (issue #635): the begin sends only the binding's hash, every
/// poll presents the secret and the handle, the cohort rides the polls, and pending and
/// slow-down are answers rather than errors.
#[tokio::test]
async fn a_device_login_begins_polls_and_completes() {
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let polls = Arc::new(AtomicUsize::new(0));
    let handler: Handler = {
        let captured = captured.clone();
        let polls = polls.clone();
        Arc::new(move |req: MockRequest| {
            let captured = captured.clone();
            let polls = polls.clone();
            Box::pin(async move {
                if let Ok(body) = serde_json::from_str::<serde_json::Value>(&req.body) {
                    captured.lock().unwrap().push(body);
                }
                match req.path.as_str() {
                    "/v1/auth/oidc/device" => MockResponse::json(
                        200,
                        serde_json::json!({
                            "handle": "handle-1",
                            "user_code": "WDJB-MJHT",
                            "verification_uri": "https://idp.test/device",
                            "verification_uri_complete":
                                "https://idp.test/device?user_code=WDJB-MJHT",
                            "interval": 5,
                            "expires_by": 1_893_456_000,
                        })
                        .to_string(),
                    ),
                    "/v1/auth/oidc/device/poll" => match polls.fetch_add(1, Ordering::SeqCst) {
                        0 => problem(400, "error.auth.oidc_device_pending"),
                        1 => problem(429, "error.auth.oidc_device_slow_down"),
                        _ => MockResponse::json(
                            200,
                            token_json("access-1", "refresh-1", far_future()),
                        ),
                    },
                    _ => MockResponse::json(404, r#"{"error":"x"}"#),
                }
            })
        })
    };
    let server = start_mock(handler).await;
    let client = AuthClient::new(&server.base_url)
        .unwrap()
        .with_cohort_hash("a-particular-machine".to_owned());

    let mut begun = client.begin_oidc_device_login().await.unwrap();
    assert_eq!(begun.handle.expose_secret(), "handle-1");
    assert_eq!(begun.user_code, "WDJB-MJHT");
    assert_eq!(begun.verification_uri, "https://idp.test/device");
    assert_eq!(
        begun.verification_uri_complete.as_deref(),
        Some("https://idp.test/device?user_code=WDJB-MJHT")
    );
    assert_eq!(begun.interval_secs, 5);
    assert_eq!(begun.expires_by, 1_893_456_000);

    assert!(matches!(
        client.poll_oidc_device_login(&begun).await.unwrap(),
        OidcDevicePoll::Pending
    ));
    assert!(matches!(
        client.poll_oidc_device_login(&begun).await.unwrap(),
        OidcDevicePoll::SlowDown
    ));
    begun.slow_down();
    assert_eq!(begun.interval_secs, 10, "RFC 8628 §3.5: five more seconds");
    let session = finished(completed(
        client.poll_oidc_device_login(&begun).await.unwrap(),
    ));
    assert!(session.is_authenticated().await);

    let bodies = captured.lock().unwrap().clone();
    let [device, polls @ ..] = bodies.as_slice() else {
        panic!("a begin and three polls, got {bodies:?}");
    };
    let binding = begun.binding.expose_secret();
    assert_eq!(
        device["binding_challenge"],
        oidc_binding_challenge(&begun.binding)
    );
    assert!(!device.to_string().contains(binding));
    assert!(device.get("cohort_hash").is_none());
    assert_eq!(polls.len(), 3);
    for poll in polls {
        assert_eq!(poll["handle"], "handle-1");
        assert_eq!(poll["binding_verifier"], binding);
        assert_eq!(poll["cohort_hash"], "a-particular-machine");
    }
}

/// The device poll's `202` is a second-factor challenge, as the callback's is.
#[tokio::test]
async fn a_device_poll_can_answer_a_second_factor_challenge() {
    let handler: Handler = Arc::new(move |req| {
        Box::pin(async move {
            match req.path.as_str() {
                "/v1/auth/oidc/device/poll" => MockResponse::json(
                    202,
                    r#"{"mfa_token":"challenge-1","expires_by":1893456000}"#,
                ),
                _ => MockResponse::json(404, r#"{"error":"x"}"#),
            }
        })
    });
    let server = start_mock(handler).await;
    let client = AuthClient::new(&server.base_url).unwrap();
    let outcome = completed(
        client
            .poll_oidc_device_login(&device_pending("handle-1"))
            .await
            .unwrap(),
    );
    assert!(matches!(outcome, LoginOutcome::SecondFactorRequired { .. }));
}

/// Each device refusal maps to its typed variant on the catalog code.
#[tokio::test]
async fn device_refusals_map_to_typed_errors_on_their_codes() {
    let handler: Handler = Arc::new(move |req| {
        Box::pin(async move {
            let handle = serde_json::from_str::<serde_json::Value>(&req.body)
                .ok()
                .and_then(|body| body["handle"].as_str().map(str::to_owned))
                .unwrap_or_default();
            match (req.path.as_str(), handle.as_str()) {
                ("/v1/auth/oidc/device", _) => problem(404, "error.auth.oidc_device_unsupported"),
                (_, "denied") => problem(401, "error.auth.oidc_device_denied"),
                (_, "expired") => problem(401, "error.auth.oidc_device_expired"),
                (_, "taken") => problem(409, "error.auth.oidc_address_taken"),
                (_, "refused") => problem(401, "error.auth.oidc_token_invalid"),
                (_, "limited") => problem(429, "error.auth.rate_limited"),
                _ => MockResponse::json(404, r#"{"error":"x"}"#),
            }
        })
    });
    let server = start_mock(handler).await;
    let client = AuthClient::new(&server.base_url).unwrap();

    let error = client
        .begin_oidc_device_login()
        .await
        .err()
        .expect("refused");
    assert!(
        matches!(error, AuthError::OidcDeviceUnsupported),
        "{error:?}"
    );
    assert_eq!(
        error.error_code(),
        Some(error_codes::AUTH_OIDC_DEVICE_UNSUPPORTED)
    );

    for (handle, code) in [
        ("denied", error_codes::AUTH_OIDC_DEVICE_DENIED),
        ("expired", error_codes::AUTH_OIDC_DEVICE_EXPIRED),
        ("taken", error_codes::AUTH_OIDC_ADDRESS_TAKEN),
        ("refused", error_codes::AUTH_OIDC_TOKEN_INVALID),
        ("limited", error_codes::AUTH_RATE_LIMITED),
    ] {
        let error = client
            .poll_oidc_device_login(&device_pending(handle))
            .await
            .err()
            .expect("refused");
        assert_eq!(error.error_code(), Some(code), "{handle}: {error:?}");
        let typed = match handle {
            "denied" => matches!(error, AuthError::OidcDeviceDenied),
            "expired" => matches!(error, AuthError::OidcDeviceExpired),
            "taken" => matches!(error, AuthError::OidcAddressTaken),
            "refused" => matches!(error, AuthError::OidcRejected { .. }),
            _ => matches!(error, AuthError::RateLimited { .. }),
        };
        assert!(typed, "{handle}: {error:?}");
    }
}

/// The generated client is built on the server root: the auth base minus `/v1/auth`.
#[test]
fn the_server_root_is_the_auth_base_without_its_suffix() {
    let endpoints = AuthEndpoints::from_base("https://api.example.test/v1/auth/").unwrap();
    assert_eq!(endpoints.server_root, "https://api.example.test");
    assert_eq!(endpoints.login, "https://api.example.test/v1/auth/login");
    let bare = AuthEndpoints::from_base("http://127.0.0.1:4242").unwrap();
    assert_eq!(bare.server_root, "http://127.0.0.1:4242");
}

#[test]
fn rejects_invalid_base_url() {
    assert!(matches!(
        AuthClient::new("not a url"),
        Err(AuthError::InvalidBaseUrl { .. })
    ));
}

// ── Cohort emission ───────────────────────────────────────────────

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
/// none of them, and its body is strict — so a field this client added for
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
/// (no `null`), so the server behaves identically to a valid one (the cohort hash is
/// advisory only).
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
