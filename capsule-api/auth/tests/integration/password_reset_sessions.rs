use crate::common::{build_service, setup};
use auth::models::responses::TokenResponse;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use secrecy::ExposeSecret;

// ── helpers ────────────────────────────────────────────────────────────────

async fn register(service: &salvo::Service, email: &str, username: &str) -> TokenResponse {
    let mut res = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": username,
            "name": "Session Test User",
            "email": email,
            "password": "oldpassword123"
        }))
        .send(service)
        .await;
    res.take_json()
        .await
        .expect("Failed to parse token response")
}

// ── tests ──────────────────────────────────────────────────────────────────

/// Completing a password reset revokes all existing sessions.  An old
/// refresh token obtained before the reset must therefore be rejected.
#[tokio::test]
async fn password_reset_revokes_existing_sessions() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let tokens = register(&service, "session_revoke@example.com", "sessionrevoke").await;
    let old_refresh = tokens.refresh_token.expose_secret().to_string();

    // Request a password reset (always returns 200 regardless of email existence).
    let res = TestClient::post("http://localhost/password-reset-request")
        .json(&serde_json::json!({"email": "session_revoke@example.com"}))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    // Retrieve the reset token directly from the DB (no email service in tests).
    let reset_token = service::user::Query::get_password_reset_token_by_email(
        &ctx.db,
        "session_revoke@example.com",
    )
    .await
    .expect("DB query failed")
    .flatten()
    .expect("Reset token must be present after reset request");

    // Confirm the reset with a new password.
    let res = TestClient::post("http://localhost/password-reset")
        .json(&serde_json::json!({
            "token": reset_token,
            "new_password": "newpassword456"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    // The old refresh token must now be rejected — all sessions were revoked.
    let res = TestClient::post("http://localhost/refresh")
        .json(&serde_json::json!({"refresh_token": old_refresh}))
        .send(&service)
        .await;
    assert_ne!(
        res.status_code,
        Some(StatusCode::OK),
        "Old refresh token should be rejected after password reset"
    );
}

/// A second login after a password reset works with the new password and
/// produces valid tokens.
#[tokio::test]
async fn login_succeeds_with_new_password_after_reset() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    register(&service, "session_newpw@example.com", "sessionnewpw").await;

    // Request and confirm a password reset.
    let res = TestClient::post("http://localhost/password-reset-request")
        .json(&serde_json::json!({"email": "session_newpw@example.com"}))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    let reset_token = service::user::Query::get_password_reset_token_by_email(
        &ctx.db,
        "session_newpw@example.com",
    )
    .await
    .expect("DB query failed")
    .flatten()
    .expect("Reset token must be present");

    let res = TestClient::post("http://localhost/password-reset")
        .json(&serde_json::json!({
            "token": reset_token,
            "new_password": "newpassword789"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    // Login with the new password must succeed and return a full token pair.
    let mut res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "session_newpw@example.com",
            "password": "newpassword789"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let new_tokens: TokenResponse = res.take_json().await.expect("Failed to parse tokens");
    assert!(!new_tokens.access_token.expose_secret().is_empty());
    assert!(!new_tokens.refresh_token.expose_secret().is_empty());
}
