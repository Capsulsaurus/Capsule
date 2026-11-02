use std::sync::Mutex;

use super::*;
use crate::federation::pull::signer::Ed25519RequestSigner;
use crate::federation::scheme::{REQUEST_SIGNATURE_HEADER, RequestClaim, RequestSignature};
use crate::store::SystemClock;

/// What the scripted peer answers: a status, headers and a body.
#[derive(Debug, Clone)]
struct Canned {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

impl Canned {
    fn json(status: u16, body: &serde_json::Value) -> Self {
        Self {
            status,
            content_type: if status < 300 {
                "application/json"
            } else {
                "application/problem+json"
            },
            body: body.to_string().into_bytes(),
        }
    }
}

/// A peer that answers every request with one canned response and records what it was asked.
#[derive(Debug)]
struct Scripted {
    answer: Canned,
    seen: Mutex<Vec<reqwest::Request>>,
}

impl Scripted {
    fn new(answer: Canned) -> Arc<Self> {
        Arc::new(Self {
            answer,
            seen: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<(String, String, Option<String>, Option<String>)> {
        self.seen
            .lock()
            .expect("unpoisoned")
            .iter()
            .map(|request| {
                let header = |name: &str| {
                    request
                        .headers()
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned)
                };
                (
                    request.method().to_string(),
                    request.url().to_string(),
                    header("authorization"),
                    header(REQUEST_SIGNATURE_HEADER),
                )
            })
            .collect()
    }
}

impl rest::HttpBackend for Scripted {
    fn execute(&self, request: reqwest::Request) -> rest::ExecuteFuture<'_> {
        self.seen.lock().expect("unpoisoned").push(request);
        let answer = self.answer.clone();
        Box::pin(async move {
            let mut response = http::Response::new(answer.body);
            *response.status_mut() =
                reqwest::StatusCode::from_u16(answer.status).expect("a status");
            response.headers_mut().insert(
                reqwest::header::CONTENT_TYPE,
                reqwest::header::HeaderValue::from_static(answer.content_type),
            );
            Ok(reqwest::Response::from(response))
        })
    }
}

fn signer() -> Arc<Ed25519RequestSigner> {
    let random = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::Ed25519KeyPair::generate_pkcs8(&random).expect("a key");
    Arc::new(Ed25519RequestSigner::from_pkcs8(pkcs8.as_ref()).expect("it reads"))
}

fn client_over(peer: &Arc<Scripted>, signer: Arc<Ed25519RequestSigner>) -> PeerClient {
    let origin = origin_of(&PeerId::new("home.test")).expect("an origin");
    PeerClient::new(peer.clone(), origin, signer, Arc::new(SystemClock))
}

const TOKEN: &str = "a.capability.token";

fn album() -> AlbumId {
    AlbumId::new("0190a5b0-0000-7000-8000-000000000001")
}

fn page() -> serde_json::Value {
    serde_json::json!({ "entries": [], "next_cursor": "c1", "has_more": false })
}

fn problem(status: u16, code: &str) -> serde_json::Value {
    serde_json::json!({ "type": "about:blank", "title": "refused", "status": status, "code": code })
}

#[test]
fn the_origin_is_https_on_the_peer() {
    assert_eq!(
        origin_of(&PeerId::new("Home.Test."))
            .expect("an origin")
            .as_str(),
        "https://home.test/"
    );
}

#[tokio::test]
async fn a_page_is_fetched_signed_and_presented() {
    let peer = Scripted::new(Canned::json(200, &page()));
    let signer = signer();
    let client = client_over(&peer, signer.clone());

    let fetched = client
        .sync_page(TOKEN, &album(), Some("c0"), 50, 1024)
        .await
        .expect("a page");
    assert_eq!(fetched.next_cursor, "c1");

    let requests = peer.requests();
    assert_eq!(requests.len(), 1);
    let (method, url, authorization, signature) = &requests[0];
    assert_eq!(method, "GET");
    let url = Url::parse(url).expect("a URL");
    assert_eq!(url.host_str(), Some("home.test"));
    assert_eq!(url.path(), "/v1/sync");
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(
        query.get("album_id").map(String::as_str),
        Some(album().as_str())
    );
    assert_eq!(query.get("cursor").map(String::as_str), Some("c0"));
    assert_eq!(query.get("page_size").map(String::as_str), Some("50"));
    assert_eq!(authorization.as_deref(), Some("Bearer a.capability.token"));

    let parsed = RequestSignature::parse(signature.as_deref().expect("signed")).expect("parses");
    let claim = RequestClaim::new("GET", "/v1/sync", TOKEN, parsed.signed_at, &parsed.nonce);
    parsed
        .verify(&claim, &signer.public_key())
        .expect("the issuer's verifier admits it");
}

#[tokio::test]
async fn an_empty_cursor_is_sent_as_none() {
    let peer = Scripted::new(Canned::json(200, &page()));
    let client = client_over(&peer, signer());
    client
        .sync_page(TOKEN, &album(), Some(""), 10, 1024)
        .await
        .expect("a page");
    let url = Url::parse(&peer.requests()[0].1).expect("a URL");
    assert!(!url.query_pairs().any(|(key, _)| key == "cursor"));
}

#[tokio::test]
async fn a_body_past_the_bound_is_malformed() {
    let peer = Scripted::new(Canned::json(200, &page()));
    let client = client_over(&peer, signer());
    let error = client
        .sync_page(TOKEN, &album(), None, 10, 8)
        .await
        .expect_err("over the bound");
    assert!(
        matches!(&error, RemoteError::Malformed(detail) if detail.contains("larger than the 8 bytes")),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_body_that_does_not_decode_is_malformed() {
    let peer = Scripted::new(Canned::json(200, &serde_json::json!({ "entries": "no" })));
    let client = client_over(&peer, signer());
    let error = client
        .sync_page(TOKEN, &album(), None, 10, 1024)
        .await
        .expect_err("undecodable");
    assert!(matches!(error, RemoteError::Malformed(_)), "{error:?}");
}

#[tokio::test]
async fn a_redirect_is_malformed_and_never_followed() {
    let peer = Scripted::new(Canned {
        status: 302,
        content_type: "text/plain",
        body: Vec::new(),
    });
    let client = client_over(&peer, signer());
    let error = client
        .sync_page(TOKEN, &album(), None, 10, 1024)
        .await
        .expect_err("a redirect");
    assert!(matches!(error, RemoteError::Malformed(_)), "{error:?}");
    assert_eq!(peer.requests().len(), 1, "one request, nothing followed");
}

#[tokio::test]
async fn a_declared_refusal_carries_its_code() {
    let peer = Scripted::new(Canned::json(
        403,
        &problem(403, "error.federation.capability_revoked"),
    ));
    let client = client_over(&peer, signer());
    let error = client
        .sync_page(TOKEN, &album(), None, 10, 4096)
        .await
        .expect_err("refused");
    assert_eq!(
        error,
        RemoteError::Refused {
            status: 403,
            code: Some("error.federation.capability_revoked".into()),
        }
    );
}

#[tokio::test]
async fn a_server_failure_is_unreachable() {
    for status in [500, 503] {
        let peer = Scripted::new(Canned::json(status, &problem(status, "error.internal")));
        let client = client_over(&peer, signer());
        let error = client
            .sync_page(TOKEN, &album(), None, 10, 4096)
            .await
            .expect_err("failed");
        assert!(
            matches!(error, RemoteError::Unreachable(_)),
            "{status}: {error:?}"
        );
    }
}

#[tokio::test]
async fn a_blob_is_fetched_by_its_signed_path() {
    let hash = "ab".repeat(32);
    let peer = Scripted::new(Canned {
        status: 200,
        content_type: "application/octet-stream",
        body: b"ciphertext".to_vec(),
    });
    let signer = signer();
    let client = client_over(&peer, signer.clone());
    let bytes = client.blob(TOKEN, &hash, 64).await.expect("bytes");
    assert_eq!(bytes, b"ciphertext");

    let (method, url, _, signature) = &peer.requests()[0];
    assert_eq!(method, "GET");
    assert_eq!(
        Url::parse(url).expect("a URL").path(),
        format!("/v1/blob/{hash}")
    );
    let parsed = RequestSignature::parse(signature.as_deref().expect("signed")).expect("parses");
    let claim = RequestClaim::new(
        "GET",
        &format!("/v1/blob/{hash}"),
        TOKEN,
        parsed.signed_at,
        &parsed.nonce,
    );
    parsed
        .verify(&claim, &signer.public_key())
        .expect("verifies");
}

#[tokio::test]
async fn a_blob_address_that_is_not_hex_never_reaches_the_wire() {
    let peer = Scripted::new(Canned::json(200, &page()));
    let client = client_over(&peer, signer());
    for hash in [
        "../../etc/passwd",
        &"AB".repeat(32),
        "ab",
        &format!("{}?x=1", "a".repeat(60)),
    ] {
        let error = client.blob(TOKEN, hash, 64).await.expect_err("refused");
        assert!(
            matches!(error, RemoteError::Malformed(_)),
            "{hash}: {error:?}"
        );
    }
    assert!(peer.requests().is_empty(), "nothing was sent");
}

#[tokio::test]
async fn a_blob_past_the_bound_is_malformed() {
    let peer = Scripted::new(Canned {
        status: 200,
        content_type: "application/octet-stream",
        body: vec![0; 65],
    });
    let client = client_over(&peer, signer());
    let error = client
        .blob(TOKEN, &"ab".repeat(32), 64)
        .await
        .expect_err("over");
    assert!(matches!(error, RemoteError::Malformed(_)), "{error:?}");
}

#[tokio::test]
async fn a_refresh_is_signed_over_its_own_path() {
    let peer = Scripted::new(Canned::json(
        200,
        &serde_json::json!({
            "token": "successor",
            "jti": "j2",
            "expires_at": "2026-11-03T00:00:00Z",
            "not_after": "2026-12-01T00:00:00Z",
            "replayed": false,
        }),
    ));
    let signer = signer();
    let client = client_over(&peer, signer.clone());
    let refreshed = client.refresh(TOKEN, 4096).await.expect("refreshed");
    assert_eq!(refreshed.token, "successor");

    let (method, url, authorization, signature) = &peer.requests()[0];
    assert_eq!(method, "POST");
    assert_eq!(
        Url::parse(url).expect("a URL").path(),
        "/v1/federation/capabilities/refresh"
    );
    assert_eq!(authorization.as_deref(), Some("Bearer a.capability.token"));
    let parsed = RequestSignature::parse(signature.as_deref().expect("signed")).expect("parses");
    let claim = RequestClaim::new(
        "POST",
        "/v1/federation/capabilities/refresh",
        TOKEN,
        parsed.signed_at,
        &parsed.nonce,
    );
    parsed
        .verify(&claim, &signer.public_key())
        .expect("verifies");
}

#[tokio::test]
async fn a_request_off_the_origin_is_never_executed() {
    let peer = Scripted::new(Canned::json(200, &page()));
    let transport = PeerTransport::new(
        peer.clone(),
        origin_of(&PeerId::new("home.test")).expect("an origin"),
    );
    transport.arm(1024);
    for url in [
        "https://elsewhere.test/v1/sync",
        "http://home.test/v1/sync",
        "https://home.test:8443/v1/sync",
    ] {
        let request = reqwest::Request::new(reqwest::Method::GET, Url::parse(url).expect("a URL"));
        let response = rest::HttpBackend::execute(&transport, request)
            .await
            .expect("answered locally");
        assert_eq!(response.status().as_u16(), VIOLATION_STATUS);
        assert_eq!(
            transport.take_violation(),
            Some(Violation::OffOrigin {
                url: url.to_owned()
            })
        );
    }
    assert!(peer.requests().is_empty());
}

#[test]
fn the_egress_client_builds() {
    egress_client().expect("a rustls client");
}
