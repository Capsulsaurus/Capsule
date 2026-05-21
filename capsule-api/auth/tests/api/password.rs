use crate::common::{build_service, setup};
use salvo::http::StatusCode;
use salvo::test::TestClient;

#[tokio::test]
async fn password_reset_flow() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    // Register user
    let _ = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": "resetuser",
            "name": "Reset User",
            "email": "reset@example.com",
            "password": "oldpassword123"
        }))
        .send(&service)
        .await;

    // Request password reset
    let res = TestClient::post("http://localhost/password-reset-request")
        .json(&serde_json::json!({"email": "reset@example.com"}))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    // Retrieve token from DB
    let reset_token =
        service::user::Query::get_password_reset_token_by_email(&ctx.db, "reset@example.com")
            .await
            .expect("DB query failed")
            .flatten()
            .expect("Should have reset token");

    // Reset password
    let res = TestClient::post("http://localhost/password-reset")
        .json(&serde_json::json!({
            "token": reset_token,
            "new_password": "newpassword456"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    // Login with new password
    let res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "reset@example.com",
            "password": "newpassword456"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    // Old password should no longer work
    let res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "reset@example.com",
            "password": "oldpassword123"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}

#[tokio::test]
async fn password_reset_request_nonexistent_email() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    // Should still return OK (to avoid user enumeration)
    let res = TestClient::post("http://localhost/password-reset-request")
        .json(&serde_json::json!({"email": "nobody@example.com"}))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
}

#[tokio::test]
async fn password_reset_invalid_token() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let res = TestClient::post("http://localhost/password-reset")
        .json(&serde_json::json!({
            "token": "invalid-token-xyz",
            "new_password": "newpassword456"
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
}
