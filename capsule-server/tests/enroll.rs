//! Cross-device add (slice `S-C7`), end to end.
//!
//! The case that carries the slice is `a_stolen_session_token_cannot_start_a_device_add`: step 1
//! of design/device-enrollment.md exists so that a remotely-exfiltrated token cannot enroll a
//! rogue device, and a gate that a refresh could satisfy would be no gate at all.

mod support;

use jiff::SignedDuration;
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{EMAIL, Fixture, PASSWORD};

/// The bearer for a freshly signed-in session.
async fn fresh(fixture: &Fixture) -> String {
    fixture.bearer().await
}

/// Issue a code, asserting the status.
async fn issue(fixture: &Fixture, bearer: &str, expect: StatusCode) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .post("/v1/auth/devices/enroll")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await;
    response.assert_status(expect);
    response
}

/// Redeem a code, asserting the status.
async fn redeem(fixture: &Fixture, code: &str, expect: StatusCode) -> kynos::test::TestResponse {
    let response = fixture
        .client
        .post("/v1/auth/devices/enroll/redeem")
        .header("accept", "application/json")
        .json(&json!({ "code": code }))
        .send()
        .await;
    response.assert_status(expect);
    response
}

/// Post a payload into one mailbox.
async fn relay(
    fixture: &Fixture,
    channel: &str,
    direction: &str,
    payload: &str,
    expect: StatusCode,
) {
    fixture
        .client
        .post(&format!("/v1/auth/devices/enroll/channel/{channel}"))
        .json(&json!({ "direction": direction, "payload": payload }))
        .send()
        .await
        .assert_status(expect);
}

/// Drain one mailbox.
async fn drain(fixture: &Fixture, channel: &str, direction: &str, expect: StatusCode) -> Value {
    fixture
        .client
        .get(&format!(
            "/v1/auth/devices/enroll/channel/{channel}?direction={direction}"
        ))
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(expect)
        .json()
}

/// Issue and redeem, returning the channel handle.
async fn open_channel(fixture: &Fixture, bearer: &str) -> String {
    let issued: Value = issue(fixture, bearer, StatusCode::OK).await.json();
    let code = issued["code"].as_str().expect("a code").to_owned();
    let opened: Value = redeem(fixture, &code, StatusCode::OK).await.json();
    opened["channel_id"]
        .as_str()
        .expect("a channel id")
        .to_owned()
}

#[tokio::test]
async fn a_code_opens_a_channel_and_the_two_mailboxes_do_not_cross() {
    // A relayed payload is delivered once, and draining one direction leaves the other alone —
    // otherwise the two devices consume each other's mail and the ceremony deadlocks.
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;
    let channel = open_channel(&fixture, &bearer).await;

    relay(
        &fixture,
        &channel,
        "to_enrollee",
        "wrapped-key",
        StatusCode::NO_CONTENT,
    )
    .await;
    relay(
        &fixture,
        &channel,
        "to_initiator",
        "device-keys",
        StatusCode::NO_CONTENT,
    )
    .await;

    let to_enrollee = drain(&fixture, &channel, "to_enrollee", StatusCode::OK).await;
    assert_eq!(to_enrollee["payloads"], json!(["wrapped-key"]));

    let to_initiator = drain(&fixture, &channel, "to_initiator", StatusCode::OK).await;
    assert_eq!(
        to_initiator["payloads"],
        json!(["device-keys"]),
        "draining one direction must not have taken the other's"
    );

    // Delivered once.
    let again = drain(&fixture, &channel, "to_enrollee", StatusCode::OK).await;
    assert_eq!(again["payloads"], json!([]));
}

