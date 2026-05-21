use crate::common::{build_service, setup};
use auth::models::responses::{Device, TokenResponse};
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use secrecy::ExposeSecret;

// ── helpers ────────────────────────────────────────────────────────────────

async fn register(service: &salvo::Service, email: &str, username: &str) -> TokenResponse {
    let mut res = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": username,
            "name": "Devices Test User",
            "email": email,
            "password": "password123"
        }))
        .send(service)
        .await;
    res.take_json()
        .await
        .expect("Failed to parse token response")
}

async fn login(service: &salvo::Service, email: &str) -> TokenResponse {
    let mut res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({
            "email": email,
            "password": "password123"
        }))
        .send(service)
        .await;
    res.take_json()
        .await
        .expect("Failed to parse token response")
}

// ── tests ──────────────────────────────────────────────────────────────────

/// GET /devices without auth must return 401.
#[tokio::test]
async fn devices_requires_auth() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let res = TestClient::get("http://localhost/devices")
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}

/// After registering (which creates a session), GET /devices must return
/// exactly one device with `is_current: true`.
#[tokio::test]
async fn devices_shows_current_session() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register(&service, "dev_single@example.com", "devsingle").await;
    let access = tokens.access_token.expose_secret().to_string();

    let mut res = TestClient::get("http://localhost/devices")
        .add_header("Authorization", format!("Bearer {}", access), true)
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::OK));
    let devices: Vec<Device> = res.take_json().await.expect("Failed to parse devices");
    assert_eq!(devices.len(), 1, "Should have exactly one active session");
    assert!(
        devices[0].is_current,
        "The only device should be marked as current"
    );
}

/// After registering (session 1) and logging in again (session 2), GET
/// /devices with the login access token must list both sessions and mark
/// only the login session as current.
#[tokio::test]
async fn devices_multiple_sessions_current_flagged_correctly() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    // Session 1: registration
    register(&service, "dev_multi@example.com", "devmulti").await;

    // Session 2: explicit login
    let login_tokens = login(&service, "dev_multi@example.com").await;
    let login_access = login_tokens.access_token.expose_secret().to_string();

    let mut res = TestClient::get("http://localhost/devices")
        .add_header("Authorization", format!("Bearer {}", login_access), true)
        .send(&service)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::OK));
    let devices: Vec<Device> = res.take_json().await.expect("Failed to parse devices");

    assert_eq!(devices.len(), 2, "Both sessions should be listed");

    let current_count = devices.iter().filter(|d| d.is_current).count();
    assert_eq!(
        current_count, 1,
        "Exactly one device should be marked current"
    );

    // All devices must have non-empty IDs.
    for d in &devices {
        assert!(!d.id.is_empty(), "Device ID must not be empty");
    }
}
