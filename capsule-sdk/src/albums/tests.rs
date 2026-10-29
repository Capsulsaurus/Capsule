//! Tests for the album-provisioning client.
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
//! | `publish_roster_sends_the_signed_bytes_verbatim` | the roster on the wire is the one the device signed |
//! | `a_published_roster_reports_what_the_server_holds` | the success mapping, replay included |
//! | `a_stale_roster_carries_the_distinct_code` | the `409` is switchable by code |
//! | `a_roster_echo_mismatch_is_malformed` | the server cannot silently answer for another album |
//! | `a_version_leap_carries_the_held_version_too` | the `400` too-far-ahead refusal is switchable and carries `current_version` |
//! | `the_widest_version_the_server_can_name_still_decodes` | the recovery hint survives at the top of the server's range, where an unbounded counter would not |
//! | `the_roster_publish_goes_through_the_generated_operation` | the path, method and body come from the committed contract, not from a hand-written request |
//! | `a_lifecycle_op_is_posted_to_its_albums_ops_route` | the op reaches `POST /v1/albums/{album}/ops` verbatim, and the answer maps, replay included |
//! | `a_closed_retention_window_is_its_own_permanent_kind` | `409 error.upload.retention_elapsed` is `retention_elapsed`, permanent, sent once, and carries the server's `retention_until` |
//! | `a_stale_chain_carries_the_head_to_rebase_onto` | the other `409` is `stale_revival` and carries `chain_head`, with no `retention_until` |
//! | `a_transient_refusal_is_not_permanent` | `500` and `403` are not judgements of the op, so they are not permanent |
//! | `a_401_under_a_session_is_refreshed_and_the_op_replayed_once` | a mid-flight `401` is refreshed once and replayed once with the rotated bearer |
//! | `an_op_naming_no_album_is_refused_before_the_wire` | nothing is sent for an envelope with no album |

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

// ─── Roster publish ─────────────────────────────────────────────────

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
    let expected = capsule_core::cbor::to_canonical_vec(&signed).expect("encodes");
    assert_eq!(
        bytes, expected,
        "the wire carries the canonical encoding of exactly what was signed"
    );
    assert_eq!(
        capsule_core::cbor::canonicalize(&bytes).expect("decodes"),
        bytes,
        "and those bytes are canonical, which is the form the server stores and replays on"
    );
    assert_eq!(
        body["roster_cbor"].as_str().expect("a string"),
        base64::engine::general_purpose::STANDARD.encode(&expected),
        "standard base64 with padding, the alphabet the server decodes"
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
    assert!(
        matches!(
            error,
            AlbumError::Status {
                status: 409,
                current_version: Some(4),
                ..
            }
        ),
        "the held version rides the refusal, so the caller can republish above it: {error:?}"
    );
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

/// The other version refusal: a roster so far *ahead* of the held one that the server would be
/// latched if it took it. A `400` rather than the `409`, and it carries the held version for the
/// same reason — the caller re-signs one above it.
#[tokio::test]
async fn a_version_leap_carries_the_held_version_too() {
    let (server, _) = recording(|_| {
        MockResponse::new(400, "Bad Request").json_body(
            r#"{"type":"about:blank","title":"Roster version leap","status":400,"detail":"roster version 9999 is past 17","code":"error.album.roster_version_leap","declared":9999,"current_version":1,"max_version":17}"#.to_owned(),
        )
    })
    .await;
    let error = client_for(&server)
        .publish_roster(&signed_roster(9999))
        .await
        .expect_err("a leap is refused");
    assert_eq!(
        error.error_code(),
        Some(error_codes::ALBUM_ROSTER_VERSION_LEAP)
    );
    assert!(
        matches!(
            error,
            AlbumError::Status {
                status: 400,
                current_version: Some(1),
                ..
            }
        ),
        "the held version rides this refusal too: {error:?}"
    );
}

/// **The wire shape is the contract's, not this module's.** `publish_roster` is orchestration
/// over the generated `publish_album_roster` operation, so the method, the path and the required
/// protocol header come from `capsule-server/openapi.json` rather than from a hand-written
/// request this module could drift.
#[tokio::test]
async fn the_roster_publish_goes_through_the_generated_operation() {
    let id = album();
    let (server, seen) = recording(move |_| held(id, 3, false)).await;
    let client = AlbumClient::new(AlbumTransport::with_static_token(
        reqwest::Client::new(),
        format!("{}/v1/albums", server.base_url().trim_end_matches('/')),
        StaticToken("test-token".into()),
    ));
    client
        .publish_roster(&signed_roster(3))
        .await
        .expect("publish");

    let requests = seen.lock().expect("recorded requests");
    assert_eq!(requests[0].method, "PUT");
    assert_eq!(
        requests[0].path,
        format!("/v1/albums/{}/roster", id.hyphenated())
    );
    assert_eq!(
        requests[0].header("x-capsule-protocol"),
        Some(capsule_core::crypto::primitives::PROTOCOL_VERSION),
        "the generated operation carries the protocol date the document declares required"
    );
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer test-token")
    );
}

