//! The second factor (slice `S-C55`), end to end.
//!
//! Two cases carry the slice, and both are about the defect it fixes. `a_confirmed_factor_makes_
//! a_password_alone_insufficient` is the whole point: the retired surface had all four TOTP
//! operations and its login never issued a challenge, so a confirmed factor gated nothing. And
//! `a_code_is_good_once_even_inside_its_own_window` is RFC 6238 §5.2 — a code stays valid for
//! ninety seconds, so "somebody read the six digits over your shoulder" is only defended against
//! by a replay ledger.

mod support;

use capsule_server::auth::totp::{DIGITS, STEP_SECONDS};
use capsule_server::auth::{CHALLENGE_TTL, TotpSecret};
use capsule_server::store::Clock as _;
use jiff::SignedDuration;
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{EMAIL, Fixture, PASSWORD};

/// The code an authenticator app would be showing at the fixture's current time.
///
/// Built from the constants `capsule_server::auth::totp` publishes rather than through a helper
/// on `TotpCodes`, which is deliberate twice over: a server has no business generating codes, and
/// reconstructing SHA-1 / six digits / thirty seconds here checks the interop contract every app
/// assumes instead of trusting one function to agree with itself.
fn app_code(secret: &TotpSecret, at: jiff::Timestamp) -> String {
    let bytes = totp_rs::Secret::Encoded(secret.expose_base32().to_owned())
        .to_bytes()
        .expect("the fixture's secret is base32");
    totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        DIGITS,
        0,
        STEP_SECONDS,
        bytes,
        Some("Capsule".to_owned()),
        String::new(),
    )
    .expect("a usable secret")
    .generate(u64::try_from(at.as_second()).expect("a post-epoch instant"))
}

/// The code the seeded account's authenticator would be showing now.
fn current_code(fixture: &Fixture) -> String {
    let secret = fixture
        .totp
        .secret_of(&support::user())
        .expect("the account is enrolled");
    app_code(&secret, fixture.clock.now())
}

/// Enroll and confirm, leaving the seeded account with an active second factor.
async fn enroll_and_confirm(fixture: &Fixture, bearer: &str) {
    fixture
        .client
        .post("/v1/auth/totp/enroll")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK);
    let code = current_code(fixture);
    fixture
        .client
        .post("/v1/auth/totp/verify-enrollment")
        .header("authorization", bearer)
        .json(&json!({ "totp_code": code }))
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
}

/// Sign in with the password alone and return the raw response.
async fn password_login(fixture: &Fixture) -> kynos::test::TestResponse {
    fixture
        .client
        .post("/v1/auth/login")
        .header("accept", "application/json")
        .json(&json!({ "email": EMAIL, "password": PASSWORD }))
        .send()
        .await
}

// ===========================================================================================
// Enrolling
// ===========================================================================================

#[tokio::test]
async fn enrolling_answers_a_provisioning_uri_and_gates_nothing_yet() {
    // Until a code confirms the secret, sign-in is unchanged — which is what stops a mis-scanned
    // QR code from locking somebody out of their own account.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let body: Value = fixture
        .client
        .post("/v1/auth/totp/enroll")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let uri = body["provisioning_uri"].as_str().expect("a URI");
    assert!(uri.starts_with("otpauth://totp/"));
    assert!(uri.contains("issuer=Capsule"));
    assert!(!fixture.totp.is_active(&support::user()));

    password_login(&fixture).await.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_confirming_code_switches_the_factor_on() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;

    assert!(fixture.totp.is_active(&support::user()));
}

#[tokio::test]
async fn a_wrong_confirming_code_is_403_and_leaves_the_factor_off() {
    // 403 and not 401: the caller is authenticated, and a 401 would send a client to a sign-in
    // its live session does not need.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture
        .client
        .post("/v1/auth/totp/enroll")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);

    let body: Value = fixture
        .client
        .post("/v1/auth/totp/verify-enrollment")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({ "totp_code": "000000" }))
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .json();
    assert_eq!(body["code"], "error.auth.totp_invalid_code");
    assert!(!fixture.totp.is_active(&support::user()));
}

