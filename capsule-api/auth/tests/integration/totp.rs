use crate::common::{build_service, setup};
use auth::models::responses::TokenResponse;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use secrecy::ExposeSecret;
use totp_rs::{Algorithm, Secret, TOTP};

// ── helpers ────────────────────────────────────────────────────────────────

/// Register a user and return the access token from the response.
async fn register_and_login(
    service: &salvo::Service,
    email: &str,
    username: &str,
) -> TokenResponse {
    let mut res = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": username,
            "name": "TOTP Test User",
            "email": email,
            "password": "password123"
        }))
        .send(service)
        .await;
    res.take_json()
        .await
        .expect("Failed to parse token response")
}

/// Call POST /totp/enroll, assert 200, and return the provisioning URI.
async fn enroll_totp(service: &salvo::Service, access_token: &str) -> String {
    let mut res = TestClient::post("http://localhost/totp/enroll")
        .add_header("Authorization", format!("Bearer {}", access_token), true)
        .send(service)
        .await;
    assert_eq!(
        res.status_code,
        Some(StatusCode::OK),
        "TOTP enroll should succeed"
    );
    let body: serde_json::Value = res.take_json().await.expect("Failed to parse enroll body");
    body["provisioning_uri"]
        .as_str()
        .expect("missing provisioning_uri")
        .to_string()
}

/// Extract the Base32 secret embedded in a provisioning URI.
///
/// URI format: `otpauth://totp/{issuer}:{email}?secret={SECRET}&issuer={issuer}`
fn secret_from_uri(uri: &str) -> String {
    uri.split("secret=")
        .nth(1)
        .expect("secret param not found in URI")
        .split('&')
        .next()
        .expect("malformed URI")
        .to_string()
}

/// Generate the current 6-digit TOTP code for a Base32-encoded secret.
fn current_totp_code(secret: &str) -> String {
    let totp = TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        Secret::Encoded(secret.to_string())
            .to_bytes()
            .expect("invalid Base32 secret"),
        None,
        String::new(),
    )
    .expect("failed to build TOTP");
    totp.generate_current().expect("failed to generate code")
}

/// Enroll TOTP and verify enrollment so the secret is marked as active.
/// Returns the Base32 secret.
async fn enroll_and_verify(service: &salvo::Service, access_token: &str) -> String {
    let uri = enroll_totp(service, access_token).await;
    let secret = secret_from_uri(&uri);
    let code = current_totp_code(&secret);

    let res = TestClient::post("http://localhost/totp/verify-enrollment")
        .add_header("Authorization", format!("Bearer {}", access_token), true)
        .json(&serde_json::json!({"totp_code": code}))
        .send(service)
        .await;
    assert_eq!(
        res.status_code,
        Some(StatusCode::OK),
        "TOTP verify-enrollment should succeed with a valid code"
    );
    secret
}

// ── enrollment ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn totp_enroll_returns_provisioning_uri() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_login(&service, "totp_enroll@example.com", "totpenroll").await;

    let mut res = TestClient::post("http://localhost/totp/enroll")
        .add_header(
            "Authorization",
            format!("Bearer {}", tokens.access_token.expose_secret()),
            true,
        )
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body: serde_json::Value = res.take_json().await.unwrap();
    let uri = body["provisioning_uri"]
        .as_str()
        .expect("missing provisioning_uri");
    assert!(
        uri.starts_with("otpauth://totp/"),
        "URI should be an otpauth URI"
    );
    assert!(uri.contains("secret="), "URI must contain a secret");
    assert!(
        uri.contains("Capsule-Test"),
        "URI must contain the configured issuer"
    );
}

#[tokio::test]
async fn totp_enroll_requires_auth() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let res = TestClient::post("http://localhost/totp/enroll")
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}

#[tokio::test]
async fn totp_enroll_twice_returns_conflict() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_login(&service, "totp_dupe@example.com", "totpdupe").await;
    let access = tokens.access_token.expose_secret().to_string();

    enroll_totp(&service, &access).await;

    // Second enroll attempt on the same account must be rejected.
    let res = TestClient::post("http://localhost/totp/enroll")
        .add_header("Authorization", format!("Bearer {}", access), true)
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CONFLICT));
}

// ── verify-enrollment ──────────────────────────────────────────────────────

#[tokio::test]
async fn totp_verify_enrollment_valid_code_succeeds() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_login(&service, "totp_verify_ok@example.com", "totpverifyok").await;
    let access = tokens.access_token.expose_secret().to_string();

    let uri = enroll_totp(&service, &access).await;
    let secret = secret_from_uri(&uri);
    let code = current_totp_code(&secret);

    let res = TestClient::post("http://localhost/totp/verify-enrollment")
        .add_header("Authorization", format!("Bearer {}", access), true)
        .json(&serde_json::json!({"totp_code": code}))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
}

#[tokio::test]
async fn totp_verify_enrollment_invalid_code_returns_bad_request() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_login(&service, "totp_verify_bad@example.com", "totpverifybad").await;
    let access = tokens.access_token.expose_secret().to_string();

    enroll_totp(&service, &access).await;

    // "000000" is an invalid TOTP code with overwhelming probability.
    let res = TestClient::post("http://localhost/totp/verify-enrollment")
        .add_header("Authorization", format!("Bearer {}", access), true)
        .json(&serde_json::json!({"totp_code": "000000"}))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
}

#[tokio::test]
async fn totp_verify_enrollment_without_enrolling_returns_bad_request() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_login(&service, "totp_no_enroll@example.com", "totpnoenroll").await;
    let access = tokens.access_token.expose_secret().to_string();

    // No /totp/enroll call — there is no secret to verify against.
    let res = TestClient::post("http://localhost/totp/verify-enrollment")
        .add_header("Authorization", format!("Bearer {}", access), true)
        .json(&serde_json::json!({"totp_code": "123456"}))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
}

