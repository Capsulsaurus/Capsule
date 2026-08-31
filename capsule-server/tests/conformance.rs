//! Conformance: every response the server sends is one its description predicts, and every
//! response the description predicts is one some test has actually produced.
//!
//! These two assertions are opposites and both are needed. Together they are the executable
//! form of the failure this rebuild exists to remove: the Salvo surface had **thirteen response
//! variants that rendered a status the published schema never declared** (slice `S-C28`) —
//! login could answer `423` and `429`, and `capsule-sdk/openapi.json` mentioned neither, so the
//! generated client had no case to map them to. That gap was invisible because it lived between
//! two hand-written impls, one rendering and one registering, with nothing comparing them.
//!
//! Here there is one declaration. `assert_conformance` fails if a response escapes that the
//! document did not predict; `assert_declared_responses_covered` fails if the document promises
//! a response no test has produced, which is the direction line coverage cannot see.
//!
//! Run with `cargo nextest run -p capsule-server`.

mod support;

use capsule_server::blob::BlobStore;
use capsule_server::index::AssetIndex;
use capsule_server::routes::auth::TokenResponse;
use capsule_server::routes::version::VersionResponse;
use kynos::http::StatusCode;
use kynos::test::TestClient;
use serde_json::json;
use support::{EMAIL, Fixture, PASSWORD, PROTOCOL_VERSION, checksum, create_request, payload};

/// A `Content-Length` no operation will accept.
fn oversized() -> u64 {
    capsule_server::limits::MAX_REQUEST_BODY_BYTES + 1
}

/// `GET /v1/version` answers the shape `capsule status` reads.
///
/// The literal `capsule-api` is asserted, not derived from the crate name: this crate is
/// `capsule-server` only until the Salvo tree retires and the rename happens, and a client
/// probing for reachability must not see the server's identity change underneath it because an
/// internal directory moved.
#[tokio::test]
async fn version_reports_the_server_identity() {
    let client =
        TestClient::new(capsule_server::service(Fixture::working_app()).expect("router builds"));

    let body: VersionResponse = client
        .get("/v1/version")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();

    assert_eq!(
        body.name, "capsule-api",
        "wire identity is a client contract"
    );
    assert_eq!(
        body.version,
        env!("CARGO_PKG_VERSION"),
        "version tracks the crate"
    );

    // Nothing was sent that the description did not predict.
    client.assert_conformance();
}

