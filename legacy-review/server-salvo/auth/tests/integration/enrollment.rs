//! Device-enrollment code + relay-channel server surface (slice `S-C7`).
//!
//! Covers the enrollment doc's code-lifecycle Validation bullets end-to-end against real
//! Postgres + Valkey testcontainers, plus the relay channel and the cross-device-add
//! directory update that reuses S-C9's monotonic publish:
//!
//! - **local-auth gate** — a stale access token cannot issue a code (`403`).
//! - **single-use** — a redeemed code cannot be redeemed again (`404`, indistinguishable).
//! - **expiry** — an expired code is refused and deleted (`404`).
//! - **relay** — opaque payloads pass between the two devices verbatim; unknown channels 404.
//! - **rate-limit** — the per-user issuance budget is enforced (`429`).
//! - **cross-device add** — B's entry lands via the existing `POST /devices/directory`.

use std::time::Duration;

use auth::claims::{Claims, Scope};
use auth::models::errors::ApiError;
use auth::models::responses::TokenResponse;
use auth::roles::UserRole;
use capsule_core::crypto::keys::hybrid_sig::HybridSigningKey;
use capsule_core::crypto::keys::{DeviceEntry, DirectoryCore};
use capsule_i18n::error_codes;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use secrecy::ExposeSecret;
use serde_json::json;
use uuid::Uuid;

use crate::common::{TestContext, build_service, setup};

// ── helpers ──────────────────────────────────────────────────────────────────

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after epoch")
        .as_secs()
}

/// Register a fresh account and return `(fresh_access_token, account_id)`.
async fn register(
    ctx: &TestContext,
    service: &salvo::Service,
    email: &str,
    username: &str,
) -> (String, String) {
    let mut res = TestClient::post("http://localhost/register")
        .json(&json!({
            "username": username,
            "name": "Enrollment Tester",
            "email": email,
            "password": "password123",
        }))
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CREATED));
    let tokens: TokenResponse = res.take_json().await.expect("token response");
    let user = service::user::Query::find_user_by_email(&ctx.db, email)
        .await
        .expect("db query")
        .expect("user exists");
    (tokens.access_token.expose_secret().to_string(), user.id)
}

/// Mint an access token whose `iat` is an hour old — structurally valid (unexpired, right
/// issuer/scope) but far outside the fresh-auth window, so the local-auth gate must refuse it.
fn stale_access_token(ctx: &TestContext, user_id: &str) -> String {
    let now = unix_now();
    let claims = Claims {
        sub: user_id.to_string(),
        exp: now + 3600,
        iat: now - 3600,
        jti: "stale-test-token".to_string(),
        iss: auth::constants::ISSUER.to_string(),
        sid: None,
        role: UserRole::User,
        scopes: vec![Scope::AccessToken],
    };
    claims
        .encode(&ctx.app_state.config.jwt_eddsa_encoding_key)
        .expect("encode stale token")
}

/// POST /devices/enroll with a bearer token; returns the raw response.
async fn issue(service: &salvo::Service, token: &str) -> salvo::http::Response {
    TestClient::post("http://localhost/devices/enroll")
        .add_header("Authorization", format!("Bearer {token}"), true)
        .send(service)
        .await
}

/// Issue a code and return its full-entropy form (asserts 200).
async fn issue_code(service: &salvo::Service, token: &str) -> String {
    let mut res = issue(service, token).await;
    assert_eq!(
        res.status_code,
        Some(StatusCode::OK),
        "issue should succeed"
    );
    let body: serde_json::Value = res.take_json().await.expect("issue json");
    body["code"].as_str().expect("code field").to_string()
}

async fn redeem(service: &salvo::Service, code: &str) -> salvo::http::Response {
    TestClient::post("http://localhost/devices/enroll/redeem")
        .json(&json!({ "code": code }))
        .send(service)
        .await
}

// ── local-auth gate ──────────────────────────────────────────────────────────

#[tokio::test]
async fn issue_requires_authentication() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let res = TestClient::post("http://localhost/devices/enroll")
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}

#[tokio::test]
async fn issue_with_stale_token_is_refused() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (fresh, uid) = register(&ctx, &service, "staleauth@example.com", "staleauth").await;

    // A stale (old-iat) token is refused with the local-auth-required code — a valid session
    // token alone cannot start a cross-device add.
    let stale = stale_access_token(&ctx, &uid);
    let mut res = issue(&service, &stale).await;
    assert_eq!(res.status_code, Some(StatusCode::FORBIDDEN));
    let err: ApiError = res.take_json().await.expect("error json");
    assert_eq!(
        err.code.as_deref(),
        Some(error_codes::ENROLLMENT_LOCAL_AUTH_REQUIRED)
    );

    // The fresh token from registration passes the gate.
    let res = issue(&service, &fresh).await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
}

