use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use capsule_i18n::error_codes;

use super::*;
use crate::auth::{AuthClient, PersistedSession};
use crate::testmock::{MockRequest, MockResponse, MockServer};

const ALBUM: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60";
const INTENT: &str = "019a0000-0000-7000-8000-00000000cafe";

/// The bytes a real client would hand over: opaque, and deliberately not valid UTF-8 so a
/// re-encoding anywhere on the path would show up.
fn signed_intent() -> Vec<u8> {
    let mut bytes = b"signed-upgrade-intent".to_vec();
    bytes.extend_from_slice(&[0x00, 0xff, 0xa5]);
    bytes
}

fn album() -> Uuid {
    Uuid::parse_str(ALBUM).expect("the literal is a uuid")
}

/// A session over `base` with a far-future token, so no refresh ever fires and the mock
/// needs no `/refresh` endpoint.
fn session_for(base: &str) -> Session {
    AuthClient::new(base)
        .expect("a base url")
        .resume(PersistedSession {
            access_token: "test-access".to_string().into(),
            refresh_token: "test-refresh".to_string().into(),
            access_expires_at_unix: jiff::Timestamp::now().as_second() + 3_600,
        })
        .expect("a session resumes from any pair")
}

fn phase_json() -> String {
    serde_json::json!({
        "album_id": ALBUM,
        "intent_id": INTENT,
        "to_protocol_version": "2030-01-01",
        "expires_at": "2030-01-01T00:05:00Z",
        "in_flight": 0,
    })
    .to_string()
}

/// An RFC 9457 problem, as `capsule-server`'s interceptor renders one.
fn problem(status: u16, reason: &str, code: &str, detail: &str) -> MockResponse {
    MockResponse::new(status, reason).json_body(
        serde_json::json!({
            "type": "about:blank",
            "title": reason,
            "status": status,
            "detail": detail,
            "code": code,
        })
        .to_string(),
    )
}

/// The signed bytes reach the documented path, in the documented media type, unchanged —
/// and the phase decodes into typed ids and a real instant.
#[tokio::test]
async fn a_proposal_sends_the_signed_bytes_verbatim_and_decodes_the_phase() {
    let intent = signed_intent();
    let expected = intent.clone();
    let seen = Arc::new(AtomicUsize::new(0));
    let counter = seen.clone();
    let server = MockServer::start(move |req: &MockRequest| {
        counter.fetch_add(1, Ordering::SeqCst);
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, format!("/v1/albums/{ALBUM}/upgrade"));
        assert_eq!(
            req.header("content-type"),
            Some(CBOR),
            "the intent must be sent in the media type it was signed in"
        );
        assert_eq!(
            req.body, expected,
            "a re-encoded intent no longer verifies under the proposer's DSK"
        );
        assert!(
            req.header("authorization")
                .is_some_and(|v| v.starts_with("Bearer ")),
            "the proposal is owner-scoped"
        );
        MockResponse::new(200, "OK").json_body(phase_json())
    })
    .await;

    let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
    let phase = client
        .begin(album(), &intent)
        .await
        .expect("the proposal is accepted");

    assert_eq!(seen.load(Ordering::SeqCst), 1, "exactly one request");
    assert_eq!(phase.album_id, album());
    assert_eq!(
        phase.intent_id,
        Some(Uuid::parse_str(INTENT).expect("a uuid"))
    );
    assert_eq!(phase.to_protocol_version.as_deref(), Some("2030-01-01"));
    assert_eq!(
        phase.expires_at,
        Some("2030-01-01T00:05:00Z".parse().expect("an instant"))
    );
    assert_eq!(phase.in_flight, 0);
}

/// A second ceremony is refused with the live `intent_id` and the code a client localizes
/// — the refusal an admin actually reads, so neither may be flattened into a status.
#[tokio::test]
async fn a_second_proposal_is_refused_with_the_live_ceremony() {
    let server = MockServer::start(move |_req: &MockRequest| {
        MockResponse::new(409, "Conflict").json_body(
            serde_json::json!({
                "type": "about:blank",
                "title": "Upgrade in flight",
                "status": 409,
                "detail": format!("album is already upgrading under {INTENT}"),
                "code": error_codes::ALBUM_UPGRADE_IN_FLIGHT,
                "intent_id": INTENT,
            })
            .to_string(),
        )
    })
    .await;

    let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
    let error = client
        .begin(album(), &signed_intent())
        .await
        .expect_err("only one ceremony may hold an album");
    let UpgradeError::InFlight { intent_id, .. } = &error else {
        panic!("expected an in-flight refusal, got {error:?}");
    };
    assert_eq!(intent_id.as_deref(), Some(INTENT));
    assert_eq!(
        error.error_code(),
        Some(error_codes::ALBUM_UPGRADE_IN_FLIGHT)
    );
}

