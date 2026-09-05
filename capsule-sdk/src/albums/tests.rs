//! Tests for the S-C25 album-provisioning client.
//!
//! Driven against the shared in-process mock server ([`crate::testmock`]), which replays the
//! real `POST /v1/albums` wire — including the `ApiError` JSON with its stable `error.*` code,
//! exactly as the server's `ProvisionResponses::write` produces it. The cross-module case
//! (this client against the real server, testcontainer Postgres and all) lives in
//! `capsule-api/upload`'s `cli_push_round_trip` suite.
//!
//! What is pinned here:
//!
//! | Case | Guarantee |
//! | --- | --- |
//! | `provision_sends_the_album_id_and_nothing_else` | no album name/description ever reaches the wire |
//! | `the_id_is_sent_in_canonical_hyphenated_form` | one spelling, so one album is never two rows |
//! | `a_fresh_album_reports_created` | the create path |
//! | `re_provisioning_the_same_id_succeeds` | **idempotency**: a second call is not an error |
//! | `an_id_bound_elsewhere_carries_the_distinct_code` | the refusal is switchable by code |
//! | `a_refusal_leaks_nothing_about_existence` | the 403 body is the same either way |
//! | `a_malformed_id_carries_the_invalid_id_code` | the 400 path |
//! | `an_echoed_mismatch_is_malformed` | the server cannot silently rebind another album |
//! | `the_request_is_authorized` | the bearer rides every call |
//! | `publish_roster_sends_the_signed_bytes_verbatim` | the roster on the wire is the one the device signed (`S-C51`) |
//! | `a_published_roster_reports_what_the_server_holds` | the success mapping, replay included |
//! | `a_stale_roster_carries_the_distinct_code` | the `409` is switchable by code |
//! | `a_roster_echo_mismatch_is_malformed` | the server cannot silently answer for another album |

use std::sync::{Arc, Mutex};

use capsule_i18n::error_codes;
use uuid::Uuid;

use super::*;
use crate::testmock::{MockRequest, MockResponse, MockServer};

/// A stable album id to provision.
fn album() -> Uuid {
    Uuid::parse_str("0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35").expect("a canonical uuid")
}

/// A client pointed at `server`, with the recorded requests it will observe.
fn client_for(server: &MockServer) -> AlbumClient {
    AlbumClient::new(AlbumTransport::with_static_token(
        reqwest::Client::new(),
        server.base_url(),
        StaticToken("test-token".into()),
    ))
}

/// Start a mock that records every request it serves and answers with `respond`.
async fn recording<F>(respond: F) -> (MockServer, Arc<Mutex<Vec<MockRequest>>>)
where
    F: Fn(&MockRequest) -> MockResponse + Send + Sync + 'static,
{
    let seen: Arc<Mutex<Vec<MockRequest>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let server = MockServer::start(move |req| {
        if let Ok(mut guard) = sink.lock() {
            guard.push(req.clone());
        }
        respond(req)
    })
    .await;
    (server, seen)
}