// ── issue / redeem round trip + single use ───────────────────────────────────

#[tokio::test]
async fn issue_and_redeem_round_trip() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (token, _uid) = register(&ctx, &service, "roundtrip@example.com", "rtenroll").await;

    let code = issue_code(&service, &token).await;
    let mut res = redeem(&service, &code).await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body: serde_json::Value = res.take_json().await.expect("channel json");
    assert!(
        !body["channel_id"].as_str().expect("channel_id").is_empty(),
        "redemption yields an opaque relay-channel handle"
    );
}

#[tokio::test]
async fn redeem_is_single_use() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (token, _uid) = register(&ctx, &service, "singleuse@example.com", "singleuse").await;

    let code = issue_code(&service, &token).await;

    // First redemption succeeds.
    assert_eq!(
        redeem(&service, &code).await.status_code,
        Some(StatusCode::OK)
    );
    // Second redemption of the same code is refused (deleted on redemption).
    let mut res = redeem(&service, &code).await;
    assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
    let err: ApiError = res.take_json().await.expect("error json");
    assert_eq!(
        err.code.as_deref(),
        Some(error_codes::ENROLLMENT_CODE_REFUSED)
    );
}

// ── expiry (short-TTL seam) ──────────────────────────────────────────────────

#[tokio::test]
async fn expired_code_redemption_is_refused() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (_token, uid) = register(&ctx, &service, "expiry@example.com", "expiryenroll").await;

    // Inject a code with a 1-second lifetime directly through the service seam (the HTTP
    // issue path fixes the 10-minute TTL). Its explicit `expires_at` is one second out.
    let issued =
        auth::enrollment::issue(&ctx.app_state.session_manager, &uid, Duration::from_secs(1))
            .await
            .expect("inject short-lived code");

    // Let it lapse (short-TTL: the storage TTL evicts it and/or the explicit expiry fires).
    tokio::time::sleep(Duration::from_millis(1_300)).await;

    let mut res = redeem(&service, &issued.code).await;
    assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
    let err: ApiError = res.take_json().await.expect("error json");
    assert_eq!(
        err.code.as_deref(),
        Some(error_codes::ENROLLMENT_CODE_REFUSED)
    );
}

// ── relay channel ────────────────────────────────────────────────────────────

#[tokio::test]
async fn relay_passes_opaque_messages() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (token, _uid) = register(&ctx, &service, "relay@example.com", "relayenroll").await;

    let code = issue_code(&service, &token).await;
    let mut res = redeem(&service, &code).await;
    let body: serde_json::Value = res.take_json().await.expect("channel json");
    let channel_id = body["channel_id"].as_str().expect("channel_id").to_string();

    // Device A relays an opaque payload toward B.
    let send = TestClient::post(format!(
        "http://localhost/devices/enroll/channel/{channel_id}"
    ))
    .json(&json!({ "to": "b", "payload": "R0VUIG9wYXF1ZQ" }))
    .send(&service)
    .await;
    assert_eq!(send.status_code, Some(StatusCode::NO_CONTENT));

    // Device B drains its mailbox and gets the payload back verbatim.
    let mut recv = TestClient::get(format!(
        "http://localhost/devices/enroll/channel/{channel_id}?to=b"
    ))
    .send(&service)
    .await;
    assert_eq!(recv.status_code, Some(StatusCode::OK));
    let drained: serde_json::Value = recv.take_json().await.expect("recv json");
    assert_eq!(
        drained["messages"],
        json!(["R0VUIG9wYXF1ZQ"]),
        "the server relays the payload opaquely and verbatim"
    );

    // A second drain is empty (messages consumed on read).
    let mut recv2 = TestClient::get(format!(
        "http://localhost/devices/enroll/channel/{channel_id}?to=b"
    ))
    .send(&service)
    .await;
    let drained2: serde_json::Value = recv2.take_json().await.expect("recv json");
    assert_eq!(drained2["messages"], json!([]));
}

#[tokio::test]
async fn relay_on_unknown_channel_is_not_found() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let recv = TestClient::get("http://localhost/devices/enroll/channel/nope?to=a")
        .send(&service)
        .await;
    assert_eq!(recv.status_code, Some(StatusCode::NOT_FOUND));

    let mut send = TestClient::post("http://localhost/devices/enroll/channel/nope")
        .json(&json!({ "to": "a", "payload": "eA" }))
        .send(&service)
        .await;
    assert_eq!(send.status_code, Some(StatusCode::NOT_FOUND));
    let err: ApiError = send.take_json().await.expect("error json");
    assert_eq!(
        err.code.as_deref(),
        Some(error_codes::ENROLLMENT_CHANNEL_NOT_FOUND)
    );
}

