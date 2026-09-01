//! `device_id` on the session listing + ceremony cohorts (slice `S-N3`).
//!
//! Closes the [Authentication — Device Cohorts] support-bundle contract on the server side:
//! the dispute path bundles `{cohort_hash, [(device_id, session_id, first_seen, last_seen)]}`,
//! so `GET /devices` must expose **both** identifiers per row, and every login ceremony —
//! not just password login — must accept the cohort that groups the row.
//!
//! - `devices_listing_pairs_device_id_with_session_id`: the wire carries `id` (session) and
//!   `device_id` (device) as separate fields; neither is a rename of the other.
//! - `malformed_device_id_is_indistinguishable_from_absent`: an unusable assertion degrades
//!   to no assertion — it never fails the ceremony and never reaches the listing.
//! - `totp_login_groups_in_the_devices_view`: a TOTP second-factor login asserting a cohort
//!   groups with the password session that shares it, instead of appearing as an unknown
//!   device.
//!
//! The passkey half of the same ceremony contract is a unit test
//! (`routes::passkey::tests`), because a real WebAuthn assertion cannot be minted without an
//! authenticator; the server code path it feeds is the one exercised here for TOTP.
//!
//! [Authentication — Device Cohorts]: https://docs/design/authentication/#device-cohorts

use auth::models::responses::{SessionListingResponse, TokenResponse};
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use secrecy::ExposeSecret;
use totp_rs::{Algorithm, Secret, TOTP};

use crate::common::{TestContext, build_service, setup};

/// A plausible well-formed cohort value (64-char hex, like a SHA-256 digest).
const COHORT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
/// A directory `device_id` — random UUIDv4, the security-bearing identifier space.
const DEVICE_ID: &str = "6f2b1c44-9a7d-4e51-b0c3-1d2e3f4a5b6c";

async fn register(
    service: &salvo::Service,
    email: &str,
    username: &str,
    cohort: Option<&str>,
    device_id: Option<&str>,
) -> TokenResponse {
    let mut body = serde_json::json!({
        "username": username,
        "name": "Ceremony Cohort User",
        "email": email,
        "password": "password123",
    });
    if let Some(c) = cohort {
        body["cohort_hash"] = serde_json::json!(c);
    }
    if let Some(d) = device_id {
        body["device_id"] = serde_json::json!(d);
    }
    let mut res = TestClient::post("http://localhost/register")
        .json(&body)
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CREATED));
    res.take_json().await.expect("token response")
}

async fn login(
    service: &salvo::Service,
    email: &str,
    cohort: Option<&str>,
    device_id: Option<&str>,
) -> (Option<StatusCode>, TokenResponse) {
    let mut body = serde_json::json!({ "email": email, "password": "password123" });
    if let Some(c) = cohort {
        body["cohort_hash"] = serde_json::json!(c);
    }
    if let Some(d) = device_id {
        body["device_id"] = serde_json::json!(d);
    }
    let mut res = TestClient::post("http://localhost/login")
        .json(&body)
        .send(service)
        .await;
    let status = res.status_code;
    let tokens = res.take_json().await.expect("token response");
    (status, tokens)
}

async fn list_sessions(service: &salvo::Service, access: &str) -> SessionListingResponse {
    let mut res = TestClient::get("http://localhost/devices")
        .add_header("Authorization", format!("Bearer {access}"), true)
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    res.take_json().await.expect("session listing")
}

/// The raw listing body, so the *wire* shape (not just the Rust model) is pinned.
async fn list_sessions_json(service: &salvo::Service, access: &str) -> serde_json::Value {
    let mut res = TestClient::get("http://localhost/devices")
        .add_header("Authorization", format!("Bearer {access}"), true)
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    res.take_json().await.expect("session listing json")
}

/// The support bundle needs `(device_id, session_id)` pairs, so the listing must carry both
/// identifiers per row — the session id under its historical `id`, the device id beside it.
#[tokio::test]
async fn devices_listing_pairs_device_id_with_session_id() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register(
        &service,
        "sn3_pair@example.com",
        "sn3pair",
        Some(COHORT),
        Some(DEVICE_ID),
    )
    .await;
    let access = tokens.access_token.expose_secret().to_string();

    let listing = list_sessions(&service, &access).await;
    assert_eq!(listing.devices.len(), 1);
    let row = &listing.devices[0];

    assert!(!row.id.is_empty(), "the session id is present");
    assert_eq!(
        row.device_id.as_deref(),
        Some(DEVICE_ID),
        "the device id is present as its own field"
    );
    assert_ne!(
        row.device_id.as_deref(),
        Some(row.id.as_str()),
        "device id and session id are distinct identifier spaces"
    );
    assert_eq!(row.cohort_hash.as_deref(), Some(COHORT));

    // Everything the support bundle's row needs is on the wire, under stable names.
    let json = list_sessions_json(&service, &access).await;
    let row = &json["devices"][0];
    for field in ["id", "device_id", "created_at", "last_active_at"] {
        assert!(!row[field].is_null(), "`{field}` is on the listing wire");
    }
}

