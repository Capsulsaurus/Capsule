use crate::common::{build_service, setup};
use auth::models::responses::TokenResponse;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use secrecy::ExposeSecret;

#[tokio::test]
async fn register_user_success() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let mut res = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": "testuser",
            "name": "Test User",
            "email": "test@example.com",
            "password": "password123"
        }))
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::CREATED));

    let tokens: TokenResponse = res
        .take_json()
        .await
        .expect("Failed to parse token response");
    assert!(!tokens.access_token.expose_secret().is_empty());
    assert!(!tokens.refresh_token.expose_secret().is_empty());

    let user = service::user::Query::find_user_by_email(&ctx.db, "test@example.com")
        .await
        .expect("DB query failed")
        .expect("User should exist");
    assert_eq!(user.username, "testuser");
}

#[tokio::test]
async fn register_user_duplicate_email() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let _ = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": "testuser1",
            "name": "Test User",
            "email": "dupe@example.com",
            "password": "password123"
        }))
        .send(&service)
        .await;

    let res = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": "testuser2",
            "name": "Test User 2",
            "email": "dupe@example.com",
            "password": "password123"
        }))
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::CONFLICT));
}

#[tokio::test]
async fn register_user_duplicate_username() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let _ = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": "sameuser",
            "name": "Test User",
            "email": "first@example.com",
            "password": "password123"
        }))
        .send(&service)
        .await;

    let res = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": "sameuser",
            "name": "Test User 2",
            "email": "second@example.com",
            "password": "password123"
        }))
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::CONFLICT));
}