// ── issuance rate limit ──────────────────────────────────────────────────────

#[tokio::test]
async fn issue_is_rate_limited_per_user() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (token, _uid) = register(&ctx, &service, "ratelimit@example.com", "rlenroll").await;

    // The per-user budget is MAX_ISSUE_PER_WINDOW; the next request over it is refused 429.
    for _ in 0..auth::enrollment::MAX_ISSUE_PER_WINDOW {
        assert_eq!(
            issue(&service, &token).await.status_code,
            Some(StatusCode::OK)
        );
    }
    let mut res = issue(&service, &token).await;
    assert_eq!(res.status_code, Some(StatusCode::TOO_MANY_REQUESTS));
    let err: ApiError = res.take_json().await.expect("error json");
    assert_eq!(
        err.code.as_deref(),
        Some(error_codes::ENROLLMENT_RATE_LIMITED)
    );
}

// ── cross-device add lands via the S-C9 directory publish ────────────────────

fn test_ik() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32])
}

/// A real `capsule-core`-signed directory at `version` carrying `device_count` entries.
fn signed_directory_bytes(version: u64, device_count: usize, ik: &HybridSigningKey) -> Vec<u8> {
    let devices = (0..device_count)
        .map(|i| {
            let key = HybridSigningKey::from_seed_bytes(&[i as u8 + 1; 32], &[i as u8 + 100; 32]);
            DeviceEntry {
                device_id: Uuid::from_u128(0xD0 + i as u128),
                dsk_public: key.verifying_key(),
                dek_public: None,
                added_at: "2026-07-10T00:00:00Z".into(),
                revoked_at: None,
            }
        })
        .collect();
    let directory = DirectoryCore {
        user_id: Uuid::from_u128(1),
        directory_version: version,
        updated_at: "2026-07-10T00:00:00Z".into(),
        devices,
    }
    .sign(ik);
    capsule_core::cbor::to_canonical_vec(&directory).expect("directory serializes")
}

async fn publish_directory(service: &salvo::Service, token: &str, bytes: Vec<u8>) -> StatusCode {
    TestClient::post("http://localhost/devices/directory")
        .add_header("Authorization", format!("Bearer {token}"), true)
        .add_header("Content-Type", "application/cbor", true)
        .bytes(bytes)
        .send(service)
        .await
        .status_code
        .expect("status")
}

#[tokio::test]
async fn cross_device_add_lands_via_directory_republish() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let ik = test_ik();
    let (token, uid) = register(&ctx, &service, "crossadd@example.com", "crossadd").await;

    // Device A's initial single-device directory.
    assert_eq!(
        publish_directory(&service, &token, signed_directory_bytes(1, 1, &ik)).await,
        StatusCode::OK
    );

    // Run the enrollment ceremony: A issues, B redeems and opens the relay channel, and the
    // two devices exchange opaque messages (the key transfer rides these).
    let code = issue_code(&service, &token).await;
    let mut redeemed = redeem(&service, &code).await;
    let channel_id = redeemed
        .take_json::<serde_json::Value>()
        .await
        .expect("json")["channel_id"]
        .as_str()
        .expect("channel_id")
        .to_string();
    let send = TestClient::post(format!(
        "http://localhost/devices/enroll/channel/{channel_id}"
    ))
    .json(&json!({ "to": "b", "payload": "d3JhcHBlZC1tYXN0ZXIta2V5" }))
    .send(&service)
    .await;
    assert_eq!(send.status_code, Some(StatusCode::NO_CONTENT));

    // Cross-sign & publish: B's new entry lands through the *existing* S-C9 monotonic publish
    // at an advanced directory_version — S-C7 reuses that path, it does not fork it.
    assert_eq!(
        publish_directory(&service, &token, signed_directory_bytes(2, 2, &ik)).await,
        StatusCode::OK
    );

    // The fetched directory now carries both devices at the advanced version.
    let mut fetched = TestClient::get(format!("http://localhost/devices/directory/{uid}"))
        .add_header("Authorization", format!("Bearer {token}"), true)
        .send(&service)
        .await;
    assert_eq!(fetched.status_code, Some(StatusCode::OK));
    let bytes = fetched.take_bytes(None).await.expect("bytes");
    let dir: capsule_core::crypto::keys::DeviceDirectory =
        capsule_core::cbor::from_slice(&bytes).expect("decode directory");
    assert_eq!(dir.core.directory_version, 2);
    assert_eq!(
        dir.core.devices.len(),
        2,
        "the added device is now in the directory"
    );
}
