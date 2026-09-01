//! The profile surface (slice `S-C54`), end to end.
//!
//! Two cases carry the slice. `a_password_change_ends_every_other_session_and_keeps_this_one` is
//! the whole reason a password change is its own operation: a rotation that left the leaked
//! credential's sessions live would be worthless, and one that signed the caller out of the
//! device they rotated on would be unusable. And
//! `an_absent_display_name_is_not_a_cleared_one` pins the nested-option discipline, because the
//! failure it prevents — a partial update wiping a field nobody sent — is silent.

mod support;

use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{EMAIL, Fixture, PASSWORD};

/// Read the caller's profile and assert the status.
async fn get(fixture: &Fixture, bearer: &str, expect: StatusCode) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .get("/v1/auth/profile")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await;
    response.assert_status(expect);
    response
}

/// Edit the caller's profile and assert the status.
async fn patch(
    fixture: &Fixture,
    bearer: &str,
    body: &Value,
    expect: StatusCode,
) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .patch("/v1/auth/profile")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(body)
        .send()
        .await;
    response.assert_status(expect);
    response
}

/// Change the caller's password and assert the status.
async fn change(
    fixture: &Fixture,
    bearer: &str,
    current: &str,
    new: &str,
    expect: StatusCode,
) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .post("/v1/auth/password")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(&json!({ "current_password": current, "new_password": new }))
        .send()
        .await;
    response.assert_status(expect);
    response
}

// ===========================================================================================
// Reading
// ===========================================================================================

#[tokio::test]
async fn a_profile_is_the_account_the_credential_names() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let body: Value = get(&fixture, &bearer, StatusCode::OK).await.json();
    assert_eq!(body["email"], EMAIL);
    assert_eq!(
        body["user_id"],
        support::user().as_str(),
        "the account comes from the credential, not from anything the caller sent"
    );
    assert!(
        body.get("display_name").is_none(),
        "an unset display name is absent rather than null, so a client's `has a name` test is a \
         key test: {body}"
    );
}

#[tokio::test]
async fn a_profile_carries_nothing_the_server_was_not_told() {
    // Everything a server stores about a person is a thing it can leak or be compelled to
    // produce. This pins the whole of that list, because the failure mode is a field creeping
    // in against a response nobody re-reads.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let body: Value = get(&fixture, &bearer, StatusCode::OK).await.json();
    let mut keys: Vec<&str> = body
        .as_object()
        .expect("a JSON object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["created_at", "email", "user_id"]);
}

#[tokio::test]
async fn a_live_session_naming_a_deleted_account_reads_404_and_not_500() {
    // Reachable with a perfectly valid credential, and the distinction matters: the server is
    // working correctly and the account is gone, which is not an outage.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture.accounts.forget(EMAIL);

    let body: Value = get(&fixture, &bearer, StatusCode::NOT_FOUND).await.json();
    assert_eq!(body["code"], "error.auth.profile_not_found");
}

#[tokio::test]
async fn reading_a_profile_answers_500_when_the_directory_cannot() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture.accounts.set_unavailable(true);

    let body: Value = get(&fixture, &bearer, StatusCode::INTERNAL_SERVER_ERROR)
        .await
        .json();
    assert_eq!(body["code"], "error.auth.unavailable");
    assert!(
        !body.to_string().contains("the double refuses"),
        "a 500 must not leak the collaborator's own words: {body}"
    );
}

#[tokio::test]
async fn a_profile_needs_a_credential() {
    let fixture = Fixture::working();
    fixture
        .client
        .get("/v1/auth/profile")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

// ===========================================================================================
// Editing
// ===========================================================================================

#[tokio::test]
async fn a_display_name_is_set_read_back_and_cleared() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let body: Value = patch(
        &fixture,
        &bearer,
        &json!({ "display_name": "  Ada Lovelace  " }),
        StatusCode::OK,
    )
    .await
    .json();
    assert_eq!(
        body["display_name"], "Ada Lovelace",
        "stored trimmed, and otherwise exactly as it was typed"
    );

    let read: Value = get(&fixture, &bearer, StatusCode::OK).await.json();
    assert_eq!(read["display_name"], "Ada Lovelace");

    // An explicit null is the request to remove it.
    let cleared: Value = patch(
        &fixture,
        &bearer,
        &json!({ "display_name": Value::Null }),
        StatusCode::OK,
    )
    .await
    .json();
    assert!(cleared.get("display_name").is_none());
}