/// The canonical success body the server sends.
fn provisioned(id: Uuid, created: bool) -> MockResponse {
    MockResponse::new(if created { 201 } else { 200 }, "OK")
        .json_body(format!(r#"{{"album_id":"{id}","created":{created}}}"#))
}

/// **The privacy guarantee, asserted on the wire.** The body a real client sends carries the
/// album id and *nothing else* — no name, no description, no title of any kind. The server is
/// not entitled to album titles, and this is the client-side half of that.
#[tokio::test]
async fn provision_sends_the_album_id_and_nothing_else() {
    let id = album();
    let (server, seen) = recording(move |_| provisioned(id, true)).await;
    client_for(&server).provision(id).await.expect("provision");

    let requests = seen.lock().expect("recorded requests");
    let body: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("the body is JSON");
    let object = body.as_object().expect("the body is a JSON object");
    assert_eq!(
        object.keys().collect::<Vec<_>>(),
        vec!["album_id"],
        "provisioning sends exactly one field; a name must never reach the server"
    );
    assert_eq!(requests[0].method, "POST");
}

/// One spelling only: two devices deriving the same UUID must produce one row, so the client
/// always sends the canonical lowercase hyphenated form.
#[tokio::test]
async fn the_id_is_sent_in_canonical_hyphenated_form() {
    let id = album();
    let (server, seen) = recording(move |_| provisioned(id, true)).await;
    client_for(&server).provision(id).await.expect("provision");

    let requests = seen.lock().expect("recorded requests");
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).expect("JSON");
    let sent = body["album_id"].as_str().expect("album_id is a string");
    assert_eq!(sent, "0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35");
    assert_eq!(sent.len(), 36, "hyphenated, not simple");
    assert_eq!(sent, sent.to_lowercase(), "lowercase");
}

#[tokio::test]
async fn a_fresh_album_reports_created() {
    let id = album();
    let (server, _) = recording(move |_| provisioned(id, true)).await;
    let result = client_for(&server).provision(id).await.expect("provision");
    assert_eq!(
        result,
        ProvisionedAlbum {
            album_id: id,
            created: true
        }
    );
}

/// **Idempotency.** The client re-derives the same id on every device and after recovery, so
/// a second registration is a success that wrote nothing — never a conflict. This is what
/// makes `capsule push` re-runnable.
#[tokio::test]
async fn re_provisioning_the_same_id_succeeds() {
    let id = album();
    let (server, seen) = recording(move |_| provisioned(id, false)).await;
    let client = client_for(&server);

    let first = client.provision(id).await.expect("first provision");
    let second = client.provision(id).await.expect("second provision");
    assert_eq!(first, second, "provisioning twice is the same success");
    assert!(!second.created, "the second call wrote nothing");
    assert_eq!(seen.lock().expect("requests").len(), 2);
}

/// An id bound to a *different* account is refused with its own code, so a client can tell
/// "not yours" apart from a malformed request or a transport fault.
#[tokio::test]
async fn an_id_bound_elsewhere_carries_the_distinct_code() {
    let (server, _) = recording(|_| {
        MockResponse::api_error(
            403,
            "Forbidden",
            error_codes::ALBUM_NOT_AVAILABLE,
            "This album id is not available to this account",
        )
    })
    .await;

    let error = client_for(&server)
        .provision(album())
        .await
        .expect_err("an album bound elsewhere is refused");
    assert_eq!(error.error_code(), Some(error_codes::ALBUM_NOT_AVAILABLE));
    assert!(matches!(error, AlbumError::Status { status: 403, .. }));
    assert_ne!(
        error.error_code(),
        Some(error_codes::ALBUM_INVALID_ID),
        "the ownership refusal is a different code from a malformed id"
    );
}

/// The refusal must read identically whichever id is probed — the endpoint is not an
/// existence oracle over other accounts' derived album ids.
#[tokio::test]
async fn a_refusal_leaks_nothing_about_existence() {
    let (server, _) = recording(|_| {
        MockResponse::api_error(
            403,
            "Forbidden",
            error_codes::ALBUM_NOT_AVAILABLE,
            "This album id is not available to this account",
        )
    })
    .await;
    let client = client_for(&server);

    let taken = client
        .provision(album())
        .await
        .expect_err("an id held by someone else");
    let never_seen = client
        .provision(Uuid::now_v7())
        .await
        .expect_err("an id nobody holds");
    assert_eq!(
        format!("{taken}"),
        format!("{never_seen}"),
        "both refusals are indistinguishable to the caller"
    );
    assert_eq!(taken.error_code(), never_seen.error_code());
}

#[tokio::test]
async fn a_malformed_id_carries_the_invalid_id_code() {
    let (server, _) = recording(|_| {
        MockResponse::api_error(
            400,
            "Bad Request",
            error_codes::ALBUM_INVALID_ID,
            "album_id must be a canonical lowercase hyphenated UUID",
        )
    })
    .await;

    let error = client_for(&server)
        .provision(album())
        .await
        .expect_err("refused");
    assert_eq!(error.error_code(), Some(error_codes::ALBUM_INVALID_ID));
}

/// A server that answers about a *different* album has not provisioned what was asked for;
/// the client refuses to treat that as success rather than pushing into the wrong album.
#[tokio::test]
async fn an_echoed_mismatch_is_malformed() {
    let other = Uuid::now_v7();
    let (server, _) = recording(move |_| provisioned(other, true)).await;

    let error = client_for(&server)
        .provision(album())
        .await
        .expect_err("a mismatched echo is not a provisioned album");
    assert!(matches!(error, AlbumError::Malformed(_)), "got {error:?}");
}

#[tokio::test]
async fn the_request_is_authorized() {
    let id = album();
    let (server, seen) = recording(move |_| provisioned(id, true)).await;
    client_for(&server).provision(id).await.expect("provision");

    let requests = seen.lock().expect("recorded requests");
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer test-token"),
        "every provisioning call rides the caller's bearer"
    );
}

// ─── Roster publish (`S-C51`) ─────────────────────────────────────────────────