// ── disable ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn totp_disable_with_valid_code_succeeds() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_login(&service, "totp_dis@example.com", "totpdis").await;
    let access = tokens.access_token.expose_secret().to_string();

    let secret = enroll_and_verify(&service, &access).await;
    let code = current_totp_code(&secret);

    let res = TestClient::post("http://localhost/totp/disable")
        .add_header("Authorization", format!("Bearer {}", access), true)
        .json(&serde_json::json!({"totp_code": code}))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    // After disabling, re-enrolling must succeed (not 409).
    let res = TestClient::post("http://localhost/totp/enroll")
        .add_header("Authorization", format!("Bearer {}", access), true)
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
}

#[tokio::test]
async fn totp_disable_with_invalid_code_fails() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_login(&service, "totp_dis_bad@example.com", "totpdisbad").await;
    let access = tokens.access_token.expose_secret().to_string();

    enroll_and_verify(&service, &access).await;

    let res = TestClient::post("http://localhost/totp/disable")
        .add_header("Authorization", format!("Bearer {}", access), true)
        .json(&serde_json::json!({"totp_code": "000000"}))
        .send(&service)
        .await;
    // Invalid code is a 4xx — exact code depends on the TotpDisableResponses writer.
    assert!(
        res.status_code
            .map(|s| s.is_client_error())
            .unwrap_or(false),
        "disable with wrong code should be a 4xx"
    );
}

// ── verify-totp login ──────────────────────────────────────────────────────

/// A garbage string as mfa_token must return 401 (not 500 or 200).
#[tokio::test]
async fn totp_verify_login_invalid_mfa_token() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let res = TestClient::post("http://localhost/login/verify-totp")
        .json(&serde_json::json!({
            "mfa_token": "this.is.not.a.valid.jwt",
            "totp_code": "123456"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}

/// A legitimate MFA token combined with a wrong TOTP code returns 403.
#[tokio::test]
async fn totp_verify_login_wrong_code_returns_forbidden() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_login(&service, "totp_wrong@example.com", "totpwrong").await;
    let access = tokens.access_token.expose_secret().to_string();

    // Enroll and verify TOTP so the user has an active secret.
    enroll_and_verify(&service, &access).await;

    // Retrieve the user's ID from the DB to generate the MFA token directly.
    let user = service::user::Query::find_user_by_email(&ctx.db, "totp_wrong@example.com")
        .await
        .expect("DB query failed")
        .expect("user should exist");

    let mfa_token = ctx
        .app_state
        .auth_service
        .generate_mfa_token(&user.id)
        .expect("failed to generate MFA token");

    let res = TestClient::post("http://localhost/login/verify-totp")
        .json(&serde_json::json!({
            "mfa_token": mfa_token,
            "totp_code": "000000"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::FORBIDDEN));
}

/// After 3 wrong TOTP codes the 4th attempt returns 429 (max attempts
/// exceeded), regardless of whether the code is correct.
#[tokio::test]
async fn totp_verify_login_max_attempts_exceeded() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_login(&service, "totp_maxattempts@example.com", "totpmax").await;
    let access = tokens.access_token.expose_secret().to_string();

    enroll_and_verify(&service, &access).await;

    let user = service::user::Query::find_user_by_email(&ctx.db, "totp_maxattempts@example.com")
        .await
        .expect("DB query failed")
        .expect("user should exist");

    // All 4 requests use the same MFA token so the attempt counter
    // accumulates on the same JTI.
    let mfa_token = ctx
        .app_state
        .auth_service
        .generate_mfa_token(&user.id)
        .expect("failed to generate MFA token");

    // 3 wrong codes — each increments the attempt counter (0→1, 1→2, 2→3).
    for attempt in 1..=3 {
        let res = TestClient::post("http://localhost/login/verify-totp")
            .json(&serde_json::json!({
                "mfa_token": mfa_token,
                "totp_code": "000000"
            }))
            .send(&service)
            .await;
        assert_eq!(
            res.status_code,
            Some(StatusCode::FORBIDDEN),
            "attempt {} should be FORBIDDEN (wrong code, not yet locked)",
            attempt
        );
    }

    // 4th attempt — counter is now 3 which satisfies `>= 3`, so locked out.
    let res = TestClient::post("http://localhost/login/verify-totp")
        .json(&serde_json::json!({
            "mfa_token": mfa_token,
            "totp_code": "000000"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::TOO_MANY_REQUESTS));
}

/// A valid MFA token + correct TOTP code must return 200 with a full token
/// pair (access + refresh).
#[tokio::test]
async fn totp_verify_login_success() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_login(&service, "totp_ok@example.com", "totpok").await;
    let access = tokens.access_token.expose_secret().to_string();

    let secret = enroll_and_verify(&service, &access).await;

    let user = service::user::Query::find_user_by_email(&ctx.db, "totp_ok@example.com")
        .await
        .expect("DB query failed")
        .expect("user should exist");

    let mfa_token = ctx
        .app_state
        .auth_service
        .generate_mfa_token(&user.id)
        .expect("failed to generate MFA token");

    let code = current_totp_code(&secret);

    let mut res = TestClient::post("http://localhost/login/verify-totp")
        .json(&serde_json::json!({
            "mfa_token": mfa_token,
            "totp_code": code
        }))
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::OK));
    let full_tokens: TokenResponse = res
        .take_json()
        .await
        .expect("Failed to parse token response");
    assert!(!full_tokens.access_token.expose_secret().is_empty());
    assert!(!full_tokens.refresh_token.expose_secret().is_empty());
}
