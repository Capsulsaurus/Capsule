//! The master-key escrow surface (slice `S-C12`), end to end.
//!
//! The case that carries the slice is `a_rotation_replaces_the_escrow_and_the_old_one_is_gone`:
//! after a guided re-wrap the lost recovery secret must unwrap nothing, and a server that kept
//! the previous blob would preserve exactly the artifact the rotation exists to destroy.

mod support;

use kynos::http::StatusCode;
use serde_json::Value;
use support::Fixture;

/// A blob that is not a real wrap and does not need to be — the server cannot tell.
fn wrapped(seed: u8, len: usize) -> Vec<u8> {
    (0..len).map(|i| seed ^ (i as u8)).collect()
}

/// Store `blob` and assert the status.
async fn store(
    fixture: &Fixture,
    bearer: &str,
    blob: Vec<u8>,
    expect: StatusCode,
) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .put("/v1/auth/escrow")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .body("application/octet-stream", blob)
        .send()
        .await;
    response.assert_status(expect);
    response
}

/// Fetch the caller's escrow and assert the status.
async fn fetch(fixture: &Fixture, bearer: &str, expect: StatusCode) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .get("/v1/auth/escrow")
        .header("authorization", bearer)
        .send()
        .await;
    response.assert_status(expect);
    response
}

#[tokio::test]
async fn a_stored_escrow_comes_back_byte_for_byte() {
    // The bytes are what a client runs its KDF against. A re-encoded wrap is a wrap that no
    // longer opens, and the failure would look like a lost master key.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let blob = wrapped(0xA5, 512);

    let body: Value = store(&fixture, &bearer, blob.clone(), StatusCode::OK)
        .await
        .json();
    assert_eq!(body["replaced"], false);

    let fetched = fetch(&fixture, &bearer, StatusCode::OK).await;
    assert_eq!(
        fetched.header("content-type"),
        Some("application/octet-stream"),
        "the escrow is ciphertext with no schema the server knows, and says so"
    );
    assert_eq!(fetched.bytes().as_ref(), blob.as_slice());
}

#[tokio::test]
async fn a_rotation_replaces_the_escrow_and_the_old_one_is_gone() {
    // The single-active-escrow rule, which is the whole point of a guided re-wrap: the lost
    // recovery secret has to stop working.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let first = wrapped(0x11, 256);
    let second = wrapped(0x22, 300);

    store(&fixture, &bearer, first.clone(), StatusCode::OK).await;
    let body: Value = store(&fixture, &bearer, second.clone(), StatusCode::OK)
        .await
        .json();
    assert_eq!(
        body["replaced"], true,
        "a rotation and a first escrow are different events, and a client acts on which it was"
    );

    let fetched = fetch(&fixture, &bearer, StatusCode::OK).await;
    assert_eq!(fetched.bytes().as_ref(), second.as_slice());
    assert_ne!(
        fetched.bytes().as_ref(),
        first.as_slice(),
        "the previous wrap must be unreachable, not merely superseded"
    );
}

#[tokio::test]
async fn an_account_with_no_escrow_is_a_distinct_answer_from_a_failure() {
    // A client tells "you have no recovery backup" (a setup prompt) from "we could not read it"
    // (a retry) by the code, and getting that wrong sends a user to the wrong screen.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let problem: Value = fetch(&fixture, &bearer, StatusCode::NOT_FOUND).await.json();
    assert_eq!(problem["code"], "error.escrow.not_stored");

    fixture.escrows.set_unavailable(true);
    let problem: Value = fetch(&fixture, &bearer, StatusCode::INTERNAL_SERVER_ERROR)
        .await
        .json();
    assert_eq!(problem["code"], "error.escrow.unavailable");
}

#[tokio::test]
async fn one_accounts_escrow_is_not_another_s() {
    // There is no `{user_id}` on this path, so fetching somebody else's is unrepresentable
    // rather than forbidden — this pins that the scoping is real and not incidental.
    let fixture = Fixture::working();
    let mine = fixture.bearer().await;
    let theirs = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;

    store(&fixture, &mine, wrapped(0x33, 128), StatusCode::OK).await;
    fetch(&fixture, &theirs, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn a_body_that_cannot_be_an_escrow_is_refused() {
    // The one judgement the server is entitled to make. It is a size bound, not a format check:
    // the server cannot tell a real wrap from noise of the same length, and pretending
    // otherwise would put a format it does not own on its critical path.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let problem: Value = store(&fixture, &bearer, Vec::new(), StatusCode::BAD_REQUEST)
        .await
        .json();
    assert_eq!(problem["code"], "error.escrow.malformed");

    let huge = wrapped(0x44, capsule_server::escrow::MAX_ESCROW_BYTES + 1);
    let problem: Value = store(&fixture, &bearer, huge, StatusCode::BAD_REQUEST)
        .await
        .json();
    assert_eq!(problem["code"], "error.escrow.malformed");

    fetch(&fixture, &bearer, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn a_refused_store_leaves_the_held_escrow_alone() {
    // A rejected rotation must not be a deletion. Otherwise a client bug that sends an empty
    // body once would take out the account's only recovery path.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let held = wrapped(0x55, 200);
    store(&fixture, &bearer, held.clone(), StatusCode::OK).await;

    store(&fixture, &bearer, Vec::new(), StatusCode::BAD_REQUEST).await;

    let fetched = fetch(&fixture, &bearer, StatusCode::OK).await;
    assert_eq!(fetched.bytes().as_ref(), held.as_slice());
}

#[tokio::test]
async fn the_escrow_surface_requires_a_credential() {
    let fixture = Fixture::working();
    fixture
        .client
        .get("/v1/auth/escrow")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    fixture
        .client
        .put("/v1/auth/escrow")
        .body("application/octet-stream", wrapped(0x66, 64))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