/// A roster for [`album`] at `version`, signed by a fresh device key.
fn signed_roster(version: u64) -> capsule_core::crypto::membership::SignedAlbumRoster {
    use capsule_core::crypto::keys::{AmkVersion, HybridSigningKey};
    use capsule_core::crypto::membership::{AlbumRoster, MemberRole, RosterMember};

    let roster = AlbumRoster {
        album_id: album(),
        roster_version: version,
        amk_epoch: AmkVersion(2),
        attested_by_user: Uuid::from_u128(0xA11CE),
        attested_by_device: Uuid::from_u128(0xD1),
        attested_at: "2026-09-02T00:00:00Z".to_owned(),
        members: vec![RosterMember {
            user_id: Uuid::from_u128(0xB0B),
            role: MemberRole::Writer,
        }],
    };
    capsule_core::crypto::membership::SignedAlbumRoster::sign(
        roster,
        &HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]),
    )
    .expect("a roster signs")
}

/// The canonical success body the server sends for a roster publish.
fn held(id: Uuid, version: u64, replayed: bool) -> MockResponse {
    MockResponse::new(200, "OK").json_body(format!(
        r#"{{"album_id":"{id}","roster_version":{version},"amk_epoch":2,"member_count":1,"replayed":{replayed}}}"#
    ))
}

/// **The bytes on the wire are the bytes the device signed.** The client base64-encodes the
/// canonical CBOR and changes nothing: a re-serialization would be a roster whose signature no
/// longer verifies, and the server decides a replay on these exact bytes.
#[tokio::test]
async fn publish_roster_sends_the_signed_bytes_verbatim() {
    use base64::Engine as _;

    let id = album();
    let signed = signed_roster(3);
    let (server, seen) = recording(move |_| held(id, 3, false)).await;
    // At the production layout — `{origin}/v1/albums` — so the path is the server's own.
    let client = AlbumClient::new(AlbumTransport::with_static_token(
        reqwest::Client::new(),
        format!("{}/v1/albums", server.base_url().trim_end_matches('/')),
        StaticToken("test-token".into()),
    ));
    client.publish_roster(&signed).await.expect("publish");

    let requests = seen.lock().expect("recorded requests");
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(
        requests[0].path,
        format!("/v1/albums/{}/roster", id.hyphenated())
    );
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).expect("JSON");
    let object = body.as_object().expect("a JSON object");
    assert_eq!(object.keys().collect::<Vec<_>>(), vec!["roster_cbor"]);
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(body["roster_cbor"].as_str().expect("a string"))
        .expect("standard base64");
    assert_eq!(
        bytes,
        capsule_core::cbor::to_canonical_vec(&signed).expect("encodes"),
        "the wire carries the canonical encoding of exactly what was signed"
    );
    assert!(
        requests[0].header("authorization").is_some(),
        "the bearer rides the roster publish too"
    );
}

#[tokio::test]
async fn a_published_roster_reports_what_the_server_holds() {
    let id = album();
    let (server, _) = recording(move |_| held(id, 3, true)).await;
    let result = client_for(&server)
        .publish_roster(&signed_roster(3))
        .await
        .expect("publish");
    assert_eq!(
        result,
        PublishedRoster {
            album_id: id,
            roster_version: 3,
            amk_epoch: 2,
            member_count: 1,
            replayed: true,
        }
    );
}

/// The `409` is the one refusal a client acts on differently — re-sync and republish above the
/// version the server names — so its code must come through.
#[tokio::test]
async fn a_stale_roster_carries_the_distinct_code() {
    let (server, _) = recording(|_| {
        MockResponse::new(409, "Conflict").json_body(
            r#"{"type":"about:blank","title":"Roster stale","status":409,"detail":"the server holds roster version 4, which this does not supersede","code":"error.album.roster_stale","current_version":4}"#.to_owned(),
        )
    })
    .await;
    let error = client_for(&server)
        .publish_roster(&signed_roster(3))
        .await
        .expect_err("a stale roster is refused");
    assert_eq!(error.error_code(), Some(error_codes::ALBUM_ROSTER_STALE));
    assert!(matches!(error, AlbumError::Status { status: 409, .. }));
}

/// A server answering for a different album than the one asked about is a malformed answer,
/// not a success with the wrong id in it.
#[tokio::test]
async fn a_roster_echo_mismatch_is_malformed() {
    let other = Uuid::parse_str("0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2eff").expect("a uuid");
    let (server, _) = recording(move |_| held(other, 3, false)).await;
    let error = client_for(&server)
        .publish_roster(&signed_roster(3))
        .await
        .expect_err("a mismatched echo is refused");
    assert!(matches!(error, AlbumError::Malformed(_)), "{error:?}");
}