/// **The refusal has to survive at the top of the server's range.**
///
/// spargen lowers every integer in the contract as `i64` and emits no `u64` at all, so a
/// counter above `i64::MAX` would not be an API error at all — the generated client would fail
/// to *decode* the body, the typed error would never be built, and `code` and `current_version`
/// would be replaced by an undifferentiated transport failure. The server bounds every counter
/// it can emit at `MAX_ROSTER_VERSION` (`i64::MAX`) precisely so this holds; the case pins the
/// boundary rather than a comfortable value in the middle of the range, and the declared
/// version — the one number a caller controls and the server therefore cannot bound — is not an
/// extension member at all.
#[tokio::test]
async fn the_widest_version_the_server_can_name_still_decodes() {
    let ceiling = i64::MAX as u64;
    let body = format!(
        r#"{{"type":"about:blank","title":"Roster version leap","status":400,"detail":"roster version 18446744073709551615 is past {ceiling}, the highest this album will accept while it holds version {ceiling}","code":"error.album.roster_version_leap","current_version":{ceiling},"max_version":{ceiling}}}"#
    );
    let (server, _) =
        recording(move |_| MockResponse::new(400, "Bad Request").json_body(body.clone())).await;

    let error = client_for(&server)
        .publish_roster(&signed_roster(u64::MAX))
        .await
        .expect_err("a leap is refused");

    assert_eq!(
        error.error_code(),
        Some(error_codes::ALBUM_ROSTER_VERSION_LEAP),
        "the structured code survives at the boundary: {error:?}"
    );
    assert!(
        matches!(
            error,
            AlbumError::Status {
                status: 400,
                current_version: Some(held),
                ..
            } if held == ceiling
        ),
        "and so does the recovery hint: {error:?}"
    );
    assert!(
        !matches!(error, AlbumError::Transport(_)),
        "a decode failure would have collapsed this into a transport error"
    );
}

// ─── The protocol window (issue #674) ───────────────────────────────────────

/// A roster publish the gate answers `426` is the SDK's one upgrade refusal, not a status.
#[tokio::test]
async fn a_roster_426_is_the_typed_upgrade_refusal() {
    let (server, _) = recording(|_| {
        MockResponse::new(426, "Upgrade Required")
            .header("X-Capsule-Protocol-Min", "2099-01-01")
            .header("X-Capsule-Protocol-Max", "2099-12-31")
            .json_body(
                r#"{"type":"about:blank","status":426,"detail":"too old","code":"error.protocol.version_unsupported"}"#
                    .to_owned(),
            )
    })
    .await;
    let error = client_for(&server)
        .publish_roster(&signed_roster(3))
        .await
        .expect_err("refused");
    let AlbumError::UpgradeRequired(refusal) = &error else {
        panic!("expected UpgradeRequired, got {error:?}");
    };
    assert_eq!(
        refusal.reason,
        crate::net::negotiation::UpgradeReason::ProtocolBelowWindow
    );
    assert_eq!(
        error.error_code(),
        Some(error_codes::PROTOCOL_VERSION_UNSUPPORTED)
    );
}

/// Once a response latched a build floor above this build, neither album write leaves: the
/// hand-written provision nor the generated roster publish.
#[tokio::test]
async fn album_writes_under_a_latched_floor_never_leave() {
    let (server, seen) = recording(|_| {
        MockResponse::new(404, "Not Found")
            .header("X-Capsule-Protocol-Min", "2026-01-01")
            .header("X-Capsule-Protocol-Max", "2099-12-31")
            .header("X-Capsule-Min-Client-Build", "999.0.0")
    })
    .await;
    // Any response from the origin latches its window; a read is the realistic first one.
    reqwest::Client::new()
        .get(format!("{}/v1/anything", server.base_url()))
        .send()
        .await
        .map(|response| {
            crate::net::negotiation::observe(response.url(), response.headers());
        })
        .expect("the read is answered");

    let client = client_for(&server);
    let provision = client.provision(album()).await.expect_err("refused");
    assert!(
        matches!(provision, AlbumError::UpgradeRequired(_)),
        "{provision:?}"
    );
    let publish = client
        .publish_roster(&signed_roster(3))
        .await
        .expect_err("refused");
    assert!(
        matches!(publish, AlbumError::UpgradeRequired(_)),
        "{publish:?}"
    );
    assert_eq!(
        seen.lock().unwrap().len(),
        1,
        "only the read reached the server"
    );
}

