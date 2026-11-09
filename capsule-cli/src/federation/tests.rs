//! The three subscription calls against a loopback double of the server: what each sends, and
//! how each reads the answers the contract declares.

use std::sync::{Arc, Mutex, PoisonError};

use capsule_sdk::auth::{AuthClient, PersistedSession};

use super::*;

/// One request the double read: `METHOD path`, the bearer, and the body.
#[derive(Debug, Clone, Default)]
struct Seen {
    line: String,
    authorization: Option<String>,
    body: String,
}

/// Serve `status` with `body` to every request on a loopback port; answer the origin and what
/// the double saw. Each connection answers one request and closes.
async fn serve(status: u16, body: &'static str) -> (String, Arc<Mutex<Vec<Seen>>>) {
    let content_type = if body.is_empty() {
        None
    } else if status >= 400 {
        Some("application/problem+json")
    } else {
        Some("application/json")
    };
    serve_as(status, content_type, body).await
}

/// [`serve`], answering every body as `content_type`.
async fn serve_as(
    status: u16,
    content_type: Option<&'static str>,
    body: &'static str,
) -> (String, Arc<Mutex<Vec<Seen>>>) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let origin = format!("http://{}", listener.local_addr().expect("an address"));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0_u8; 4096];
                let (head_end, length) = loop {
                    let read = socket.read(&mut chunk).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..read]);
                    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    break (end + 4, length);
                };
                while buf.len() < head_end + length {
                    let read = socket.read(&mut chunk).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..read]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let mut words = head.split_whitespace();
                let line = format!(
                    "{} {}",
                    words.next().unwrap_or_default(),
                    words.next().unwrap_or_default()
                );
                let authorization = head.lines().find_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.trim()
                        .eq_ignore_ascii_case("authorization")
                        .then(|| value.trim().to_owned())
                });
                log.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(Seen {
                        line,
                        authorization,
                        body: String::from_utf8_lossy(&buf[head_end..head_end + length])
                            .into_owned(),
                    });
                let content_type = content_type
                    .map(|content_type| format!("content-type: {content_type}\r\n"))
                    .unwrap_or_default();
                // The negotiation headers every answer of the real server carries.
                let window = format!(
                    "x-capsule-protocol-min: {PROTOCOL_VERSION}\r\n\
                     x-capsule-protocol-max: {PROTOCOL_VERSION}\r\n\
                     x-capsule-min-client-build: 0.0.0\r\n"
                );
                let response = format!(
                    "HTTP/1.1 {status} X\r\n{window}{content_type}content-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    (origin, seen)
}

/// A session whose access token is fresh for decades, so no request refreshes first.
fn session(origin: &str) -> Session {
    AuthClient::new(origin)
        .expect("the origin parses")
        .resume(PersistedSession {
            access_token: "access-token".into(),
            refresh_token: "refresh-token".into(),
            access_expires_at_unix: 4_000_000_000,
        })
        .expect("the session resumes")
}

const VIEW: &str = r#"{"peer":"home.test","album_id":"album-1","scope":"read","jti":"jti-1","issued_at":"2026-11-02T00:00:00Z","expires_at":"2026-11-03T00:00:00Z","subscribed_at":"2026-11-02T00:00:00Z","live":true}"#;

fn entry() -> SubscriptionEntry {
    SubscriptionEntry {
        peer: "home.test".to_owned(),
        album_id: "album-1".to_owned(),
        scope: "read".to_owned(),
        jti: "jti-1".to_owned(),
        issued_at: "2026-11-02T00:00:00Z".to_owned(),
        expires_at: "2026-11-03T00:00:00Z".to_owned(),
        subscribed_at: "2026-11-02T00:00:00Z".to_owned(),
        live: true,
    }
}

fn seen(log: &Arc<Mutex<Vec<Seen>>>) -> Vec<Seen> {
    log.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

#[tokio::test]
async fn subscribe_posts_the_issuer_and_token_and_reads_a_created_subscription() {
    let (origin, log) = serve(201, VIEW).await;
    let subscribed = post_subscription(&origin, session(&origin), "home.test", "the.token")
        .await
        .expect("the subscription is recorded");
    assert_eq!(
        subscribed,
        Subscribed {
            created: true,
            subscription: entry(),
        }
    );
    let requests = seen(&log);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].line, "POST /v1/federation/subscriptions");
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Bearer access-token"),
        "the member's own session, never the capability"
    );
    let body: serde_json::Value = serde_json::from_str(&requests[0].body).expect("a JSON body");
    assert_eq!(
        body,
        serde_json::json!({ "issuer": "home.test", "token": "the.token" })
    );
}