#[tokio::test]
async fn confirming_with_nothing_pending_is_refused() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let body: Value = fixture
        .client
        .post("/v1/auth/totp/verify-enrollment")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({ "totp_code": "000000" }))
        .send()
        .await
        .assert_status(StatusCode::CONFLICT)
        .json();
    assert_eq!(body["code"], "error.auth.totp_not_pending");
}

#[tokio::test]
async fn enrolling_over_an_active_factor_is_refused_and_keeps_the_old_secret() {
    // The refusal exists because the alternative lets a stolen session swap the factor for one
    // the attacker holds, without ever presenting a code.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;
    let secret = fixture.totp.secret_of(&support::user()).expect("enrolled");

    let body: Value = fixture
        .client
        .post("/v1/auth/totp/enroll")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::CONFLICT)
        .json();
    assert_eq!(body["code"], "error.auth.totp_already_active");
    assert_eq!(
        fixture
            .totp
            .secret_of(&support::user())
            .map(|s| s.expose_base32().to_owned()),
        Some(secret.expose_base32().to_owned()),
        "the active secret must survive a refused enroll"
    );
}

#[tokio::test]
async fn a_pending_enrollment_is_replaced_without_ceremony() {
    // Somebody who abandoned a QR code and started again is the ordinary case, and nothing is
    // protecting an unconfirmed secret.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture
        .client
        .post("/v1/auth/totp/enroll")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);
    let first = fixture.totp.secret_of(&support::user()).expect("pending");

    fixture
        .client
        .post("/v1/auth/totp/enroll")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);
    let second = fixture.totp.secret_of(&support::user()).expect("pending");
    assert_ne!(first.expose_base32(), second.expose_base32());
}

// ===========================================================================================
// Signing in
// ===========================================================================================

#[tokio::test]
async fn a_confirmed_factor_makes_a_password_alone_insufficient() {
    // The case this slice exists for. The retired surface answered a token pair here.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;

    let response = password_login(&fixture).await;
    response.assert_status(StatusCode::ACCEPTED);
    let body: Value = response.json();
    assert!(
        body.get("access_token").is_none(),
        "a half-finished sign-in must not carry a session credential: {body}"
    );
    assert!(body["mfa_token"].as_str().is_some());
}