#[tokio::test]
async fn an_absent_display_name_is_not_a_cleared_one() {
    // The nested option earning its keep. With a flat `Option<String>` this case passes an empty
    // body and gets the name wiped, silently — which is the classic partial-update defect.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    patch(
        &fixture,
        &bearer,
        &json!({ "display_name": "Ada Lovelace" }),
        StatusCode::OK,
    )
    .await;

    let body: Value = patch(&fixture, &bearer, &json!({}), StatusCode::OK)
        .await
        .json();
    assert_eq!(
        body["display_name"], "Ada Lovelace",
        "an empty update asked for nothing and must change nothing"
    );
}

#[tokio::test]
async fn a_display_name_past_the_ceiling_is_refused() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let over = "a".repeat(capsule_server::auth::MAX_DISPLAY_NAME_CHARS + 1);

    let body: Value = patch(
        &fixture,
        &bearer,
        &json!({ "display_name": over }),
        StatusCode::BAD_REQUEST,
    )
    .await
    .json();
    assert_eq!(body["code"], "error.auth.profile_invalid");
}

#[tokio::test]
async fn a_display_name_with_a_control_character_is_refused_and_not_stripped() {
    // Silently rewriting what somebody typed is worse than declining it: a name carrying a
    // newline renders as something other than itself in every client that shows it.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let body: Value = patch(
        &fixture,
        &bearer,
        &json!({ "display_name": "Ada\nLovelace" }),
        StatusCode::BAD_REQUEST,
    )
    .await
    .json();
    assert_eq!(body["code"], "error.auth.profile_invalid");

    let read: Value = get(&fixture, &bearer, StatusCode::OK).await.json();
    assert!(read.get("display_name").is_none(), "and nothing was stored");
}

#[tokio::test]
async fn an_email_cannot_be_changed_through_the_profile() {
    // Not a validation rule — there is no field. Moving an account onto an address nobody
    // proved they control is the first step of a takeover, and this server has no way to obtain
    // that proof, so the body is strict and the key does not exist.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    patch(
        &fixture,
        &bearer,
        &json!({ "email": "attacker@example.test" }),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    let read: Value = get(&fixture, &bearer, StatusCode::OK).await.json();
    assert_eq!(read["email"], EMAIL);
}

#[tokio::test]
async fn editing_a_profile_answers_500_when_the_directory_cannot() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture.accounts.set_unavailable(true);

    let body: Value = patch(
        &fixture,
        &bearer,
        &json!({ "display_name": "Ada" }),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await
    .json();
    assert_eq!(body["code"], "error.auth.unavailable");
}

#[tokio::test]
async fn editing_the_profile_of_a_deleted_account_is_404() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture.accounts.forget(EMAIL);

    let body: Value = patch(
        &fixture,
        &bearer,
        &json!({ "display_name": "Ada" }),
        StatusCode::NOT_FOUND,
    )
    .await
    .json();
    assert_eq!(body["code"], "error.auth.profile_not_found");
}

// ===========================================================================================
// Changing a password
// ===========================================================================================

/// A replacement password comfortably over the floor.
const NEW_PASSWORD: &str = "a different correct horse";

