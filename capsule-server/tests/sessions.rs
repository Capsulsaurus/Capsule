//! `GET /v1/upload/sessions` — the resumption listing (slice `S-C57`), end to end.
//!
//! The case that carries the slice is `a_client_that_lost_its_session_ids_can_find_them_again`:
//! that is the whole reason the operation exists. Beside it,
//! `an_unknown_status_filter_is_refused_rather_than_ignored` pins the defect the retired handler
//! had — it parsed the filter with `.ok()` and dropped the `None`, so one letter off returned
//! every session instead of none.

mod support;

use kynos::http::StatusCode;
use serde_json::Value;
use support::{Fixture, payload};

/// List the caller's sessions, asserting the status.
async fn list(
    fixture: &Fixture,
    bearer: &str,
    query: &str,
    expect: StatusCode,
) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .get(&format!("/v1/upload/sessions{query}"))
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await;
    response.assert_status(expect);
    response
}

/// The ids in a listing, in the order they were served.
fn ids(body: &Value) -> Vec<String> {
    body["sessions"]
        .as_array()
        .expect("a sessions array")
        .iter()
        .map(|session| session["id"].as_str().expect("an id").to_owned())
        .collect()
}

#[tokio::test]
async fn a_client_that_lost_its_session_ids_can_find_them_again() {
    // The reason the operation exists. Without it, bytes already on the server after a reinstall
    // are unreachable and are eventually evicted.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let first = fixture
        .open_session(&payload(b'a', 4096), "original", &bearer)
        .await;
    let second = fixture
        .open_session(&payload(b'b', 2048), "metadata", &bearer)
        .await;

    let body: Value = list(&fixture, &bearer, "", StatusCode::OK).await.json();
    let listed = ids(&body);
    assert!(listed.contains(&first), "{listed:?}");
    assert!(listed.contains(&second), "{listed:?}");
}

#[tokio::test]
async fn a_listing_carries_what_a_resume_needs_and_nothing_signed() {
    // The manifest envelope is finalization's input and is already the client's own; echoing it
    // into every listing would put a signed document in a response nobody reads it from.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'a', 8192);
    let id = fixture.open_session(&bytes, "original", &bearer).await;
    fixture
        .chunk(&id, 0, &bytes[..4096], &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let body: Value = list(&fixture, &bearer, "", StatusCode::OK).await.json();
    let session = &body["sessions"][0];
    assert_eq!(session["id"], id);
    assert_eq!(
        session["received_bytes"], 4096,
        "the offset a resume continues from"
    );
    assert_eq!(session["total_size"], 8192);
    assert_eq!(session["status"], "uploading");
    assert_eq!(session["blob_role"], "original");
    assert!(session["asset_id"].as_str().is_some());
    assert!(
        session.get("manifest_envelope").is_none()
            && session.get("expected_hash").is_none()
            && session.get("crypto_suite_id").is_none(),
        "the listing is a resumption view, not the finalization record: {session}"
    );
}

#[tokio::test]
async fn an_empty_listing_is_a_normal_answer() {
    // An account with nothing in flight has an empty library of sessions, not a missing one.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let body: Value = list(&fixture, &bearer, "", StatusCode::OK).await.json();
    assert_eq!(body["sessions"].as_array().expect("an array").len(), 0);
}

#[tokio::test]
async fn the_listing_is_scoped_to_the_uploader() {
    // Resuming is something only the uploading party can do, so this is what the scope is for.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture
        .open_session(&payload(b'a', 4096), "original", &bearer)
        .await;

    let stranger = fixture
        .other_bearer("018f3f1e-0000-7000-8000-00000000ffff")
        .await;
    let body: Value = list(&fixture, &stranger, "", StatusCode::OK).await.json();
    assert_eq!(
        body["sessions"].as_array().expect("an array").len(),
        0,
        "another account's in-flight uploads are not this caller's to resume: {body}"
    );
}

#[tokio::test]
async fn a_status_filter_narrows_the_listing() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let untouched = fixture
        .open_session(&payload(b'a', 4096), "original", &bearer)
        .await;
    let bytes = payload(b'b', 8192);
    let started = fixture.open_session(&bytes, "metadata", &bearer).await;
    fixture
        .chunk(&started, 0, &bytes[..4096], &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let pending: Value = list(&fixture, &bearer, "?status=pending", StatusCode::OK)
        .await
        .json();
    assert_eq!(ids(&pending), vec![untouched]);

    let uploading: Value = list(&fixture, &bearer, "?status=uploading", StatusCode::OK)
        .await
        .json();
    assert_eq!(ids(&uploading), vec![started]);
}

#[tokio::test]
async fn an_unknown_status_filter_is_refused_rather_than_ignored() {
    // The retired handler answered this with the *whole* list, so a caller who typed `complete`
    // for `completed` acted on a list they believed was filtered.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture
        .open_session(&payload(b'a', 4096), "original", &bearer)
        .await;

    let body: Value = list(
        &fixture,
        &bearer,
        "?status=complete",
        StatusCode::BAD_REQUEST,
    )
    .await
    .json();
    assert_eq!(body["code"], "error.upload.invalid_status_filter");
    assert!(
        body["detail"]
            .as_str()
            .is_some_and(|d| d.contains("completed")),
        "the refusal names what was expected: {body}"
    );
}

#[tokio::test]
async fn listing_answers_500_when_the_session_store_cannot() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture.uploads.set_unavailable(true);

    let body: Value = list(&fixture, &bearer, "", StatusCode::INTERNAL_SERVER_ERROR)
        .await
        .json();
    assert_eq!(body["code"], "error.upload.unavailable");
}

#[tokio::test]
async fn the_listing_needs_a_credential() {
    let fixture = Fixture::working();
    fixture
        .client
        .get("/v1/upload/sessions")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_literal_path_is_not_read_as_an_upload_id() {
    // `/v1/upload/sessions` and `/v1/upload/{id}` share a prefix, and a router that preferred
    // the parameter would turn this listing into a lookup for a session called "sessions".
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    list(&fixture, &bearer, "", StatusCode::OK).await;
}
