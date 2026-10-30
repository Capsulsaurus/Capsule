//! The account activity log, end to end (design/activity-log.md).
//!
//! What the HTTP surface must hold: an act the log covers leaves one event naming the request
//! that caused it, the owner reads their own events newest first, and nobody reads anyone
//! else's.

mod support;

use capsule_server::activity::{ActivityKind, ActivityStore};
use capsule_server::routes::auth::TokenResponse;
use capsule_server::store::{Clock as _, UserId};
use jiff::Timestamp;
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{EMAIL, Fixture, PASSWORD, user};

/// Read the caller's activity log over HTTP.
async fn listing(fixture: &Fixture, bearer: &str, query: &str) -> Value {
    fixture
        .client
        .get(&format!("/v1/auth/activity{query}"))
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json()
}

/// Every event the store holds for `account`, newest first, read past the HTTP surface.
async fn held(fixture: &Fixture, account: &UserId) -> Vec<capsule_server::activity::ActivityEvent> {
    fixture
        .activity
        .recent(account, Timestamp::MIN, 1_000)
        .await
        .expect("the in-memory log reads")
}

/// The kinds of `events`, as the wire spells them.
fn kinds(body: &Value) -> Vec<&str> {
    body["events"]
        .as_array()
        .expect("an events array")
        .iter()
        .map(|event| event["kind"].as_str().expect("a kind"))
        .collect()
}

/// The session id an access token names, read back off the devices listing.
async fn current_session(fixture: &Fixture, bearer: &str) -> String {
    let body: Value = fixture
        .client
        .get("/v1/auth/devices")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    body["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|session| session["current"] == true)
        .and_then(|session| session["session_id"].as_str())
        .expect("the current session is listed")
        .to_owned()
}

#[tokio::test]
async fn a_password_sign_in_is_recorded_with_its_request_and_its_session() {
    let fixture = Fixture::working();
    let response = fixture
        .client
        .post("/v1/auth/login")
        .header("accept", "application/json")
        .json(&json!({ "email": EMAIL, "password": PASSWORD }))
        .send()
        .await;
    response.assert_status(StatusCode::OK);
    let request_id = response
        .header("x-request-id")
        .expect("every response names its request")
        .to_owned();
    let pair: TokenResponse = response.json();
    let bearer = format!("Bearer {}", pair.access_token);

    let body = listing(&fixture, &bearer, "").await;
    let events = body["events"].as_array().expect("events");
    assert_eq!(events.len(), 1, "{body}");
    let event = &events[0];
    assert_eq!(event["kind"], "sign_in.password");
    assert_eq!(event["request_id"], request_id.as_str());
    assert_eq!(
        event["subject"].as_str(),
        Some(current_session(&fixture, &bearer).await.as_str()),
        "the subject is the session the sign-in opened"
    );
    assert!(
        event.get("session_id").is_none(),
        "a sign-in is not acted by a session"
    );
    assert_eq!(event["at"], fixture.clock.now().to_string());
}

#[tokio::test]
async fn the_log_reads_newest_first_and_honours_its_limit() {
    let fixture = Fixture::working();
    let first = fixture.bearer().await;
    let second = fixture.bearer().await;
    let doomed = current_session(&fixture, &first).await;

    fixture
        .client
        .delete(&format!("/v1/auth/devices/{doomed}"))
        .header("authorization", &second)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let body = listing(&fixture, &second, "").await;
    assert_eq!(
        kinds(&body),
        ["session.revoked", "sign_in.password", "sign_in.password"]
    );
    let revoked = &body["events"][0];
    assert_eq!(revoked["subject"], doomed.as_str());
    assert_eq!(
        revoked["session_id"].as_str(),
        Some(current_session(&fixture, &second).await.as_str()),
        "the acting session is the one that revoked"
    );

    let limited = listing(&fixture, &second, "?limit=1").await;
    assert_eq!(kinds(&limited), ["session.revoked"]);
}

#[tokio::test]
async fn a_sign_out_is_recorded_against_the_session_it_closed() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let session = current_session(&fixture, &bearer).await;

    fixture
        .client
        .post("/v1/auth/logout")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let events = held(&fixture, &user()).await;
    let latest = events.first().expect("the sign-out was recorded");
    assert_eq!(latest.kind, ActivityKind::SignOut);
    assert_eq!(latest.subject.as_deref(), Some(session.as_str()));
    assert_eq!(
        latest.session_id.as_ref().map(|id| id.as_str().to_owned()),
        Some(session)
    );
}