#[tokio::test]
async fn subscribe_reads_a_renewal_as_not_created() {
    let (origin, _log) = serve(200, VIEW).await;
    let subscribed = post_subscription(&origin, session(&origin), "home.test", "the.token")
        .await
        .expect("the renewal is answered");
    assert!(!subscribed.created);
    assert_eq!(subscribed.subscription, entry());
}

#[tokio::test]
async fn subscribe_carries_the_servers_code_on_a_refusal() {
    let (origin, _log) = serve(
        403,
        r#"{"type":"about:blank","title":"Forbidden","status":403,"code":"error.federation.subscription_limit"}"#,
    )
    .await;
    let error = post_subscription(&origin, session(&origin), "home.test", "the.token")
        .await
        .expect_err("the server refused");
    assert_eq!(
        error.error_code(),
        Some("error.federation.subscription_limit")
    );
    assert!(matches!(
        error,
        FederationCommandError::Refused { status: 403, .. }
    ));
    assert_eq!(
        describe_error(&error),
        error_message("error.federation.subscription_limit"),
        "a coded refusal renders through the English catalog"
    );
}

#[tokio::test]
async fn list_reads_every_subscription_in_order() {
    let (origin, log) = serve(
        200,
        r#"{"subscriptions":[{"peer":"home.test","album_id":"album-1","scope":"read","jti":"jti-1","issued_at":"2026-11-02T00:00:00Z","expires_at":"2026-11-03T00:00:00Z","subscribed_at":"2026-11-02T00:00:00Z","live":true},{"peer":"home.test","album_id":"album-2","scope":"read-derivative-only","jti":"jti-2","issued_at":"2026-11-01T00:00:00Z","expires_at":"2026-11-01T12:00:00Z","subscribed_at":"2026-11-01T00:00:00Z","live":false}]}"#,
    )
    .await;
    let listed = read_subscriptions(&origin, session(&origin))
        .await
        .expect("the list is answered");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0], entry());
    assert_eq!(listed[1].album_id, "album-2");
    assert_eq!(listed[1].scope, "read-derivative-only");
    assert!(!listed[1].live, "a lapsed subscription is listed as lapsed");
    assert_eq!(seen(&log)[0].line, "GET /v1/federation/subscriptions");
}

#[tokio::test]
async fn list_reads_a_deployment_that_does_not_federate_as_its_code() {
    let (origin, _log) = serve(
        404,
        r#"{"type":"about:blank","title":"Not found","status":404,"code":"error.federation.not_configured"}"#,
    )
    .await;
    let error = read_subscriptions(&origin, session(&origin))
        .await
        .expect_err("the server does not federate");
    assert_eq!(error.error_code(), Some("error.federation.not_configured"));
}

const PULLED_ASSET: &str = r#"{"asset_id":"asset-1","head":"ab","timestamp":"2026-11-02T00:00:00Z","author":"01937b7c-0000-7000-8000-0000000000b0","size":42}"#;

fn pulled_asset() -> PulledAssetEntry {
    PulledAssetEntry {
        asset_id: "asset-1".to_owned(),
        head: "ab".to_owned(),
        timestamp: "2026-11-02T00:00:00Z".to_owned(),
        author: "01937b7c-0000-7000-8000-0000000000b0".to_owned(),
        size: 42,
    }
}

#[tokio::test]
async fn assets_reads_pages_to_the_albums_end() {
    let body: &'static str = Box::leak(
        format!(
            r#"{{"peer":"home.test","album_id":"album-1","assets":[{PULLED_ASSET}],"next_after":null}}"#
        )
        .into_boxed_str(),
    );
    let (origin, log) = serve(200, body).await;
    let pulled = read_pulled_assets(&origin, session(&origin), "home.test", "album-1")
        .await
        .expect("the album is listed");
    assert_eq!(
        pulled,
        PulledAssets {
            peer: "home.test".to_owned(),
            album_id: "album-1".to_owned(),
            assets: vec![pulled_asset()],
        }
    );
    let requests = seen(&log);
    assert_eq!(requests.len(), 1, "a page with no next_after is the last");
    assert_eq!(
        requests[0].line,
        "GET /v1/federation/subscriptions/home.test/album-1/assets?limit=1000"
    );
    assert_eq!(
        requests[0].authorization.as_deref(),
        Some("Bearer access-token")
    );
}

#[tokio::test]
async fn assets_refuses_a_page_that_does_not_move_past_the_last() {
    let body: &'static str = Box::leak(
        format!(
            r#"{{"peer":"home.test","album_id":"album-1","assets":[{PULLED_ASSET}],"next_after":"asset-1"}}"#
        )
        .into_boxed_str(),
    );
    let (origin, log) = serve(200, body).await;
    let error = read_pulled_assets(&origin, session(&origin), "home.test", "album-1")
        .await
        .expect_err("a page repeating the last one never ends");
    assert!(
        matches!(error, FederationCommandError::Transport(_)),
        "{error:?}"
    );
    let requests = seen(&log);
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert_eq!(
        requests[1].line,
        "GET /v1/federation/subscriptions/home.test/album-1/assets?after=asset-1&limit=1000"
    );
}