#[tokio::test]
async fn the_challenge_and_a_code_complete_the_sign_in() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;

    let challenge: Value = password_login(&fixture).await.json();
    // A step on, so the confirming code is not the current one.
    fixture.clock.advance(SignedDuration::from_secs(
        i64::try_from(STEP_SECONDS).expect("in range"),
    ));

    let tokens: Value = fixture
        .client
        .post("/v1/auth/login/verify-totp")
        .header("accept", "application/json")
        .json(&json!({
            "mfa_token": challenge["mfa_token"],
            "totp_code": current_code(&fixture),
        }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();

    let access = tokens["access_token"].as_str().expect("an access token");
    fixture
        .client
        .get("/v1/auth/profile")
        .header("authorization", &format!("Bearer {access}"))
        .send()
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn the_confirming_code_cannot_also_complete_a_sign_in() {
    // The confirming code is spent into the replay ledger, so the newest code in the account's
    // history is not left lying around usable.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;
    let spent = current_code(&fixture);

    let challenge: Value = password_login(&fixture).await.json();
    let body: Value = fixture
        .client
        .post("/v1/auth/login/verify-totp")
        .header("accept", "application/json")
        .json(&json!({ "mfa_token": challenge["mfa_token"], "totp_code": spent }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED)
        .json();
    assert_eq!(body["code"], "error.auth.totp_invalid_code");
}

#[tokio::test]
async fn a_code_is_good_once_even_inside_its_own_window() {
    // RFC 6238 §5.2. A code is valid for ninety seconds with drift, so a shoulder-surfed one is
    // defended against only by the ledger — verification alone cannot see the second use.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;
    fixture.clock.advance(SignedDuration::from_secs(
        i64::try_from(STEP_SECONDS).expect("in range"),
    ));
    let code = current_code(&fixture);

    let first: Value = password_login(&fixture).await.json();
    fixture
        .client
        .post("/v1/auth/login/verify-totp")
        .header("accept", "application/json")
        .json(&json!({ "mfa_token": first["mfa_token"], "totp_code": code }))
        .send()
        .await
        .assert_status(StatusCode::OK);

    // The same code, a second later, on a fresh challenge — still inside its window.
    fixture.clock.advance(SignedDuration::from_secs(1));
    let second: Value = password_login(&fixture).await.json();
    fixture
        .client
        .post("/v1/auth/login/verify-totp")
        .header("accept", "application/json")
        .json(&json!({ "mfa_token": second["mfa_token"], "totp_code": code }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_advisory_identifiers_ride_the_completing_request() {
    // The session is opened *here*, so this is where a client's cohort and device belong.
    // Without them a TOTP sign-in lands in the devices view as an unknown, ungrouped device.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;
    fixture.clock.advance(SignedDuration::from_secs(
        i64::try_from(STEP_SECONDS).expect("in range"),
    ));

    let challenge: Value = password_login(&fixture).await.json();
    let tokens: Value = fixture
        .client
        .post("/v1/auth/login/verify-totp")
        .header("accept", "application/json")
        .json(&json!({
            "mfa_token": challenge["mfa_token"],
            "totp_code": current_code(&fixture),
            "cohort_hash": "a-particular-phone",
        }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();

    let devices: Value = fixture
        .client
        .get("/v1/auth/devices")
        .header(
            "authorization",
            &format!(
                "Bearer {}",
                tokens["access_token"].as_str().expect("a token")
            ),
        )
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert!(
        devices.to_string().contains("a-particular-phone"),
        "the cohort the completing request carried must reach the devices view: {devices}"
    );
}

#[tokio::test]
async fn an_access_token_is_not_a_second_factor_challenge() {
    // The kind is an argument to the reader, not a check somebody has to remember — which is the
    // lesson the Salvo refresh handler taught by never checking at all.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;
    let access = bearer.trim_start_matches("Bearer ").to_owned();

    let body: Value = fixture
        .client
        .post("/v1/auth/login/verify-totp")
        .header("accept", "application/json")
        .json(&json!({ "mfa_token": access, "totp_code": current_code(&fixture) }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED)
        .json();
    assert_eq!(body["code"], "error.auth.totp_challenge_invalid");
}

#[tokio::test]
async fn an_expired_challenge_is_refused() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;

    let challenge: Value = password_login(&fixture).await.json();
    fixture
        .clock
        .advance(CHALLENGE_TTL + SignedDuration::from_secs(1));

    let body: Value = fixture
        .client
        .post("/v1/auth/login/verify-totp")
        .header("accept", "application/json")
        .json(&json!({
            "mfa_token": challenge["mfa_token"],
            "totp_code": current_code(&fixture),
        }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED)
        .json();
    assert_eq!(body["code"], "error.auth.totp_challenge_invalid");
}

#[tokio::test]
async fn five_wrong_codes_exhaust_one_challenge_and_not_the_account() {
    // Keyed on the challenge, deliberately: a per-account key would let anyone who knows an
    // address lock its owner out with sign-ins they cannot complete.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;

    let challenge: Value = password_login(&fixture).await.json();
    for _ in 0..5 {
        fixture
            .client
            .post("/v1/auth/login/verify-totp")
            .header("accept", "application/json")
            .json(&json!({ "mfa_token": challenge["mfa_token"], "totp_code": "000000" }))
            .send()
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
    let body: Value = fixture
        .client
        .post("/v1/auth/login/verify-totp")
        .header("accept", "application/json")
        .json(&json!({ "mfa_token": challenge["mfa_token"], "totp_code": "000000" }))
        .send()
        .await
        .assert_status(StatusCode::TOO_MANY_REQUESTS)
        .json();
    assert_eq!(body["code"], "error.auth.rate_limited");

    // A fresh challenge carries a fresh budget, and the correct code still works.
    fixture.clock.advance(SignedDuration::from_secs(
        i64::try_from(STEP_SECONDS).expect("in range"),
    ));
    let second: Value = password_login(&fixture).await.json();
    fixture
        .client
        .post("/v1/auth/login/verify-totp")
        .header("accept", "application/json")
        .json(&json!({
            "mfa_token": second["mfa_token"],
            "totp_code": current_code(&fixture),
        }))
        .send()
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_second_factor_store_outage_fails_the_sign_in_closed() {
    // A sign-in that proceeded because the second-factor store was unreachable is a second
    // factor an attacker turns off by loading that store.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;
    fixture.totp.set_unavailable(true);

    let body: Value = password_login(&fixture)
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .json();
    assert_eq!(body["code"], "error.auth.unavailable");
}

// ===========================================================================================
// Removing
// ===========================================================================================

#[tokio::test]
async fn disabling_needs_a_live_code_and_not_just_a_session() {
    // The whole point of the factor is that a stolen access token is insufficient. A disable
    // that took only a token would let the token turn off the control that makes it so.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;

    let body: Value = fixture
        .client
        .post("/v1/auth/totp/disable")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({ "totp_code": "000000" }))
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN)
        .json();
    assert_eq!(body["code"], "error.auth.totp_invalid_code");
    assert!(fixture.totp.is_active(&support::user()));
}

#[tokio::test]
async fn a_live_code_removes_the_factor_and_the_gate_with_it() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;
    fixture.clock.advance(SignedDuration::from_secs(
        i64::try_from(STEP_SECONDS).expect("in range"),
    ));

    fixture
        .client
        .post("/v1/auth/totp/disable")
        .header("authorization", &bearer)
        .json(&json!({ "totp_code": current_code(&fixture) }))
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    assert!(!fixture.totp.is_active(&support::user()));
    password_login(&fixture).await.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn disabling_what_is_not_enrolled_is_refused() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    let body: Value = fixture
        .client
        .post("/v1/auth/totp/disable")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({ "totp_code": "000000" }))
        .send()
        .await
        .assert_status(StatusCode::CONFLICT)
        .json();
    assert_eq!(body["code"], "error.auth.totp_not_enrolled");
}

#[tokio::test]
async fn a_factor_removed_elsewhere_invalidates_an_outstanding_challenge() {
    // The sign-in the challenge belonged to no longer describes the account. Answered as an
    // expired challenge, and the caller's second attempt will not ask for a code at all.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    enroll_and_confirm(&fixture, &bearer).await;
    fixture.clock.advance(SignedDuration::from_secs(
        i64::try_from(STEP_SECONDS).expect("in range"),
    ));

    let challenge: Value = password_login(&fixture).await.json();
    fixture
        .client
        .post("/v1/auth/totp/disable")
        .header("authorization", &bearer)
        .json(&json!({ "totp_code": current_code(&fixture) }))
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let body: Value = fixture
        .client
        .post("/v1/auth/login/verify-totp")
        .header("accept", "application/json")
        .json(&json!({ "mfa_token": challenge["mfa_token"], "totp_code": "000000" }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED)
        .json();
    assert_eq!(body["code"], "error.auth.totp_challenge_invalid");
    password_login(&fixture).await.assert_status(StatusCode::OK);
}

#[tokio::test]
async fn every_totp_operation_needs_a_credential() {
    let fixture = Fixture::working();
    for path in [
        "/v1/auth/totp/enroll",
        "/v1/auth/totp/verify-enrollment",
        "/v1/auth/totp/disable",
    ] {
        fixture
            .client
            .post(path)
            .json(&json!({ "totp_code": "000000" }))
            .send()
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }
}