// ─── Lifecycle ops ────────────────────────────────────────────────────────────

/// A client whose transport is the album endpoint of `server`, as production builds it.
fn ops_client_for(server: &MockServer) -> AlbumClient {
    AlbumClient::new(AlbumTransport::with_static_token(
        reqwest::Client::new(),
        format!("{}/v1/albums", server.base_url().trim_end_matches('/')),
        StaticToken("test-token".into()),
    ))
}

/// A `trash-restore` op for an asset of [`album`]. The bytes are placeholders: the server does
/// not parse the manifest, and neither does this client.
fn restore_op() -> crate::rest::types::OpRequest {
    crate::rest::types::OpRequest {
        manifest_envelope: crate::rest::types::ManifestEnvelope {
            crypto_suite_id: 1,
            protocol_version: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_owned(),
            album_id: Some(album().hyphenated().to_string()),
            file_id: "0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e99".to_owned(),
            amk_version: 1,
            ciphertext_hash: "00".repeat(32),
            plaintext_size: 3,
            chunk_size: 65536,
            key_mode: "derived".to_owned(),
            metadata_blob_hash: None,
            original_blob_hash: None,
            derivative_role: None,
            derivative_format: None,
            created_by_user: "user".to_owned(),
            created_by_device: "0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e00".to_owned(),
            client_version: "test".to_owned(),
            timestamp: "2026-10-29T00:00:00Z".to_owned(),
            action: "trash-restore".to_owned(),
            prior_provenance_hash: Some("11".repeat(32)),
            retention_until: None,
        },
        manifest_cbor: "AAEC".to_owned(),
        metadata_blob: None,
    }
}

/// The success body the server sends for [`restore_op`].
fn applied(replayed: bool) -> MockResponse {
    MockResponse::new(200, "OK").json_body(format!(
        r#"{{"asset_id":"0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e99","sync_seq":42,"action":"trash-restore","replayed":{replayed}}}"#
    ))
}

#[tokio::test]
async fn a_lifecycle_op_is_posted_to_its_albums_ops_route() {
    let (server, seen) = recording(|_| applied(true)).await;
    let op = restore_op();
    let result = ops_client_for(&server)
        .apply_op(&op)
        .await
        .expect("applied");
    assert_eq!(
        result,
        AppliedOp {
            asset_id: "0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e99".to_owned(),
            action: "trash-restore".to_owned(),
            sync_seq: 42,
            replayed: true,
        }
    );

    let requests = seen.lock().expect("recorded requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(
        requests[0].path,
        format!("/v1/albums/{}/ops", album().hyphenated())
    );
    assert_eq!(
        requests[0].header("authorization"),
        Some("Bearer test-token")
    );
    let sent: serde_json::Value =
        serde_json::from_slice(&requests[0].body).expect("the body is JSON");
    assert_eq!(
        sent,
        serde_json::to_value(&op).expect("the op serializes"),
        "the op reaches the wire as it was signed"
    );
}

/// **The refusal #871 added.** A restore after the asset's signed retention floor is final: its
/// own kind, the server's code, permanent — and sent exactly once.
#[tokio::test]
async fn a_closed_retention_window_is_its_own_permanent_kind() {
    let (server, seen) = recording(|_| {
        MockResponse::new(409, "Conflict").json_body(
            r#"{"type":"about:blank","title":"Retention elapsed","status":409,"detail":"this asset's retention window has closed, so it can no longer be restored","code":"error.upload.retention_elapsed","retention_until":"2026-09-01T00:00:00Z"}"#.to_owned(),
        )
    })
    .await;
    let error = ops_client_for(&server)
        .apply_op(&restore_op())
        .await
        .expect_err("refused");
    assert_eq!(error.kind(), "retention_elapsed");
    assert_eq!(
        error.error_code(),
        Some(error_codes::UPLOAD_RETENTION_ELAPSED)
    );
    assert!(error.is_permanent(), "no resend reopens the window");
    assert!(
        matches!(
            &error,
            LifecycleOpError::Refused { retention_until: Some(until), chain_head: None, .. }
                if until == "2026-09-01T00:00:00Z"
        ),
        "the floor the server judged survives the generated decode: {error:?}"
    );
    assert_eq!(seen.lock().expect("requests").len(), 1, "not retried");
}