#[tokio::test]
async fn assets_and_metadata_read_an_album_not_subscribed_to_as_its_code() {
    let refusal = r#"{"type":"about:blank","title":"Not found","status":404,"code":"error.federation.album_not_found"}"#;
    let (origin, _log) = serve(404, refusal).await;
    let error = read_pulled_assets(&origin, session(&origin), "home.test", "album-1")
        .await
        .expect_err("not subscribed");
    assert_eq!(error.error_code(), Some("error.federation.album_not_found"));
    let error = read_pulled_metadata(&origin, session(&origin), "home.test", "album-1", "a")
        .await
        .expect_err("not subscribed");
    assert_eq!(error.error_code(), Some("error.federation.album_not_found"));
    assert!(matches!(
        error,
        FederationCommandError::Refused { status: 404, .. }
    ));
}

#[tokio::test]
async fn metadata_reads_the_blob_byte_for_byte() {
    let (origin, log) = serve_as(200, Some("application/octet-stream"), "sealed bytes").await;
    let bytes = read_pulled_metadata(&origin, session(&origin), "home.test", "album-1", "asset-1")
        .await
        .expect("the blob is served");
    assert_eq!(bytes, b"sealed bytes");
    assert_eq!(
        seen(&log)[0].line,
        "GET /v1/federation/subscriptions/home.test/album-1/assets/asset-1/metadata"
    );
}

/// `capsule federation assets --json` prints exactly these keys: the document is a contract.
#[test]
fn the_assets_document_keeps_its_keys() {
    let json = serde_json::to_value(PulledAssets {
        peer: "home.test".to_owned(),
        album_id: "album-1".to_owned(),
        assets: vec![pulled_asset()],
    })
    .expect("serializes");
    assert_eq!(
        json,
        serde_json::json!({
            "peer": "home.test",
            "album_id": "album-1",
            "assets": [{
                "asset_id": "asset-1",
                "head": "ab",
                "timestamp": "2026-11-02T00:00:00Z",
                "author": "01937b7c-0000-7000-8000-0000000000b0",
                "size": 42,
            }],
        })
    );
}

#[tokio::test]
async fn unsubscribe_deletes_the_peer_and_album_path() {
    let (origin, log) = serve(204, "").await;
    delete_subscription(&origin, session(&origin), "home.test", "album-1")
        .await
        .expect("the removal is answered");
    assert_eq!(
        seen(&log)[0].line,
        "DELETE /v1/federation/subscriptions/home.test/album-1"
    );
}

#[tokio::test]
async fn empty_arguments_are_refused_before_any_request() {
    // Neither is read: the refusal comes first.
    let store = SessionStore::new(std::env::temp_dir().join("capsule-cli-federation-unread.json"));
    let remote = RemoteConfig::from_env();
    for (issuer, token) in [("", "token"), ("home.test", " ")] {
        assert!(matches!(
            subscribe(&remote, &store, issuer, token).await,
            Err(FederationCommandError::Invalid(_))
        ));
    }
    for (peer, album) in [("", "album"), ("home.test", "")] {
        assert!(matches!(
            unsubscribe(&remote, &store, peer, album).await,
            Err(FederationCommandError::Invalid(_))
        ));
        assert!(matches!(
            assets(&remote, &store, peer, album).await,
            Err(FederationCommandError::Invalid(_))
        ));
        assert!(matches!(
            metadata(&remote, &store, peer, album, "asset").await,
            Err(FederationCommandError::Invalid(_))
        ));
    }
    assert!(matches!(
        metadata(&remote, &store, "home.test", "album", " ").await,
        Err(FederationCommandError::Invalid(_))
    ));
}

/// `capsule federation subscribe --json` prints exactly these keys: the document is a contract.
#[test]
fn the_subscribe_document_keeps_its_keys() {
    let json = serde_json::to_value(Subscribed {
        created: true,
        subscription: entry(),
    })
    .expect("serializes");
    assert_eq!(
        json,
        serde_json::json!({
            "created": true,
            "subscription": {
                "peer": "home.test",
                "album_id": "album-1",
                "scope": "read",
                "jti": "jti-1",
                "issued_at": "2026-11-02T00:00:00Z",
                "expires_at": "2026-11-03T00:00:00Z",
                "subscribed_at": "2026-11-02T00:00:00Z",
                "live": true,
            },
        })
    );
}
