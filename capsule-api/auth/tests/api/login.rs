use crate::common::{build_service, setup};
use auth::models::responses::TokenResponse;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use secrecy::ExposeSecret;

async fn register_test_user(service: &salvo::Service) -> TokenResponse {
    let mut res = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": "testuser",
            "name": "Test User",
            "email": "test@example.com",
            "password": "password123"
        }))
        .send(service)
        .await;
    res.take_json().await.expect("Failed to parse tokens")
}

#[tokio::test]
async fn login_user_success() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    register_test_user(&service).await;

    let mut res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "test@example.com",
            "password": "password123"
        }))
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::OK));

    let tokens: TokenResponse = res
        .take_json()
        .await
        .expect("Failed to parse token response");
    assert!(!tokens.access_token.expose_secret().is_empty());
    assert!(!tokens.refresh_token.expose_secret().is_empty());
}

#[tokio::test]
async fn login_user_wrong_password() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    register_test_user(&service).await;

    let res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "test@example.com",
            "password": "wrongpassword"
        }))
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}

#[tokio::test]
async fn login_nonexistent_user() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": "nobody@example.com",
            "password": "password123"
        }))
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}
