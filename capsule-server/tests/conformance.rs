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

use capsule_server::routes::auth::TokenResponse;
use capsule_server::routes::version::VersionResponse;
use kynos::http::StatusCode;
use kynos::test::TestClient;
use serde_json::json;
use support::{EMAIL, Fixture, PASSWORD};

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