/// Every refusal the operation declares maps to its own variant and keeps the server's
/// code. A status collapsed into the wrong variant would tell an admin to fix the wrong
/// thing — re-sign an intent that was fine, or wait out a ceremony that does not exist.
#[tokio::test]
async fn each_declared_refusal_keeps_its_own_identity() {
    for (status, reason, code) in [
        (400u16, "Bad Request", error_codes::ALBUM_UPGRADE_MALFORMED),
        (403, "Forbidden", error_codes::ALBUM_UPGRADE_PROPOSER),
        (404, "Not Found", error_codes::ALBUM_UPGRADE_NOT_FOUND),
        (
            415,
            "Unsupported Media Type",
            error_codes::ALBUM_UPGRADE_MALFORMED,
        ),
        (
            426,
            "Upgrade Required",
            error_codes::PROTOCOL_VERSION_UNSUPPORTED,
        ),
        (500, "Internal Server Error", error_codes::ALBUM_UNAVAILABLE),
    ] {
        let server =
            MockServer::start(move |_req: &MockRequest| problem(status, reason, code, "no")).await;
        let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
        let error = client
            .begin(album(), &signed_intent())
            .await
            .expect_err("the server refused");
        assert_eq!(error.error_code(), Some(code), "status {status}: {error:?}");
        let matched = match status {
            400 | 415 => matches!(error, UpgradeError::Malformed { .. }),
            403 => matches!(error, UpgradeError::NotProposer { .. }),
            404 => matches!(error, UpgradeError::NotFound { .. }),
            426 => matches!(error, UpgradeError::ProtocolUnsupported { .. }),
            500 => matches!(error, UpgradeError::Unavailable { .. }),
            _ => false,
        };
        assert!(matched, "status {status} took the wrong variant: {error:?}");
    }
}

/// The body-size backstop carries no problem body, so the client supplies the variant —
/// and **no code**. Minting one here would put words in the server's mouth, and every other
/// code this module reports is the server's own.
#[tokio::test]
async fn a_body_too_large_carries_no_invented_code() {
    let server =
        MockServer::start(move |_req: &MockRequest| MockResponse::new(413, "Payload Too Large"))
            .await;
    let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
    let error = client
        .begin(album(), &signed_intent())
        .await
        .expect_err("the server refused the size");
    assert!(
        matches!(error, UpgradeError::Malformed { code: None, .. }),
        "got {error:?}"
    );
    assert_eq!(
        error.error_code(),
        None,
        "a code the server never sent is not this client's to supply"
    );
}

/// A negative drain count is a malformed response, not a count wrapped into a huge one — a
/// proposer waiting for zero would otherwise wait forever.
#[tokio::test]
async fn a_negative_in_flight_count_is_a_malformed_response() {
    let server = MockServer::start(move |_req: &MockRequest| {
        MockResponse::new(200, "OK")
            .json_body(serde_json::json!({ "album_id": ALBUM, "in_flight": -1 }).to_string())
    })
    .await;
    let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
    let error = client
        .begin(album(), &signed_intent())
        .await
        .expect_err("a negative count is not a count");
    assert!(
        matches!(error, UpgradeError::MalformedResponse(_)),
        "got {error:?}"
    );
}

/// A refusal whose body is not a coded problem keeps its variant and carries no code: the
/// status still says what happened, and a code the body never held is not invented.
#[tokio::test]
async fn a_refusal_without_a_coded_problem_keeps_its_variant_and_no_code() {
    let server = MockServer::start(move |_req: &MockRequest| {
        MockResponse::new(409, "Conflict").json_body(r#"{"detail":"no code here"}"#.to_owned())
    })
    .await;
    let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
    let error = client
        .begin(album(), &signed_intent())
        .await
        .expect_err("the server refused");
    assert!(
        matches!(
            error,
            UpgradeError::InFlight {
                intent_id: None,
                code: None,
                ..
            }
        ),
        "got {error:?}"
    );
    assert_eq!(error.error_code(), None);
}

/// An undeclared status is surfaced as itself rather than guessed at.
#[tokio::test]
async fn an_undeclared_status_is_surfaced_as_unexpected() {
    let server =
        MockServer::start(move |_req: &MockRequest| MockResponse::new(418, "I'm a teapot")).await;
    let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
    let error = client
        .begin(album(), &signed_intent())
        .await
        .expect_err("418 is not in the contract");
    assert!(
        matches!(error, UpgradeError::Unexpected { status: 418 }),
        "got {error:?}"
    );
    assert_eq!(error.error_code(), None);
}

/// A phase document whose deadline is not an instant is a malformed *response*, not a
/// silent `None` — a dropped deadline would make a client think the ceremony never expires.
#[tokio::test]
async fn an_unparseable_deadline_is_a_malformed_response() {
    let server = MockServer::start(move |_req: &MockRequest| {
        MockResponse::new(200, "OK").json_body(
            serde_json::json!({
                "album_id": ALBUM,
                "intent_id": INTENT,
                "expires_at": "next tuesday",
                "in_flight": 0,
            })
            .to_string(),
        )
    })
    .await;
    let client = UpgradeClient::new(session_for(&server.base_url()), &server.base_url());
    let error = client
        .begin(album(), &signed_intent())
        .await
        .expect_err("a deadline that is not an instant is not a deadline");
    assert!(
        matches!(error, UpgradeError::MalformedResponse(_)),
        "got {error:?}"
    );
}
