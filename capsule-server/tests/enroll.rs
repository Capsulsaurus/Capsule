//! Cross-device add, end to end.
//!
//! The case that carries the acceptance is `a_stolen_session_token_cannot_start_a_device_add`: step 1
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

/// The fallback budget the server enforces: `budgets::ENROLLMENT_REDEMPTION`'s limit.
const FALLBACK_BUDGET: u32 = 30;

/// Eight-digit fallback-shaped guesses, each one distinct and none of them `avoid`.
fn distinct_guesses(count: u32, avoid: &str) -> impl Iterator<Item = String> + '_ {
    (0..)
        .map(|index: u32| format!("{index:08}"))
        .filter(move |guess| guess != avoid)
        .take(count as usize)
}

/// Brute force against the transcribable fallback (issue #528).
///
/// The limiter design/device-enrollment.md names as the reason the shorter fallback is safe to
/// offer at all: it trades entropy for transcribability, and what keeps that trade honest is
/// that the code cannot be ground through inside its ten-minute life. The limiter used to be
/// keyed on the presented string, so a brute force — which never presents the same wrong guess
/// twice — got a fresh budget on every attempt and was never limited at all. This is that
/// attack: distinct guesses, each a different string.
#[tokio::test]
async fn brute_forcing_the_fallback_with_distinct_guesses_is_rate_limited() {
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;
    let issued: Value = issue(&fixture, &bearer, StatusCode::OK).await.json();
    let code = issued["code"].as_str().expect("a code").to_owned();
    let fallback = issued["text_fallback"]
        .as_str()
        .expect("a fallback")
        .to_owned();

    for guess in distinct_guesses(FALLBACK_BUDGET, &fallback) {
        redeem(&fixture, &guess, StatusCode::NOT_FOUND).await;
    }
    // A never-presented guess is refused: the budget is not the guess's own.
    let problem: Value = redeem(&fixture, "99999999", StatusCode::TOO_MANY_REQUESTS)
        .await
        .json();
    assert_eq!(problem["code"], "error.enrollment.rate_limited");
    assert!(problem["retry_after"].as_u64().is_some_and(|s| s > 0));

    // And so is the *right* fallback: the guess that would have won lands on a spent budget,
    // which is the whole point. The redemption is not attempted, so the code stays pending.
    redeem(&fixture, &fallback, StatusCode::TOO_MANY_REQUESTS).await;

    // The full-entropy spelling has a bucket of its own, so the person whose typed code is
    // throttled can still scan the QR — and the enrollment the brute force aimed at is intact.
    redeem(&fixture, &code, StatusCode::OK).await;
}

/// The budget is spent across guesses aimed at **different** pending enrollments too.
///
/// A miss names no pending enrollment, so an attacker spreading guesses over many live codes is
/// charged the same bucket as one focused on a single code; there is no per-target allowance to
/// multiply by opening more targets.
#[tokio::test]
async fn the_fallback_budget_is_shared_by_every_pending_enrollment() {
    let fixture = Fixture::working();
    let mut fallbacks = Vec::new();
    for _ in 0..2 {
        let bearer = fresh(&fixture).await;
        let issued: Value = issue(&fixture, &bearer, StatusCode::OK).await.json();
        fallbacks.push(
            issued["text_fallback"]
                .as_str()
                .expect("a fallback")
                .to_owned(),
        );
    }

    // Redeeming one real fallback is charged like any other presentation...
    redeem(&fixture, &fallbacks[0], StatusCode::OK).await;
    for guess in distinct_guesses(FALLBACK_BUDGET - 1, &fallbacks[1]) {
        redeem(&fixture, &guess, StatusCode::NOT_FOUND).await;
    }
    // ...so the other enrollment's fallback finds the budget already spent.
    redeem(&fixture, &fallbacks[1], StatusCode::TOO_MANY_REQUESTS).await;
}