/// Present `email` and `password` to the login route, expecting `status`; the request id.
async fn refused_login(
    fixture: &Fixture,
    email: &str,
    password: &str,
    status: StatusCode,
) -> String {
    let response = fixture
        .client
        .post("/v1/auth/login")
        .header("accept", "application/json")
        .json(&json!({ "email": email, "password": password }))
        .send()
        .await;
    response.assert_status(status);
    let request_id = response
        .header("x-request-id")
        .expect("every response names its request")
        .to_owned();
    fixture.activity_settled().await;
    request_id
}

#[tokio::test]
async fn a_wrong_password_is_recorded_against_the_account_it_was_tried_on() {
    let fixture = Fixture::working();
    let request_id = refused_login(
        &fixture,
        EMAIL,
        "not the password",
        StatusCode::UNAUTHORIZED,
    )
    .await;

    let events = held(&fixture, &user()).await;
    assert_eq!(events.len(), 1, "{events:?}");
    let event = &events[0];
    assert_eq!(event.kind, ActivityKind::SignInPasswordRefused);
    assert_eq!(event.kind.as_str(), "sign_in.password_refused");
    assert_eq!(
        event.request_id.as_deref(),
        Some(request_id.as_str()),
        "the event names the request that caused it, though it was written off its path"
    );
    assert_eq!(
        event.session_id, None,
        "a sign-in is not acted by a session"
    );
    assert_eq!(
        event.subject, None,
        "a refused sign-in opened nothing to name"
    );
    assert_eq!(event.at, fixture.clock.now());
}

#[tokio::test]
async fn an_unknown_address_records_nothing() {
    let fixture = Fixture::working();
    refused_login(
        &fixture,
        "nobody@example.test",
        PASSWORD,
        StatusCode::UNAUTHORIZED,
    )
    .await;
    // The fixture holds one account, so its log is every log there is.
    assert!(held(&fixture, &user()).await.is_empty());
}

#[tokio::test]
async fn a_sign_in_to_a_locked_account_is_recorded_against_it() {
    let fixture = Fixture::working();
    fixture.accounts.lock(EMAIL);
    let request_id = refused_login(&fixture, EMAIL, PASSWORD, StatusCode::LOCKED).await;

    let events = held(&fixture, &user()).await;
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0].kind, ActivityKind::SignInLocked);
    assert_eq!(events[0].kind.as_str(), "sign_in.locked");
    assert_eq!(events[0].request_id.as_deref(), Some(request_id.as_str()));
    assert_eq!(events[0].subject, None);
}

#[tokio::test]
async fn a_refused_sign_in_is_answered_the_same_when_the_log_is_down() {
    let fixture = Fixture::working();
    fixture.activity.set_unavailable(true);
    refused_login(
        &fixture,
        EMAIL,
        "not the password",
        StatusCode::UNAUTHORIZED,
    )
    .await;
    fixture.activity.set_unavailable(false);
    assert!(held(&fixture, &user()).await.is_empty());
}

#[tokio::test]
async fn a_refused_sign_in_is_answered_without_waiting_for_its_write() {
    let fixture = Fixture::working();
    // A write that never lands: a login that awaited it would never answer, so the bound below
    // is generous and still decides the case — no answer within it means the route waited.
    fixture.activity.stall_writes();
    let login = |password: &'static str| {
        tokio::time::timeout(
            std::time::Duration::from_secs(30),
            fixture
                .client
                .post("/v1/auth/login")
                .header("accept", "application/json")
                .json(&json!({ "email": EMAIL, "password": password }))
                .send(),
        )
    };

    login("not the password")
        .await
        .expect("a wrong password is answered while its write is pending")
        .assert_status(StatusCode::UNAUTHORIZED);

    fixture.accounts.lock(EMAIL);
    login(PASSWORD)
        .await
        .expect("a locked account is answered while its write is pending")
        .assert_status(StatusCode::LOCKED);
}

#[tokio::test]
async fn an_account_reads_only_its_own_log() {
    let fixture = Fixture::working();
    let _owner = fixture.bearer().await;
    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000aa")
        .await;

    let body = listing(&fixture, &stranger, "").await;
    assert!(kinds(&body).is_empty(), "{body}");
    assert_eq!(held(&fixture, &user()).await.len(), 1);
}

#[tokio::test]
async fn the_listing_needs_a_session() {
    let fixture = Fixture::working();
    fixture
        .client
        .get("/v1/auth/activity")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