/// Everything the description promises has actually been produced by a test.
///
/// This is the assertion that would have caught `S-C28` at the moment it was introduced, and it
/// is the reason the auth port deleted a status rather than documenting it: the Salvo login can
/// answer `429`, the counter port that backs rate limiting does not exist (`S-C29` excluded
/// counters; `S-C32` owns them), and a `429` declared here would fail this test on the first
/// run. A gap the suite reports beats a promise the server cannot keep.
///
/// It walks **every** operation in one client, because the recorder is per-client and a status
/// produced against a second server would not count. So the whole surface — eighteen responses
/// across four operations at the time of writing — is driven here in order, and the two
/// collaborators are broken for one request each and repaired, which is what
/// [`support::SwitchableSessions`] and the directory's switch exist for.
#[tokio::test]
async fn every_declared_response_is_exercised() {
    let fixture = Fixture::working();
    let client = &fixture.client;

    // ── GET /v1/version → 200 ──────────────────────────────────────────────────────────────
    client
        .get("/v1/version")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK);

    // 413 is declared on *every* operation, because the body-size limit is mounted on the whole
    // router and a Kynos interceptor's declaration is its type (`S-C33`). So every operation
    // produces one here, sent the cheapest way the limit can be reached: an over-declared
    // `Content-Length`, refused before a byte of the body is read.
    for (method, path) in [
        ("GET", "/v1/version"),
        ("POST", "/v1/auth/login"),
        ("POST", "/v1/auth/refresh"),
        ("POST", "/v1/auth/logout"),
        ("POST", "/v1/upload"),
        ("PATCH", "/v1/upload/anything"),
        ("HEAD", "/v1/upload/anything"),
        ("DELETE", "/v1/upload/anything"),
        ("GET", "/v1/sync"),
        ("GET", "/v1/blob/deadbeef"),
        ("POST", "/v1/storage/verify"),
        ("POST", "/v1/auth/devices/directory"),
        ("GET", "/v1/auth/devices/directory/anyone"),
        ("POST", "/v1/albums/anything/ops"),
        ("POST", "/v1/albums"),
        ("GET", "/v1/quota"),
        ("GET", "/v1/upload/anything/receipt"),
        ("GET", "/.well-known/capsule/attestation-keys"),
        ("GET", "/.well-known/capsule/server-info"),
        ("GET", "/.well-known/capsule/deprecation"),
        ("GET", "/.well-known/capsule/revoked-jti"),
        ("POST", "/v1/auth/logout/all/challenge"),
        ("POST", "/v1/auth/logout/all"),
        ("PUT", "/v1/auth/escrow"),
        ("GET", "/v1/auth/escrow"),
        ("GET", "/v1/auth/devices"),
        ("DELETE", "/v1/auth/devices/anything"),
        ("POST", "/v1/auth/reauthenticate"),
        ("POST", "/v1/auth/devices/enroll"),
        ("POST", "/v1/auth/devices/enroll/redeem"),
        ("POST", "/v1/auth/devices/enroll/channel/anything"),
        ("GET", "/v1/auth/devices/enroll/channel/anything"),
        ("DELETE", "/v1/auth/devices/enroll/channel/anything"),
        ("GET", "/v1/moderation/record"),
    ] {
        let request = match method {
            "GET" => client.get(path),
            "PUT" => client.put(path),
            "PATCH" => client.patch(path),
            "HEAD" => client.head(path),
            "DELETE" => client.delete(path),
            _ => client.post(path),
        };
        request
            .header("content-length", &oversized().to_string())
            .body("application/json", "{}")
            .send()
            .await
            .assert_status(StatusCode::PAYLOAD_TOO_LARGE);
    }

    // ── POST /v1/auth/login ────────────────────────────────────────────────────────────────
    // 400, 415, 422 are the `Json` extractor's, declared on every operation that takes a body.
    client
        .post("/v1/auth/login")
        .body("application/json", "{ not json")
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .post("/v1/auth/login")
        .body("text/plain", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    client
        .post("/v1/auth/login")
        .json(&json!({ "email": EMAIL }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    // 401: the credentials did not match.
    client
        .post("/v1/auth/login")
        .json(&json!({ "email": EMAIL, "password": "wrong" }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    // 423: the account is locked. Reachable, therefore documented — the half of `S-C28` that
    // was closed by declaring rather than deleting.
    fixture.accounts.lock(EMAIL);
    client
        .post("/v1/auth/login")
        .json(&json!({ "email": EMAIL, "password": PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::LOCKED);
    fixture.accounts.insert(EMAIL, PASSWORD, &support::user());

    // 500: a collaborator could not answer.
    fixture.accounts.set_unavailable(true);
    client
        .post("/v1/auth/login")
        .json(&json!({ "email": EMAIL, "password": PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.accounts.set_unavailable(false);

    // 200.
    let pair: TokenResponse = client
        .post("/v1/auth/login")
        .header("accept", "application/json")
        .json(&json!({ "email": EMAIL, "password": PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();

    // ── POST /v1/auth/refresh ──────────────────────────────────────────────────────────────
    client
        .post("/v1/auth/refresh")
        .body("application/json", "{ not json")
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .post("/v1/auth/refresh")
        .body("text/plain", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    client
        .post("/v1/auth/refresh")
        .json(&json!({ "refresh_token": 42 }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    client
        .post("/v1/auth/refresh")
        .json(&json!({ "refresh_token": "not-a-token" }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    fixture.sessions.set_unavailable(true);
    client
        .post("/v1/auth/refresh")
        .json(&json!({ "refresh_token": pair.refresh_token }))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.sessions.set_unavailable(false);

    let rotated: TokenResponse = client
        .post("/v1/auth/refresh")
        .header("accept", "application/json")
        .json(&json!({ "refresh_token": pair.refresh_token }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();

    // ── POST /v1/auth/logout ───────────────────────────────────────────────────────────────
    // 401 with the `WWW-Authenticate` challenge the operation declares as required — a header
    // `assert_conformance` checks was actually sent.
    client
        .post("/v1/auth/logout")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    // 403: a live refresh token is a valid credential and an insufficient one.
    client
        .post("/v1/auth/logout")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    fixture.sessions.set_unavailable(true);
    client
        .post("/v1/auth/logout")
        .header("authorization", &format!("Bearer {}", rotated.access_token))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.sessions.set_unavailable(false);

    client
        .post("/v1/auth/logout")
        .header("authorization", &format!("Bearer {}", rotated.access_token))
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // ── The upload surface ─────────────────────────────────────────────────────────────────
    // One session is opened and driven through every answer the four operations can give. The
    // order is load-bearing: a `409` on `DELETE` needs a session mid-finalization, and a `200`
    // on `POST` needs one already open for the same bytes.
    let bearer = format!("Bearer {}", rotated.access_token);
    let first = payload(b'a', 4096);
    let second = payload(b'b', 4096);
    let whole: Vec<u8> = first.iter().chain(second.iter()).copied().collect();

    // POST 201, then PATCH 204 against it.
    let opened: serde_json::Value = client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await
        .assert_status(StatusCode::CREATED)
        .json();
    let id = opened["id"].as_str().expect("a session id").to_owned();
    let session = format!("/v1/upload/{id}");

    client
        .patch(&session)
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("x-capsule-offset", "0")
        .header("x-capsule-checksum", &checksum(&first))
        .body("application/octet-stream", first.clone())
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // POST 200: the active session for the same `(owner, hash, album)` tuple.
    client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await
        .assert_status(StatusCode::OK);

    // POST 400 / 415 / 422 / 426 / 401 / 403 / 500.
    client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .body("text/plain", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&json!({ "size": "not a number" }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", "2020-01-01")
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await
        .assert_status(StatusCode::UPGRADE_REQUIRED);
    client
        .post("/v1/upload")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    // 403: an album the authority does not hold — the fixture seeds exactly one.
    let mut elsewhere = create_request(&fixture.clock, &whole, "original");
    elsewhere["album_id"] = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5eff".into();
    elsewhere["manifest_envelope"]["album_id"] = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5eff".into();
    client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&elsewhere)
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // HEAD 200 / 400 / 401 / 403 / 404 / 426.
    client
        .head(&session)
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::OK);
    client
        .head(&session)
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .head(&session)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let stranger = fixture.other_bearer("01937b7c-0000-7000-8000-0000000000ff");
    client
        .head(&session)
        .header("authorization", &stranger)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    client
        .head("/v1/upload/nobody-opened-this")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
    client
        .head(&session)
        .header("authorization", &bearer)
        .header("x-capsule-protocol", "2020-01-01")
        .send()
        .await
        .assert_status(StatusCode::UPGRADE_REQUIRED);

    // PATCH 400 / 401 / 403 / 404 / 409 / 415 / 426.
    client
        .patch(&session)
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("x-capsule-checksum", &checksum(&second))
        .body("application/octet-stream", second.clone())
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .patch(&session)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .body("application/octet-stream", second.clone())
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .patch(&session)
        .header("authorization", &stranger)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("x-capsule-offset", "4096")
        .header("x-capsule-checksum", &checksum(&second))
        .body("application/octet-stream", second.clone())
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    client
        .patch("/v1/upload/nobody-opened-this")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("x-capsule-offset", "0")
        .header("x-capsule-checksum", &checksum(&second))
        .body("application/octet-stream", second.clone())
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
    client
        .patch(&session)
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("x-capsule-offset", "8192")
        .header("x-capsule-checksum", &checksum(&second))
        .body("application/octet-stream", second.clone())
        .send()
        .await
        .assert_status(StatusCode::CONFLICT);
    client
        .patch(&session)
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("x-capsule-offset", "4096")
        .header("x-capsule-checksum", &checksum(&second))
        .json(&json!({ "not": "bytes" }))
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    client
        .patch(&session)
        .header("authorization", &bearer)
        .header("x-capsule-protocol", "2020-01-01")
        .header("x-capsule-offset", "4096")
        .header("x-capsule-checksum", &checksum(&second))
        .body("application/octet-stream", second.clone())
        .send()
        .await
        .assert_status(StatusCode::UPGRADE_REQUIRED);

    // DELETE 401 / 403 / 404 / 426, then 204 on the session itself.
    client
        .delete(&session)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .delete(&session)
        .header("authorization", &stranger)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    client
        .delete("/v1/upload/nobody-opened-this")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
    client
        .delete(&session)
        .header("authorization", &bearer)
        .header("x-capsule-protocol", "2020-01-01")
        .send()
        .await
        .assert_status(StatusCode::UPGRADE_REQUIRED);
    client
        .delete(&session)
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // DELETE 409: finalization is not interruptible. A second session, claimed and left there.
    let finalizing: serde_json::Value = client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &second, "metadata"))
        .send()
        .await
        .assert_status(StatusCode::CREATED)
        .json();
    let claimed = finalizing["id"].as_str().expect("a session id").to_owned();
    fixture.uploads.claim_for_test(&claimed).await;
    client
        .delete(&format!("/v1/upload/{claimed}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::CONFLICT);

    // DELETE 204 on the first session, which is still open.
    client
        .delete(&session)
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // POST 409: the owner already holds these bytes (`S-C22`). Needs a blob that actually
    // finalized, so one small session is opened and completed in a single chunk first — which
    // is also the only place in this test where the index's durable half runs end to end.
    let held = payload(b'c', 4096);
    let completing: serde_json::Value = client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &held, "derivative"))
        .send()
        .await
        .assert_status(StatusCode::CREATED)
        .json();
    let completing = completing["id"].as_str().expect("a session id").to_owned();
    client
        .patch(&format!("/v1/upload/{completing}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("x-capsule-offset", "0")
        .header("x-capsule-checksum", &checksum(&held))
        .body("application/octet-stream", held.clone())
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &held, "derivative"))
        .send()
        .await
        .assert_status(StatusCode::CONFLICT);

    // The four 500s: one collaborator, broken for four requests and repaired.
    fixture.uploads.set_unavailable(true);
    client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&create_request(&fixture.clock, &whole, "original"))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    client
        .head(&session)
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    client
        .patch(&session)
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("x-capsule-offset", "0")
        .header("x-capsule-checksum", &checksum(&first))
        .body("application/octet-stream", first.clone())
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    client
        .delete(&session)
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.uploads.set_unavailable(false);

    // ── GET /v1/sync ───────────────────────────────────────────────────────────────────────
    // 401 first: the framework's, with the `WWW-Authenticate` challenge the operation declares.
    client
        .get("/v1/sync")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    // 403: a refresh token is a valid credential and an insufficient one. Declared by the
    // `Auth<AccessToken>` scheme rather than by this surface — which is the point of the
    // covered-check: the framework declared a status the handler never thinks about, and the
    // assertion made someone establish that it is reachable.
    client
        .get("/v1/sync")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // 400: the one cursor rejection. Malformed and foreign are deliberately the same answer.
    client
        .get("/v1/sync?cursor=not-a-cursor")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // 500: the index could not answer.
    fixture.index.set_unavailable(true);
    client
        .get("/v1/sync")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.index.set_unavailable(false);

    // 200, on an empty library: a client with nothing to sync still gets a cursor.
    client
        .get("/v1/sync")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK);

    // ── GET /v1/blob/{hash} ────────────────────────────────────────────────────────────────
    // 401 and 403 are the scheme's, exactly as on the feed.
    client
        .get("/v1/blob/deadbeef")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .get("/v1/blob/deadbeef")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // 400 is the `Path` extractor's, and it took a probe to establish that anything reaches it:
    // a `String` path parameter accepts every segment that *is* a string, so the only way in is
    // a segment that is not one. `%FF` is not valid UTF-8, so the request is refused before the
    // route runs. Reachable, therefore declared — the `S-C28` question asked of the framework's
    // own rejection rather than only of the handler's.
    client
        .get("/v1/blob/%FF")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // A blob to serve, put in through the ports rather than through an upload: this test is
    // about which statuses exist, and `tests/blob.rs` is where the serving behaviour lives.
    let ciphertext = payload(b'z', 8192);
    let address = capsule_server::blob::ContentAddress::parse(&checksum(&ciphertext))
        .expect("a content address");
    fixture
        .blobs
        .put(&address, &ciphertext)
        .await
        .expect("the store accepts");
    let asset = capsule_server::store::AssetId::new("conformance-served");
    fixture
        .index
        .reserve(capsule_server::index::PendingAsset {
            asset_id: asset.clone(),
            owner_id: support::owner(),
            album_id: support::album(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            crypto_suite_id: 1,
            created_at: jiff::Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the index reserves");
    for (role, at) in [
        (capsule_server::store::BlobRole::Provenance, &address),
        (capsule_server::store::BlobRole::Metadata, &address),
    ] {
        fixture
            .index
            .record_blob(
                &asset,
                capsule_server::index::BlobRecord {
                    role,
                    address: at.clone(),
                    size: ciphertext.len() as u64,
                    finalized_at: jiff::Timestamp::UNIX_EPOCH,
                },
            )
            .await
            .expect("the index records");
    }

    // 404: a well-formed address nothing references.
    client
        .get(&format!("/v1/blob/{}", checksum(b"nothing holds these")))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);

    // 200, 206 and 304 all come from one `Delivery`, which is why they are declared together.
    client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);
    client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .header("range", "bytes=0-1023")
        .send()
        .await
        .assert_status(StatusCode::PARTIAL_CONTENT);
    client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .header("if-none-match", &format!("\"{address}\""))
        .send()
        .await
        .assert_status(StatusCode::NOT_MODIFIED);

    // 500: the index could not answer, which must never look like a missing blob.
    fixture.index.set_unavailable(true);
    client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.index.set_unavailable(false);

    // 410 last, because it is the one that consumes the asset.
    fixture
        .index
        .tombstone(&asset, jiff::Timestamp::UNIX_EPOCH)
        .await
        .expect("the index tombstones");
    client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::GONE);

    // ── POST /v1/storage/verify ────────────────────────────────────────────────────────────
    // 401 and 403 are the scheme's; 415 and 422 are the `Json` extractor's, declared on every
    // operation that takes a body.
    client
        .post("/v1/storage/verify")
        .json(&json!({ "assets": [] }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .post("/v1/storage/verify")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .json(&json!({ "assets": [] }))
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    client
        .post("/v1/storage/verify")
        .header("authorization", &bearer)
        .body("text/plain", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    client
        .post("/v1/storage/verify")
        .header("authorization", &bearer)
        .json(&json!({ "assets": 42 }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    // 400: a well-formed body that is not a well-formed question.
    client
        .post("/v1/storage/verify")
        .header("authorization", &bearer)
        .json(&json!({ "assets": [] }))
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // 500: the index could not answer, which must never read as a durability finding.
    fixture.index.set_unavailable(true);
    client
        .post("/v1/storage/verify")
        .header("authorization", &bearer)
        .json(&json!({
            "assets": [{ "asset_id": "anything", "blob_hashes": [checksum(b"anything")] }],
        }))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.index.set_unavailable(false);

    // 200: a verdict, even for an asset that does not exist — "not durable" is an answer, and
    // refusing would make it indistinguishable from "could not check".
    client
        .post("/v1/storage/verify")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({
            "assets": [{ "asset_id": "anything", "blob_hashes": [checksum(b"anything")] }],
        }))
        .send()
        .await
        .assert_status(StatusCode::OK);

    // ── /v1/auth/devices/directory ─────────────────────────────────────────────────────────
    // One identity key for the whole block: the account is anchored to the first key it
    // publishes under (`S-C42`), so a fresh key per request would make every publish after the
    // first a 403 for the wrong reason.
    let account_ik = support::identity_key();
    let anchor = support::identity_header(&account_ik);
    let directory = support::signed_directory_by(&account_ik, 1);

    // 401 and 403 on both operations.
    client
        .post("/v1/auth/devices/directory")
        .header("x-capsule-identity-key", &anchor)
        .body("application/cbor", directory.clone())
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .get("/v1/auth/devices/directory/anyone")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .post("/v1/auth/devices/directory")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .header("x-capsule-identity-key", &anchor)
        .body("application/cbor", directory.clone())
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    client
        .get("/v1/auth/devices/directory/anyone")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // 415: the body is not CBOR. The `422` Kynos's own body rejection would also have declared
    // is gone, because `OpaqueBody` replaces the rejection with one a raw-bytes body can
    // actually produce — see `capsule_server::body`.
    client
        .post("/v1/auth/devices/directory")
        .header("authorization", &bearer)
        .header("x-capsule-identity-key", &anchor)
        .body("application/json", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);

    // 400 on publish: CBOR that is not a directory. 400 on fetch is the `Path` extractor's,
    // reached the only way a `String` path parameter can be: a segment that is not a string.
    client
        .post("/v1/auth/devices/directory")
        .header("authorization", &bearer)
        .header("x-capsule-identity-key", &anchor)
        .body("application/cbor", "not a directory")
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .get("/v1/auth/devices/directory/%FF")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // 404: nobody has published for this account yet.
    client
        .get(&format!("/v1/auth/devices/directory/{}", support::user()))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);

    // 500 on both: the store cannot answer.
    fixture.directories.set_unavailable(true);
    client
        .post("/v1/auth/devices/directory")
        .header("authorization", &bearer)
        .header("x-capsule-identity-key", &anchor)
        .body("application/cbor", directory.clone())
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    client
        .get(&format!("/v1/auth/devices/directory/{}", support::user()))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.directories.set_unavailable(false);

    // 200 on publish, then 200 on fetch, then 409 on a version that does not advance.
    client
        .post("/v1/auth/devices/directory")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .header("x-capsule-identity-key", &anchor)
        .body("application/cbor", directory.clone())
        .send()
        .await
        .assert_status(StatusCode::OK);
    client
        .get(&format!("/v1/auth/devices/directory/{}", support::user()))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);
    client
        .post("/v1/auth/devices/directory")
        .header("authorization", &bearer)
        .header("x-capsule-identity-key", &anchor)
        .body(
            "application/cbor",
            support::signed_directory_by(&account_ik, 1),
        )
        .send()
        .await
        .assert_status(StatusCode::CONFLICT);

    // 403 on publish, the *other* way: the document is well-formed and correctly signed, but by
    // a key that is not this anchored account's (`S-C42`). Distinct from the credential 403
    // above, which is the framework's.
    let impostor = support::identity_key();
    client
        .post("/v1/auth/devices/directory")
        .header("authorization", &bearer)
        .header(
            "x-capsule-identity-key",
            &support::identity_header(&impostor),
        )
        .body(
            "application/cbor",
            support::signed_directory_by(&impostor, 9),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // ── POST /v1/albums/{album_id}/ops ─────────────────────────────────────────────────────
    // The asset every arm below acts on, published through the ports.
    let provenance = payload(b'p', 64);
    let created = checksum(&provenance);
    let provenance_address =
        capsule_server::blob::ContentAddress::parse(&created).expect("a content address");
    fixture
        .blobs
        .put(&provenance_address, &provenance)
        .await
        .expect("the store accepts");
    let asset = capsule_server::store::AssetId::new(support::OPS_ASSET);
    fixture
        .index
        .reserve(capsule_server::index::PendingAsset {
            asset_id: asset.clone(),
            owner_id: support::owner(),
            album_id: support::album(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            crypto_suite_id: capsule_core::crypto::CRYPTO_SUITE_ID,
            created_at: jiff::Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the index reserves");
    for (role, at) in [
        (
            capsule_server::store::BlobRole::Provenance,
            provenance_address.clone(),
        ),
        (
            capsule_server::store::BlobRole::Metadata,
            capsule_server::blob::ContentAddress::parse(&checksum(b"ops-metadata"))
                .expect("a content address"),
        ),
    ] {
        fixture
            .index
            .record_blob(
                &asset,
                capsule_server::index::BlobRecord {
                    role,
                    address: at,
                    size: 64,
                    finalized_at: jiff::Timestamp::UNIX_EPOCH,
                },
            )
            .await
            .expect("the index records");
    }
    let ops_path = format!("/v1/albums/{}/ops", support::album());

    // 401 and 403 (no capability on an album this caller does not hold).
    client
        .post(&ops_path)
        .json(&support::op_bundle(
            &fixture.clock,
            "delete",
            "c1",
            Some(&created),
        ))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    let elsewhere = support::second_album();
    let mut unwritable = support::op_bundle(&fixture.clock, "delete", "c1", Some(&created));
    unwritable["manifest_envelope"]["album_id"] = json!(elsewhere.as_str());
    client
        .post(&format!("/v1/albums/{elsewhere}/ops"))
        .header("authorization", &bearer)
        .json(&unwritable)
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // 415 and 422 are the `Json` extractor's; 400 is a manifest that fails the battery.
    client
        .post(&ops_path)
        .header("authorization", &bearer)
        .body("text/plain", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    client
        .post(&ops_path)
        .header("authorization", &bearer)
        .json(&json!({ "manifest_envelope": 42 }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    client
        .post(&ops_path)
        .header("authorization", &bearer)
        .json(&support::op_bundle(
            &fixture.clock,
            "create",
            "c1",
            Some(&created),
        ))
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // 426: a protocol version outside the accepted window.
    let mut ancient = support::op_bundle(&fixture.clock, "delete", "c1", Some(&created));
    ancient["manifest_envelope"]["protocol_version"] = json!("1999-01-01");
    client
        .post(&ops_path)
        .header("authorization", &bearer)
        .json(&ancient)
        .send()
        .await
        .assert_status(StatusCode::UPGRADE_REQUIRED);

    // 500: the index could not answer.
    fixture.index.set_unavailable(true);
    client
        .post(&ops_path)
        .header("authorization", &bearer)
        .json(&support::op_bundle(
            &fixture.clock,
            "delete",
            "c1",
            Some(&created),
        ))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.index.set_unavailable(false);

    // 200, then 409 for a manifest that no longer chains.
    client
        .post(&ops_path)
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&support::op_bundle(
            &fixture.clock,
            "delete",
            "c1",
            Some(&created),
        ))
        .send()
        .await
        .assert_status(StatusCode::OK);
    client
        .post(&ops_path)
        .header("authorization", &bearer)
        .json(&support::op_bundle(
            &fixture.clock,
            "trash-restore",
            "c2",
            Some(&created),
        ))
        .send()
        .await
        .assert_status(StatusCode::CONFLICT);

    // ── POST /v1/albums ────────────────────────────────────────────────────────────────────
    const DERIVED: &str = "0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35";

    client
        .post("/v1/albums")
        .json(&json!({ "album_id": DERIVED }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .post("/v1/albums")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .json(&json!({ "album_id": DERIVED }))
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // 415 and 422 are the `Json` extractor's — the second reachable because the body is
    // strict, so an album *name* is a refusal rather than a silently-dropped field.
    client
        .post("/v1/albums")
        .header("authorization", &bearer)
        .body("text/plain", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    client
        .post("/v1/albums")
        .header("authorization", &bearer)
        .json(&json!({ "album_id": DERIVED, "name": "Holidays" }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    // 400: not a canonical UUID.
    client
        .post("/v1/albums")
        .header("authorization", &bearer)
        .json(&json!({ "album_id": "not-a-uuid" }))
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // 500: the store could not answer.
    fixture.albums.set_unavailable(true);
    client
        .post("/v1/albums")
        .header("authorization", &bearer)
        .json(&json!({ "album_id": DERIVED }))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.albums.set_unavailable(false);

    // 201 then 200: idempotent by contract.
    client
        .post("/v1/albums")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({ "album_id": DERIVED }))
        .send()
        .await
        .assert_status(StatusCode::CREATED);
    client
        .post("/v1/albums")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({ "album_id": DERIVED }))
        .send()
        .await
        .assert_status(StatusCode::OK);

    // ── GET /v1/quota ──────────────────────────────────────────────────────────────────────
    client
        .get("/v1/quota")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .get("/v1/quota")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    fixture.quotas.set_unavailable(true);
    client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.quotas.set_unavailable(false);
    client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK);

    // ── GET /v1/upload/{id}/receipt ────────────────────────────────────────────────────────
    client
        .get("/v1/upload/anything/receipt")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .get("/v1/upload/anything/receipt")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    // 400 is the `Path` extractor's, reached the only way a `String` parameter allows.
    client
        .get("/v1/upload/%FF/receipt")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .get("/v1/upload/no-such-session/receipt")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
    fixture.uploads.set_unavailable(true);
    client
        .get("/v1/upload/anything/receipt")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.uploads.set_unavailable(false);

    // 409 then 200: a session mid-transfer has no receipt yet, and a finalized one does.
    let receipt_bytes = payload(b'r', 8192);
    let opened: serde_json::Value = client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("accept", "application/json")
        .json(&create_request(&fixture.clock, &receipt_bytes, "original"))
        .send()
        .await
        .assert_status(StatusCode::CREATED)
        .json();
    let receipt_session = opened["id"].as_str().expect("a session id").to_owned();
    client
        .get(&format!("/v1/upload/{receipt_session}/receipt"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::CONFLICT);
    for offset in [0_usize, 4096] {
        client
            .patch(&format!("/v1/upload/{receipt_session}"))
            .header("authorization", &bearer)
            .header("x-capsule-protocol", PROTOCOL_VERSION)
            .header("x-capsule-offset", &offset.to_string())
            .header(
                "x-capsule-checksum",
                &checksum(&receipt_bytes[offset..offset + 4096]),
            )
            .body(
                "application/octet-stream",
                receipt_bytes[offset..offset + 4096].to_vec(),
            )
            .send()
            .await
            .assert_status(StatusCode::NO_CONTENT);
    }
    client
        .get(&format!("/v1/upload/{receipt_session}/receipt"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);

    // ── GET /.well-known/capsule/attestation-keys ──────────────────────────────────────────
    // The one operation with no credential, deliberately: a client pinning the key that checks
    // the server's own liability must not need the server's permission to fetch it. So there is
    // no 401 to cover here, and its absence from the declared set is the assertion.
    client
        .get("/.well-known/capsule/attestation-keys")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK);

    // ── The rest of the `.well-known/capsule` registry (`S-C18`) ───────────────────────────
    // Public for the same reason, so again there is no 401 in any of these declared sets and
    // again the absence is the assertion.
    for path in [
        "/.well-known/capsule/server-info",
        "/.well-known/capsule/deprecation",
        "/.well-known/capsule/revoked-jti",
    ] {
        client
            .get(path)
            .header("accept", "application/json")
            .send()
            .await
            .assert_status(StatusCode::OK);
    }

    // The revocation list's 503, which is a claim rather than a formality: the endpoint refuses
    // to serve an empty list on a storage failure, because an empty list is the strongest
    // statement it can make and a peer's fail-closed rule would believe it.
    fixture.revocations.set_unavailable(true);
    client
        .get("/.well-known/capsule/revoked-jti")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::SERVICE_UNAVAILABLE);
    fixture.revocations.set_unavailable(false);

    // ── Re-authentication and the cross-device add (`S-C7`) ────────────────────────────────
    // Before the ledger block, because a re-authentication is what reopens the freshness window
    // the enrollment gate reads.
    //
    // A fresh sign-in first: `bearer`'s own session was closed by the logout block above, and
    // re-authentication is the one operation here that needs a *live session record* rather
    // than a merely valid token — it moves a field on one. That the two differ is `S-C48`.
    let enrolling: capsule_server::routes::auth::TokenResponse = client
        .post("/v1/auth/login")
        .header("accept", "application/json")
        .json(&serde_json::json!({ "email": support::EMAIL, "password": support::PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let bearer = format!("Bearer {}", enrolling.access_token);

    client
        .post("/v1/auth/reauthenticate")
        .json(&serde_json::json!({ "password": support::PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .post("/v1/auth/reauthenticate")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .json(&serde_json::json!({ "password": support::PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    client
        .post("/v1/auth/reauthenticate")
        .header("authorization", &bearer)
        .body("text/plain", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    client
        .post("/v1/auth/reauthenticate")
        .header("authorization", &bearer)
        .body("application/json", "{ not json")
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .post("/v1/auth/reauthenticate")
        .header("authorization", &bearer)
        .json(&serde_json::json!({ "password": 7 }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    // 423: the account is locked. Locked and unlocked around the one request, so nothing after
    // this block inherits the state.
    fixture.accounts.lock(support::EMAIL);
    client
        .post("/v1/auth/reauthenticate")
        .header("authorization", &bearer)
        .json(&serde_json::json!({ "password": support::PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::LOCKED);
    fixture.accounts.unlock(support::EMAIL);

    fixture.accounts.set_unavailable(true);
    client
        .post("/v1/auth/reauthenticate")
        .header("authorization", &bearer)
        .json(&serde_json::json!({ "password": support::PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.accounts.set_unavailable(false);

    client
        .post("/v1/auth/reauthenticate")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&serde_json::json!({ "password": support::PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::OK);

    // Enrollment: 401 and 500 first, then the 403 the freshness gate produces, then a 200 with
    // the window reopened.
    client
        .post("/v1/auth/devices/enroll")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .post("/v1/auth/devices/enroll")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    fixture.sessions.set_unavailable(true);
    client
        .post("/v1/auth/devices/enroll")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.sessions.set_unavailable(false);

    let enrolled: serde_json::Value = client
        .post("/v1/auth/devices/enroll")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let code = enrolled["code"].as_str().expect("a code").to_owned();

    // Redeem: the body rejections, the 404, the 500, then the 200 that opens the channel.
    client
        .post("/v1/auth/devices/enroll/redeem")
        .body("text/plain", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    client
        .post("/v1/auth/devices/enroll/redeem")
        .body("application/json", "{ not json")
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .post("/v1/auth/devices/enroll/redeem")
        .json(&serde_json::json!({ "code": 7 }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    client
        .post("/v1/auth/devices/enroll/redeem")
        .json(&serde_json::json!({ "code": "never issued" }))
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);

    let opened: serde_json::Value = client
        .post("/v1/auth/devices/enroll/redeem")
        .header("accept", "application/json")
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let channel = opened["channel_id"].as_str().expect("a channel").to_owned();
    let relay_path = format!("/v1/auth/devices/enroll/channel/{channel}");

    // The relay's own answers.
    client
        .post(&relay_path)
        .body("text/plain", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    client
        .post(&relay_path)
        .body("application/json", "{ not json")
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .post(&relay_path)
        .json(&serde_json::json!({ "direction": 7, "payload": "x" }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    client
        .post("/v1/auth/devices/enroll/channel/no-such-channel")
        .json(&serde_json::json!({ "direction": "to_enrollee", "payload": "x" }))
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
    client
        .post(&relay_path)
        .json(&serde_json::json!({ "direction": "to_enrollee", "payload": "x" }))
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // The drain: a bad direction, a missing channel, a 400 from the `Path` extractor, then 200.
    client
        .get(&format!("{relay_path}?direction=sideways"))
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .get("/v1/auth/devices/enroll/channel/no-such-channel?direction=to_enrollee")
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
    client
        .get(&format!("{relay_path}?direction=to_enrollee"))
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK);

    // The close: unauthenticated, insufficient, a 400 from the `Path` extractor, a 404 for a
    // channel that is not this account's, then the 204.
    client
        .delete(&relay_path)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .delete(&relay_path)
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    client
        .delete("/v1/auth/devices/enroll/channel/%FF")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .delete("/v1/auth/devices/enroll/channel/no-such-channel")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
    client
        .delete(&relay_path)
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // The two extractor `400`s the relay and drain paths still owe, reached the only way a
    // `String` path parameter can be.
    client
        .post("/v1/auth/devices/enroll/channel/%FF")
        .json(&serde_json::json!({ "direction": "to_enrollee", "payload": "x" }))
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .get("/v1/auth/devices/enroll/channel/%FF?direction=to_enrollee")
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // 500 on all three relay operations: the channel store cannot answer.
    fixture.channels.set_unavailable(true);
    client
        .post(&relay_path)
        .json(&serde_json::json!({ "direction": "to_enrollee", "payload": "x" }))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    client
        .get(&format!("{relay_path}?direction=to_enrollee"))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    client
        .delete(&relay_path)
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.channels.set_unavailable(false);

    // 500 on redeem: the enrollment store cannot answer.
    fixture.enrollments.set_unavailable(true);
    client
        .post("/v1/auth/devices/enroll/redeem")
        .json(&serde_json::json!({ "code": "anything" }))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.enrollments.set_unavailable(false);

    // ── The session ledger (`S-C13`, `S-N3`) ───────────────────────────────────────────────
    // Before the escrow block, and before the global sign-out at the end, because a revoke
    // closes sessions the rest of the walk is using.
    client
        .get("/v1/auth/devices")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .delete("/v1/auth/devices/anything")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .get("/v1/auth/devices")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    client
        .delete("/v1/auth/devices/anything")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // 404: no live session of this account has that id. 400 is the `Path` extractor's, reached
    // the only way a `String` path parameter can be — a segment that is not a string.
    client
        .delete("/v1/auth/devices/01937b7c-0000-7000-8000-00000000dead")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
    client
        .delete("/v1/auth/devices/%FF")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // 500 on both, from each of the two collaborators the listing reads.
    fixture.sessions.set_unavailable(true);
    client
        .get("/v1/auth/devices")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    client
        .delete("/v1/auth/devices/anything")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.sessions.set_unavailable(false);
    fixture.cohorts.set_unavailable(true);
    client
        .get("/v1/auth/devices")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.cohorts.set_unavailable(false);

    // 200, then a 204 that revokes a session this walk is not using.
    let ledger: serde_json::Value = client
        .get("/v1/auth/devices")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let spare = ledger["sessions"]
        .as_array()
        .expect("a sessions array")
        .iter()
        .find(|s| s["current"] == serde_json::json!(false))
        .map(|s| s["session_id"].as_str().expect("a session id").to_owned());
    if let Some(spare) = spare {
        client
            .delete(&format!("/v1/auth/devices/{spare}"))
            .header("authorization", &bearer)
            .send()
            .await
            .assert_status(StatusCode::NO_CONTENT);
    } else {
        // No spare session to end, so a second sign-in provides one rather than the walk
        // revoking the credential every block after this depends on.
        let extra: capsule_server::routes::auth::TokenResponse = client
            .post("/v1/auth/login")
            .header("accept", "application/json")
            .json(&serde_json::json!({ "email": support::EMAIL, "password": support::PASSWORD }))
            .send()
            .await
            .assert_status(StatusCode::OK)
            .json();
        let ledger: serde_json::Value = client
            .get("/v1/auth/devices")
            .header("authorization", &format!("Bearer {}", extra.access_token))
            .header("accept", "application/json")
            .send()
            .await
            .assert_status(StatusCode::OK)
            .json();
        let target = ledger["sessions"]
            .as_array()
            .expect("a sessions array")
            .iter()
            .find(|s| s["current"] == serde_json::json!(true))
            .expect("the extra session")["session_id"]
            .as_str()
            .expect("a session id")
            .to_owned();
        client
            .delete(&format!("/v1/auth/devices/{target}"))
            .header("authorization", &bearer)
            .send()
            .await
            .assert_status(StatusCode::NO_CONTENT);
    }

    // ── The moderation record (`S-C8`) ─────────────────────────────────────────────────────
    client
        .get("/v1/moderation/record")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .get("/v1/moderation/record")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    fixture.moderation.set_unavailable(true);
    client
        .get("/v1/moderation/record")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.moderation.set_unavailable(false);
    client
        .get("/v1/moderation/record")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK);

    // ── The master-key escrow (`S-C12`) ────────────────────────────────────────────────────
    client
        .put("/v1/auth/escrow")
        .body("application/octet-stream", vec![7_u8; 64])
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .get("/v1/auth/escrow")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .put("/v1/auth/escrow")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .body("application/octet-stream", vec![7_u8; 64])
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);
    client
        .get("/v1/auth/escrow")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    // 415: not octet-stream. 400: a body that cannot be an escrow at any version.
    client
        .put("/v1/auth/escrow")
        .header("authorization", &bearer)
        .body("application/json", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    client
        .put("/v1/auth/escrow")
        .header("authorization", &bearer)
        .body("application/octet-stream", Vec::<u8>::new())
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    // 404: nothing escrowed yet — asserted before the store, which is why this order matters.
    client
        .get("/v1/auth/escrow")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);

    // 500 on both: the store cannot answer.
    fixture.escrows.set_unavailable(true);
    client
        .put("/v1/auth/escrow")
        .header("authorization", &bearer)
        .body("application/octet-stream", vec![7_u8; 64])
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    client
        .get("/v1/auth/escrow")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.escrows.set_unavailable(false);

    // 200 on both.
    client
        .put("/v1/auth/escrow")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .body("application/octet-stream", vec![7_u8; 64])
        .send()
        .await
        .assert_status(StatusCode::OK);
    client
        .get("/v1/auth/escrow")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);

    // ── The global sign-out ceremony (`S-C23`) ─────────────────────────────────────────────
    // Last, because a successful revoke closes every session this walk has been using. The
    // directory block above anchored the account to `account_ik`, which is the key the proof
    // has to be made under — that dependency is the ceremony, not test order.
    client
        .post("/v1/auth/logout/all/challenge")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    client
        .post("/v1/auth/logout/all/challenge")
        .header(
            "authorization",
            &format!("Bearer {}", rotated.refresh_token),
        )
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    fixture.challenges.set_unavailable(true);
    client
        .post("/v1/auth/logout/all/challenge")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.challenges.set_unavailable(false);

    let issued: serde_json::Value = client
        .post("/v1/auth/logout/all/challenge")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let challenge = issued["challenge"]
        .as_str()
        .expect("a challenge")
        .to_owned();

    // The revoke's body rejections: 415 for the wrong media type, 400 for JSON that does not
    // parse, 422 for JSON that parses and is not this request.
    client
        .post("/v1/auth/logout/all")
        .body("text/plain", "{}")
        .send()
        .await
        .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    client
        .post("/v1/auth/logout/all")
        .body("application/json", "{ not json")
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);
    client
        .post("/v1/auth/logout/all")
        .json(&serde_json::json!({ "challenge": 7 }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    // 401: a proof that does not verify. Burns a challenge, so this one is asked for on its own.
    let doomed: serde_json::Value = client
        .post("/v1/auth/logout/all/challenge")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    client
        .post("/v1/auth/logout/all")
        .json(&serde_json::json!({
            "challenge": doomed["challenge"],
            "proof": support::revoke_proof(&support::identity_key(), "not this challenge"),
        }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    // 500: the challenge store cannot answer. The proof decodes first, so this reaches the
    // store rather than stopping at the body.
    fixture.challenges.set_unavailable(true);
    client
        .post("/v1/auth/logout/all")
        .json(&serde_json::json!({
            "challenge": challenge,
            "proof": support::revoke_proof(&account_ik, &challenge),
        }))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.challenges.set_unavailable(false);

    // 200: the whole ceremony, against the directory the block above anchored.
    let final_challenge: serde_json::Value = client
        .post("/v1/auth/logout/all/challenge")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let final_challenge = final_challenge["challenge"].as_str().expect("a challenge");
    client
        .post("/v1/auth/logout/all")
        .header("accept", "application/json")
        .json(&serde_json::json!({
            "challenge": final_challenge,
            "proof": support::revoke_proof(&account_ik, final_challenge),
        }))
        .send()
        .await
        .assert_status(StatusCode::OK);

    // Nothing escaped the description on the way through, and nothing the description promises
    // was left unproduced.
    client.assert_conformance();
    client.assert_declared_responses_covered();
}

/// The router builds and describes itself.
///
/// `openapi()` is the only path from this code to a description — there is no document to
/// hand-edit — so a failure here means the types cannot be described, which is a design fault
/// rather than a documentation one.
#[test]
fn the_router_emits_a_document() {
    let document = capsule_server::openapi().expect("router describes itself");
    let json = document.to_json().expect("document serializes");

    assert!(
        json.contains("/v1/version"),
        "the emitted document must carry the operation the server serves"
    );
}

/// No album-shaped schema carries a plaintext title (`S-C26`).
///
/// The Salvo schema had `albums.name` and `albums.description` as plaintext Postgres columns —
/// a residue of the pre-key-free design, and on a key-free server a privacy defect rather than
/// dead weight. This port simply never declared them, which is the cheapest possible
/// resolution and also the easiest to undo by accident: somebody adds a convenience field to
/// an album response and the server is holding user content again.
///
/// So the guard is on the **emitted document**, not on a Rust struct. It walks every schema
/// whose name mentions an album and asserts none of them has a `name` or `description`
/// property. `VersionResponse` legitimately has a `name` — the server's own package name — and
/// is exactly why this is scoped to album schemas rather than banning the word.
#[test]
fn no_album_schema_carries_a_plaintext_title() {
    let document = capsule_server::openapi().expect("router describes itself");
    let json: serde_json::Value =
        serde_json::from_str(&document.to_json().expect("document serializes"))
            .expect("the document is JSON");

    let schemas = json["components"]["schemas"]
        .as_object()
        .expect("the document declares schemas");

    let mut checked = 0;
    for (name, schema) in schemas {
        if !name.to_lowercase().contains("album") {
            continue;
        }
        checked += 1;
        let Some(properties) = schema["properties"].as_object() else {
            continue;
        };
        for forbidden in ["name", "description", "album_name", "album_description"] {
            assert!(
                !properties.contains_key(forbidden),
                "{name} declares `{forbidden}`: album titles are user content and belong in the \
                 encrypted sidecar, not on a key-free server (S-C26)"
            );
        }
    }
    assert!(
        checked > 0,
        "the guard checked nothing; the album schemas must have been renamed out from under it"
    );
}

/// The emitted document declares OpenAPI 3.2.
///
/// 3.2 is a project-level requirement, not a preference: spargen consumes this document to
/// generate the typed client, and 3.2 is what lets a stream and a binary body be *described*
/// rather than hand-parsed. A silent drop to the `openapi31` default would not fail any other
/// test here — the paths and schemas would still be right — so the version is asserted on its
/// own. It is a one-word difference in a feature list that changes what the client can express.
#[test]
fn the_document_declares_openapi_32() {
    let document = capsule_server::openapi().expect("router describes itself");
    let json = document.to_json().expect("document serializes");

    let parsed: serde_json::Value = serde_json::from_str(&json).expect("document is JSON");
    let version = parsed
        .get("openapi")
        .and_then(serde_json::Value::as_str)
        .expect("every OpenAPI document declares its version");

    assert!(
        version.starts_with("3.2"),
        "expected an OpenAPI 3.2 document, got {version:?} — check the `openapi32` feature"
    );
}