#[tokio::test]
async fn a_stale_chain_carries_the_head_to_rebase_onto() {
    let (server, _) = recording(|_| {
        MockResponse::new(409, "Conflict").json_body(
            r#"{"type":"about:blank","title":"Stale revival","status":409,"detail":"this manifest does not follow the asset's current state","code":"error.upload.stale_revival","chain_head":"abcd"}"#.to_owned(),
        )
    })
    .await;
    let error = ops_client_for(&server)
        .apply_op(&restore_op())
        .await
        .expect_err("refused");
    assert_eq!(error.kind(), "stale_revival");
    assert!(error.is_permanent());
    assert!(
        matches!(
            &error,
            LifecycleOpError::Refused { status: 409, chain_head: Some(head), retention_until: None, .. }
                if head == "abcd"
        ),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_transient_refusal_is_not_permanent() {
    for (status, reason, code, kind) in [
        (
            500,
            "Internal Server Error",
            "error.internal",
            "unavailable",
        ),
        (
            403,
            "Forbidden",
            "error.upload.album_access_denied",
            "forbidden",
        ),
    ] {
        let (server, _) = recording(move |_| {
            MockResponse::new(status, reason).json_body(format!(
                r#"{{"type":"about:blank","status":{status},"detail":"no","code":"{code}"}}"#
            ))
        })
        .await;
        let error = ops_client_for(&server)
            .apply_op(&restore_op())
            .await
            .expect_err("refused");
        assert_eq!(error.kind(), kind, "{status}");
        assert_eq!(error.error_code(), Some(code), "{status}");
        assert!(!error.is_permanent(), "{status}");
    }
}

/// **The session path.** A token the server refuses mid-flight is refreshed once and the op
/// replayed once with the rotated bearer; the replay's answer is the one the caller sees.
#[tokio::test]
async fn a_401_under_a_session_is_refreshed_and_the_op_replayed_once() {
    let ops = Arc::new(Mutex::new(0_usize));
    let counter = ops.clone();
    let (server, seen) = recording(move |req| {
        if req.path == "/v1/auth/refresh" {
            return MockResponse::new(200, "OK").json_body(
                serde_json::json!({
                    "access_token": "access-2",
                    "refresh_token": "refresh-2",
                    "token_type": "Bearer",
                    "expires_by": jiff::Timestamp::now().as_second() + 3_600,
                })
                .to_string(),
            );
        }
        let mut calls = counter.lock().expect("op counter");
        *calls += 1;
        if *calls == 1 {
            MockResponse::new(401, "Unauthorized").json_body(format!(
                r#"{{"type":"about:blank","status":401,"detail":"expired","code":"{}"}}"#,
                error_codes::REQUEST_UNAUTHENTICATED
            ))
        } else {
            applied(true)
        }
    })
    .await;
    let base = server.base_url();
    let origin = base.trim_end_matches('/');
    let session = crate::auth::AuthClient::new(&format!("{origin}/v1/auth"))
        .expect("auth client")
        .resume(crate::auth::PersistedSession {
            access_token: "access-1".to_owned().into(),
            refresh_token: "refresh-1".to_owned().into(),
            // Far from expiry, so only the reactive `401` path can refresh.
            access_expires_at_unix: jiff::Timestamp::now().as_second() + 3_600,
        })
        .expect("session");
    let client = AlbumClient::new(AlbumTransport::with_session(
        session,
        format!("{origin}/v1/albums"),
    ));

    let result = client.apply_op(&restore_op()).await.expect("replayed");
    assert_eq!(result.sync_seq, 42);
    assert!(result.replayed);

    let requests = seen.lock().expect("recorded requests");
    let paths: Vec<&str> = requests.iter().map(|r| r.path.as_str()).collect();
    let op_path = format!("/v1/albums/{}/ops", album().hyphenated());
    assert_eq!(
        paths,
        vec![op_path.as_str(), "/v1/auth/refresh", op_path.as_str()],
        "one attempt, one refresh, one replay"
    );
    assert_eq!(requests[0].header("authorization"), Some("Bearer access-1"));
    assert_eq!(
        requests[2].header("authorization"),
        Some("Bearer access-2"),
        "the replay carries the rotated token"
    );
}

#[tokio::test]
async fn an_op_naming_no_album_is_refused_before_the_wire() {
    let (server, seen) = recording(|_| applied(false)).await;
    let mut op = restore_op();
    op.manifest_envelope.album_id = None;
    let error = ops_client_for(&server)
        .apply_op(&op)
        .await
        .expect_err("refused");
    assert_eq!(error.kind(), "malformed");
    assert!(seen.lock().expect("requests").is_empty());
}