#[tokio::test]
async fn a_password_change_ends_every_other_session_and_keeps_this_one() {
    // The case the slice exists for. A rotation that left the leaked credential's sessions live
    // would be worthless; one that signed the caller out of the device they rotated on would be
    // unusable.
    let fixture = Fixture::working();
    let mine = fixture.bearer().await;
    let elsewhere = fixture.bearer().await;

    // Both work before the change.
    get(&fixture, &mine, StatusCode::OK).await;
    get(&fixture, &elsewhere, StatusCode::OK).await;

    change(
        &fixture,
        &mine,
        PASSWORD,
        NEW_PASSWORD,
        StatusCode::NO_CONTENT,
    )
    .await;

    get(&fixture, &mine, StatusCode::OK).await;
    fixture
        .client
        .get("/v1/auth/profile")
        .header("authorization", &elsewhere)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_changed_password_is_the_one_that_signs_in() {
    // Proving the change reached the store rather than only the response.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    change(
        &fixture,
        &bearer,
        PASSWORD,
        NEW_PASSWORD,
        StatusCode::NO_CONTENT,
    )
    .await;
    assert_eq!(
        fixture.accounts.password_of(EMAIL).as_deref(),
        Some(NEW_PASSWORD)
    );

    fixture
        .client
        .post("/v1/auth/login")
        .json(&json!({ "email": EMAIL, "password": PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    fixture
        .client
        .post("/v1/auth/login")
        .json(&json!({ "email": EMAIL, "password": NEW_PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_wrong_current_password_is_403_and_changes_nothing() {
    // 403 and not 401: the caller is authenticated, and a 401 would send a client to a sign-in
    // its live session does not need.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let body: Value = change(
        &fixture,
        &bearer,
        "not the password",
        NEW_PASSWORD,
        StatusCode::FORBIDDEN,
    )
    .await
    .json();
    assert_eq!(body["code"], "error.auth.current_password_invalid");
    assert_eq!(
        fixture.accounts.password_of(EMAIL).as_deref(),
        Some(PASSWORD)
    );
    get(&fixture, &bearer, StatusCode::OK).await;
}

#[tokio::test]
async fn a_new_password_under_the_floor_is_refused_before_anything_is_verified() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let body: Value = change(
        &fixture,
        &bearer,
        PASSWORD,
        "short",
        StatusCode::BAD_REQUEST,
    )
    .await
    .json();
    assert_eq!(body["code"], "error.auth.password_invalid");
    assert_eq!(
        fixture.accounts.password_of(EMAIL).as_deref(),
        Some(PASSWORD)
    );
}

#[tokio::test]
async fn changing_a_password_to_itself_is_refused() {
    // A change that changes nothing leaves the caller believing a leaked credential has been
    // rotated when it has not.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let body: Value = change(
        &fixture,
        &bearer,
        PASSWORD,
        PASSWORD,
        StatusCode::BAD_REQUEST,
    )
    .await
    .json();
    assert_eq!(body["code"], "error.auth.password_invalid");
}

#[tokio::test]
async fn a_locked_account_cannot_change_its_password() {
    // The same directory method decides a sign-in and a change, so the lockout applies to both.
    // A path around it would be a path around the lockout.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture.accounts.lock(EMAIL);

    let body: Value = change(
        &fixture,
        &bearer,
        PASSWORD,
        NEW_PASSWORD,
        StatusCode::LOCKED,
    )
    .await
    .json();
    assert_eq!(body["code"], "error.auth.account_locked");
    assert_eq!(
        fixture.accounts.password_of(EMAIL).as_deref(),
        Some(PASSWORD)
    );
}

#[tokio::test]
async fn changing_a_password_answers_500_when_the_directory_cannot() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture.accounts.set_unavailable(true);

    let body: Value = change(
        &fixture,
        &bearer,
        PASSWORD,
        NEW_PASSWORD,
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await
    .json();
    assert_eq!(body["code"], "error.auth.unavailable");
}

#[tokio::test]
async fn changing_the_password_of_a_deleted_account_is_404() {
    // Reachable only as a deletion landing between the verification and the write, which is
    // exactly what the double stages here.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let response = fixture
        .client
        .post("/v1/auth/password")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({
            "current_password": PASSWORD,
            "new_password": NEW_PASSWORD,
        }));
    fixture.accounts.forget_after_next_authentication();
    let body: Value = response
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .json();
    assert_eq!(body["code"], "error.auth.profile_not_found");
}