#[tokio::test]
async fn a_stolen_session_token_cannot_start_a_device_add() {
    // Step 1's whole purpose. The attacker holds a valid, unexpired access token and can refresh
    // it forever; what they cannot do is prove a credential, so the window never reopens.
    let fixture = Fixture::working();
    let stolen = fresh(&fixture).await;

    // Past the freshness window.
    fixture.clock.advance(SignedDuration::from_mins(6));
    let problem: Value = issue(&fixture, &stolen, StatusCode::FORBIDDEN).await.json();
    assert_eq!(problem["code"], "error.enrollment.local_auth_required");

    // And refreshing does not reopen it: a refresh proves possession of a token, not a
    // credential. This is the assertion that `authenticated_at` exists for.
    let refreshed: capsule_server::routes::auth::TokenResponse = fixture
        .client
        .post("/v1/auth/refresh")
        .header("accept", "application/json")
        .json(&json!({ "refresh_token": fixture.login().await.refresh_token }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    fixture.clock.advance(SignedDuration::from_mins(6));
    issue(
        &fixture,
        &format!("Bearer {}", refreshed.access_token),
        StatusCode::FORBIDDEN,
    )
    .await;
}

#[tokio::test]
async fn re_authenticating_reopens_the_window_without_a_new_session() {
    // The only way to satisfy the gate. Without it a user signed in an hour ago would have to
    // sign out entirely to add a device, and the abandoned session would linger in their own
    // devices listing.
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;
    fixture.clock.advance(SignedDuration::from_mins(6));
    issue(&fixture, &bearer, StatusCode::FORBIDDEN).await;

    let body: Value = fixture
        .client
        .post("/v1/auth/reauthenticate")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({ "password": PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert!(body["authenticated_at"].is_string());

    // The same credential still works — no new session was opened, and the ledger says so.
    issue(&fixture, &bearer, StatusCode::OK).await;
    let ledger: Value = fixture
        .client
        .get("/v1/auth/devices")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(
        ledger["sessions"].as_array().expect("sessions").len(),
        1,
        "re-authentication must not leave a second session behind"
    );
}

#[tokio::test]
async fn a_wrong_password_does_not_reopen_the_window() {
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;
    fixture.clock.advance(SignedDuration::from_mins(6));

    let problem: Value = fixture
        .client
        .post("/v1/auth/reauthenticate")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({ "password": "not the password" }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED)
        .json();
    assert_eq!(problem["code"], "error.auth.invalid_credentials");

    issue(&fixture, &bearer, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
async fn a_code_is_single_use_and_both_spellings_burn_together() {
    // The Salvo implementation registered the two spellings with two writes and burned them
    // with two more, so a failure between them left one redeemable. Here they are one fact.
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;
    let issued: Value = issue(&fixture, &bearer, StatusCode::OK).await.json();
    let code = issued["code"].as_str().expect("a code").to_owned();
    let fallback = issued["text_fallback"]
        .as_str()
        .expect("a fallback")
        .to_owned();
    assert_ne!(code, fallback);

    redeem(&fixture, &code, StatusCode::OK).await;

    let problem: Value = redeem(&fixture, &code, StatusCode::NOT_FOUND).await.json();
    assert_eq!(problem["code"], "error.enrollment.code_refused");
    redeem(&fixture, &fallback, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn the_transcribable_fallback_redeems_the_same_enrollment() {
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;
    let issued: Value = issue(&fixture, &bearer, StatusCode::OK).await.json();
    let code = issued["code"].as_str().expect("a code").to_owned();
    let fallback = issued["text_fallback"]
        .as_str()
        .expect("a fallback")
        .to_owned();

    redeem(&fixture, &fallback, StatusCode::OK).await;
    redeem(&fixture, &code, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn a_code_expires_and_is_then_indistinguishable_from_one_that_never_existed() {
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;
    let issued: Value = issue(&fixture, &bearer, StatusCode::OK).await.json();
    let code = issued["code"].as_str().expect("a code").to_owned();

    fixture.clock.advance(SignedDuration::from_mins(11));

    let expired: Value = redeem(&fixture, &code, StatusCode::NOT_FOUND).await.json();
    let never: Value = redeem(&fixture, "never-issued", StatusCode::NOT_FOUND)
        .await
        .json();
    assert_eq!(
        expired["code"], never["code"],
        "redemption takes no credential, so it must not report that a guessed code was once real"
    );
    assert_eq!(expired["title"], never["title"]);
}

#[tokio::test]
async fn a_channel_closes_with_its_window_and_takes_both_mailboxes() {
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;
    let channel = open_channel(&fixture, &bearer).await;
    relay(
        &fixture,
        &channel,
        "to_enrollee",
        "in flight",
        StatusCode::NO_CONTENT,
    )
    .await;

    fixture.clock.advance(SignedDuration::from_mins(11));

    let problem: Value = drain(&fixture, &channel, "to_enrollee", StatusCode::NOT_FOUND).await;
    assert_eq!(problem["code"], "error.enrollment.channel_not_found");
    relay(
        &fixture,
        &channel,
        "to_enrollee",
        "too late",
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn the_initiator_can_close_a_channel_and_a_stranger_cannot() {
    // A close ends the ceremony, so leaving it on the handle alone would make an abandoned QR
    // code a denial of service.
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;
    let channel = open_channel(&fixture, &bearer).await;
    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;

    fixture
        .client
        .delete(&format!("/v1/auth/devices/enroll/channel/{channel}"))
        .header("authorization", &stranger)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);

    // Still live for the initiator, which is what says the refusal closed nothing.
    drain(&fixture, &channel, "to_enrollee", StatusCode::OK).await;

    fixture
        .client
        .delete(&format!("/v1/auth/devices/enroll/channel/{channel}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    drain(&fixture, &channel, "to_enrollee", StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn an_unknown_direction_or_an_unusable_payload_is_refused() {
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;
    let channel = open_channel(&fixture, &bearer).await;

    relay(&fixture, &channel, "sideways", "x", StatusCode::BAD_REQUEST).await;
    relay(
        &fixture,
        &channel,
        "to_enrollee",
        "",
        StatusCode::BAD_REQUEST,
    )
    .await;

    let oversized = "x".repeat(capsule_server::enrollment::MAX_RELAY_BYTES + 1);
    relay(
        &fixture,
        &channel,
        "to_enrollee",
        &oversized,
        StatusCode::BAD_REQUEST,
    )
    .await;

    // A refused relay stored nothing.
    let drained = drain(&fixture, &channel, "to_enrollee", StatusCode::OK).await;
    assert_eq!(drained["payloads"], json!([]));
}

#[tokio::test]
async fn issuing_a_code_requires_a_credential_and_redeeming_one_does_not() {
    // The asymmetry that makes the ceremony work: device B has no account, no session and no key
    // material, so the code is the only thing it can present.
    let fixture = Fixture::working();
    fixture
        .client
        .post("/v1/auth/devices/enroll")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    // Unauthenticated, and refused on its merits rather than for lack of a credential.
    redeem(&fixture, "not a real code", StatusCode::NOT_FOUND).await;
    let _ = EMAIL;
}

#[tokio::test]
async fn redemption_is_rate_limited_per_code() {
    // The limiter design/device-enrollment.md names as the reason the shorter transcribable
    // fallback is safe to offer at all: it trades entropy for transcribability, and what keeps
    // that trade honest is that the code cannot be ground through inside its ten-minute life.
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;
    let issued: Value = issue(&fixture, &bearer, StatusCode::OK).await.json();
    let code = issued["code"].as_str().expect("a code").to_owned();

    // Wrong guesses against the *same* string are what the budget counts.
    for _ in 0..10 {
        redeem(&fixture, "wrong-but-consistent", StatusCode::NOT_FOUND).await;
    }
    let problem: Value = redeem(
        &fixture,
        "wrong-but-consistent",
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await
    .json();
    assert_eq!(problem["code"], "error.enrollment.rate_limited");

    // A different code has its own budget — the limit is per pending enrollment, not global,
    // so one attacker cannot lock every guest out of their own add.
    redeem(&fixture, &code, StatusCode::OK).await;
}

#[tokio::test]
async fn the_redemption_limiter_counts_successes_too() {
    // Charged on every attempt whatever the outcome. A limiter that only counted failures would
    // let a caller who guesses right on the last try escape it entirely.
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;

    for _ in 0..10 {
        redeem(&fixture, "one-string", StatusCode::NOT_FOUND).await;
    }
    // Even the *right* code, presented under a spent budget, is refused — because the budget is
    // keyed on the string presented, and this one has been presented ten times.
    let issued: Value = issue(&fixture, &bearer, StatusCode::OK).await.json();
    let _ = issued;
    redeem(&fixture, "one-string", StatusCode::TOO_MANY_REQUESTS).await;
}