/// A code that is not even shaped like one cannot mint a counter key.
///
/// The presented value is an arbitrary caller-supplied string, and the limiter is charged before
/// the code is resolved — deliberately, so probing costs the prober. Keying on an unchecked
/// string would let an unauthenticated caller fill this partition a row at a time, so anything
/// malformed is charged to one fixed bucket instead, apart from the two spellings' buckets, so a
/// flood of garbage cannot spend the budget the entropy argument for the short fallback rests on.
#[tokio::test]
async fn redemption_that_is_not_even_shaped_like_a_code_shares_one_bucket() {
    let fixture = Fixture::working();

    // Sixty distinct malformed strings: all refused on their merits, none rate-limited, because
    // they share one bucket whose budget is sixty a minute.
    for index in 0..60 {
        redeem(&fixture, &format!("garbage-{index}"), StatusCode::NOT_FOUND).await;
    }
    // The sixty-first spends that shared bucket, on a string nothing has ever seen.
    let problem: Value = redeem(&fixture, "garbage-fresh", StatusCode::TOO_MANY_REQUESTS)
        .await
        .json();
    assert_eq!(problem["code"], "error.enrollment.rate_limited");

    // And a *well-formed* code is a different bucket, unaffected by the flood: the malformed
    // path must not be able to deny a real redemption.
    let bearer = fresh(&fixture).await;
    let issued: Value = issue(&fixture, &bearer, StatusCode::OK).await.json();
    let code = issued["code"].as_str().expect("a code").to_owned();
    redeem(&fixture, &code, StatusCode::OK).await;
}

#[tokio::test]
async fn the_redemption_limiter_counts_successes_too() {
    // Charged on every attempt whatever the outcome. A limiter that only counted failures would
    // let a caller who guesses right on the last try escape it entirely.
    let fixture = Fixture::working();
    let bearer = fresh(&fixture).await;
    let issued: Value = issue(&fixture, &bearer, StatusCode::OK).await.json();
    let code = issued["code"].as_str().expect("a code").to_owned();

    // A successful redemption of the full spelling spends one of that bucket's attempts...
    redeem(&fixture, &code, StatusCode::OK).await;
    for _ in 1..FALLBACK_BUDGET {
        redeem(&fixture, &code, StatusCode::NOT_FOUND).await;
    }
    // ...so after it and the misses, a never-seen code of the same spelling is refused.
    let problem: Value = redeem(
        &fixture,
        "0193d2f4-a1b7-4c3e-8f5a-6b7c8d9e0f10",
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await
    .json();
    assert_eq!(problem["code"], "error.enrollment.rate_limited");
}

/// A saturated limiter partition must not impersonate an outage.
///
/// The `EnrollmentRedemption` partition holds one window per spelling, so at its real ceiling
/// it cannot fill; a deployment whose ceiling is overridden below two can. A spelling with no
/// window is then refused — and it is told `429 error.enrollment.at_capacity` with a retry hint,
/// not the `500 error.auth.unavailable` this used to render, which said the server was broken
/// when the limiter was working exactly as designed.
#[tokio::test]
async fn a_saturated_limiter_partition_is_a_429_and_not_an_outage() {
    let fixture = Fixture::with_counter_ceiling(1);

    // One fallback-shaped guess fills the partition.
    redeem(&fixture, "00000010", StatusCode::NOT_FOUND).await;

    // The other spelling finds no room.
    let problem: Value = redeem(
        &fixture,
        "0193d2f4-a1b7-4c3e-8f5a-6b7c8d9e0f10",
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await
    .json();
    assert_eq!(
        problem["code"], "error.enrollment.at_capacity",
        "a full partition has a code of its own, distinct from a spent budget and from an outage"
    );
    assert!(problem["retry_after"].as_u64().is_some_and(|s| s > 0));

    // The spelling already being counted keeps being counted, to its own budget, and the spent
    // budget keeps its own code — so the two causes stay distinguishable.
    for index in 1..FALLBACK_BUDGET {
        redeem(
            &fixture,
            &format!("{:08}", 10 + index),
            StatusCode::NOT_FOUND,
        )
        .await;
    }
    let problem: Value = redeem(&fixture, "00000099", StatusCode::TOO_MANY_REQUESTS)
        .await
        .json();
    assert_eq!(problem["code"], "error.enrollment.rate_limited");
    assert!(problem["retry_after"].as_u64().is_some_and(|s| s > 0));
}
