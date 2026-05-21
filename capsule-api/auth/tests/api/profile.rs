use crate::common::{build_service, setup};
use auth::models::UserProfile;
use auth::models::responses::TokenResponse;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use secrecy::ExposeSecret;

async fn register_and_get_token(service: &salvo::Service) -> TokenResponse {
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
async fn get_user_profile_success() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_get_token(&service).await;

    let mut res = TestClient::get("http://localhost/profile")
        .add_header(
            "Authorization",
            format!("Bearer {}", tokens.access_token.expose_secret()),
            true,
        )
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::OK));

    let profile: UserProfile = res.take_json().await.expect("Failed to parse profile");
    assert_eq!(profile.username, "testuser");
    assert_eq!(profile.email, "test@example.com");
}

#[tokio::test]
async fn get_profile_without_auth_fails() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let res = TestClient::get("http://localhost/profile")
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}

#[tokio::test]
async fn update_user_profile_success() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register_and_get_token(&service).await;

    let res = TestClient::post("http://localhost/profile")
        .add_header(
            "Authorization",
            format!("Bearer {}", tokens.access_token.expose_secret()),
            true,
        )
        .json(&serde_json::json!({"username": "newusername"}))
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::OK));

    let user = service::user::Query::find_user_by_email(&ctx.db, "test@example.com")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.username, "newusername");
}