/// A refresh rotates the session; the device id must survive the rotation, or a long-lived
/// client's support bundle would decay to a bare session id.
#[tokio::test]
async fn device_id_survives_token_refresh() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let tokens = register(
        &service,
        "sn3_refresh@example.com",
        "sn3refresh",
        Some(COHORT),
        Some(DEVICE_ID),
    )
    .await;

    let mut res = TestClient::post("http://localhost/refresh")
        .json(&serde_json::json!({
            "refresh_token": tokens.refresh_token.expose_secret(),
        }))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let rotated: TokenResponse = res.take_json().await.expect("token response");

    let listing = list_sessions(&service, rotated.access_token.expose_secret()).await;
    let current = listing
        .devices
        .iter()
        .find(|d| d.is_current)
        .expect("the rotated session is listed");
    assert_eq!(current.device_id.as_deref(), Some(DEVICE_ID));
    assert_eq!(current.cohort_hash.as_deref(), Some(COHORT));
}

/// `device_id` is client-asserted, so an unusable value is dropped rather than refused: the
/// ceremony behaves identically to one that asserted nothing at all.
#[tokio::test]
async fn malformed_device_id_is_indistinguishable_from_absent() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    register(&service, "sn3_bad@example.com", "sn3bad", None, None).await;

    let unusable = [
        "not-a-uuid".to_string(),
        String::new(),
        "00000000-0000-0000-0000-000000000000".to_string(),
        // Over-long garbage: refused as a shape, never buffered into the listing.
        "f".repeat(500),
    ];
    for asserted in &unusable {
        let (status, tokens) = login(&service, "sn3_bad@example.com", None, Some(asserted)).await;
        assert_eq!(
            status,
            Some(StatusCode::OK),
            "login must succeed with device_id {asserted:?}"
        );

        let listing = list_sessions(&service, tokens.access_token.expose_secret()).await;
        let current = listing
            .devices
            .iter()
            .find(|d| d.is_current)
            .expect("current session listed");
        assert_eq!(
            current.device_id, None,
            "an unusable device_id {asserted:?} reaches the listing as absent"
        );
    }
}

/// Enroll TOTP and verify the enrollment so the secret is active; returns the Base32 secret.
async fn enroll_and_verify_totp(service: &salvo::Service, access: &str) -> String {
    let mut res = TestClient::post("http://localhost/totp/enroll")
        .add_header("Authorization", format!("Bearer {access}"), true)
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body: serde_json::Value = res.take_json().await.expect("enroll body");
    let secret = body["provisioning_uri"]
        .as_str()
        .expect("provisioning_uri")
        .split("secret=")
        .nth(1)
        .expect("secret param")
        .split('&')
        .next()
        .expect("secret value")
        .to_string();

    let res = TestClient::post("http://localhost/totp/verify-enrollment")
        .add_header("Authorization", format!("Bearer {access}"), true)
        .json(&serde_json::json!({ "totp_code": current_code(&secret) }))
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    secret
}

fn current_code(secret: &str) -> String {
    TOTP::new(
        Algorithm::SHA1,
        6,
        1,
        30,
        Secret::Encoded(secret.to_string())
            .to_bytes()
            .expect("valid Base32 secret"),
        None,
        String::new(),
    )
    .expect("totp")
    .generate_current()
    .expect("code")
}

/// The TOTP ceremony is a login like any other: the session it opens must carry the cohort
/// and device it asserted, so it groups with the account's other sessions from that device
/// instead of surfacing as an unknown one.
#[tokio::test]
async fn totp_login_groups_in_the_devices_view() {
    let ctx: TestContext = setup().await;
    let service = build_service(&ctx);

    // Session 1: password registration from the cohort's device.
    let tokens = register(
        &service,
        "sn3_totp@example.com",
        "sn3totp",
        Some(COHORT),
        Some(DEVICE_ID),
    )
    .await;
    let access = tokens.access_token.expose_secret().to_string();
    let secret = enroll_and_verify_totp(&service, &access).await;

    let user = service::user::Query::find_user_by_email(&ctx.db, "sn3_totp@example.com")
        .await
        .expect("db query")
        .expect("user exists");
    let mfa_token = ctx
        .app_state
        .auth_service
        .generate_mfa_token(&user.id)
        .expect("mfa token");

    // Session 2: the TOTP second factor, asserting the same cohort and device.
    let mut res = TestClient::post("http://localhost/login/verify-totp")
        .json(&serde_json::json!({
            "mfa_token": mfa_token,
            "totp_code": current_code(&secret),
            "cohort_hash": COHORT,
            "device_id": DEVICE_ID,
        }))
        .send(&service)
        .await;
    assert_eq!(
        res.status_code,
        Some(StatusCode::OK),
        "verify-totp accepts the cohort fields and completes login"
    );
    let totp_tokens: TokenResponse = res.take_json().await.expect("token response");

    let listing = list_sessions(&service, totp_tokens.access_token.expose_secret()).await;
    assert_eq!(listing.devices.len(), 2, "both sessions are listed");
    assert!(
        listing
            .devices
            .iter()
            .all(|d| d.cohort_hash.as_deref() == Some(COHORT)),
        "the TOTP session groups with the password session under one cohort"
    );
    assert!(
        listing
            .devices
            .iter()
            .all(|d| d.device_id.as_deref() == Some(DEVICE_ID)),
        "both sessions name the same device"
    );

    // The durable map has exactly one entry: one cohort, recorded by both ceremonies.
    let cohorts: Vec<_> = listing
        .cohorts
        .iter()
        .filter(|c| c.cohort_hash == COHORT)
        .collect();
    assert_eq!(
        cohorts.len(),
        1,
        "one durable cohort row, not one per login"
    );
}
