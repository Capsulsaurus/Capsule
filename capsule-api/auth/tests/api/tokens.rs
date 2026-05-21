use crate::common::{build_service, setup};
use auth::models::responses::TokenResponse;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use secrecy::ExposeSecret;

async fn register_and_login(service: &salvo::Service) -> TokenResponse {
    let _ = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": "testuser",
            "name": "Test User",
            "email": "test@example.com",
            "password": "password123"
        }))
        .send(service)
        .await;

    let mut res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "test@example.com",
            "password": "password123"
        }))
        .send(service)
        .await;
    res.take_json().await.expect("Failed to parse tokens")
}

#[tokio::test]
async fn token_lifecycle() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_login(&service).await;

    assert!(!tokens.access_token.expose_secret().is_empty());
    assert!(!tokens.refresh_token.expose_secret().is_empty());

    // Validate token
    let res = TestClient::post("http://localhost/validate")
        .add_header(
            "Authorization",
            format!("Bearer {}", tokens.access_token.expose_secret()),
            true,
        )
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    // Refresh token
    let mut res = TestClient::post("http://localhost/refresh")
        .json(&serde_json::json!({
            "refresh_token": tokens.refresh_token.expose_secret()
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let new_tokens: TokenResponse = res
        .take_json()
        .await
        .expect("Failed to parse refreshed tokens");
    assert!(!new_tokens.access_token.expose_secret().is_empty());

    // Logout
    let res = TestClient::post("http://localhost/logout")
        .add_header(
            "Authorization",
            format!("Bearer {}", new_tokens.access_token.expose_secret()),
            true,
        )
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    // Refresh with old token should fail after logout (session revoked)
    let res = TestClient::post("http://localhost/refresh")
        .json(&serde_json::json!({
            "refresh_token": new_tokens.refresh_token.expose_secret()
        }))
        .send(&service)
        .await;
    assert_ne!(res.status_code, Some(StatusCode::OK));
}

#[tokio::test]
async fn validate_invalid_token() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let res = TestClient::post("http://localhost/validate")
        .add_header("Authorization", "Bearer invalid.token.here", true)
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}

#[tokio::test]
async fn refresh_with_invalid_token() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let res = TestClient::post("http://localhost/refresh")
        .json(&serde_json::json!({"refresh_token": "not.a.valid.token"}))
        .send(&service)
        .await;
    assert_ne!(res.status_code, Some(StatusCode::OK));
}

#[tokio::test]
async fn get_devices_success() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_login(&service).await;

    let res = TestClient::get("http://localhost/devices")
        .add_header(
            "Authorization",
            format!("Bearer {}", tokens.access_token.expose_secret()),
            true,
        )
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
}

#[tokio::test]
async fn logout_without_token_fails() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let res = TestClient::post("http://localhost/logout")
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}
