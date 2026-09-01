//! Device-directory publish/fetch server surface (slice `S-C9`).
//!
//! Covers the two "Done when" cases against a real testcontainer Postgres:
//! - `invariant_23_*`: a non-advancing or regressing `directory_version` is refused `409`
//!   with the stable `error.directory.version_conflict` code, and a strictly-advancing one
//!   is accepted (the anti-rollback high-water mark).
//! - `publish_fetch_verify_round_trip`: a real `capsule-core`-signed directory published,
//!   fetched back byte-for-byte, and verified against the signing IK.

use auth::models::errors::ApiError;
use capsule_core::crypto::keys::hybrid_sig::HybridSigningKey;
use capsule_core::crypto::keys::{DeviceDirectory, DeviceEntry, DirectoryCore};
use capsule_i18n::error_codes;
use salvo::http::StatusCode;
use salvo::test::{RequestBuilder, ResponseExt, TestClient};
use secrecy::ExposeSecret;
use uuid::Uuid;

use crate::common::{TestContext, build_service, setup};

/// The signing IK used across a test (deterministic seeds so the same key verifies later).
fn test_ik() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
}

/// A real `capsule-core`-signed `DeviceDirectory` at `version`, as canonical CBOR bytes —
/// exactly what a client publishes.
fn signed_directory_bytes(version: u64, ik: &HybridSigningKey) -> Vec<u8> {
    let device = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
    let directory = DirectoryCore {
        user_id: Uuid::from_u128(1),
        directory_version: version,
        updated_at: "2026-07-10T00:00:00Z".into(),
        devices: vec![DeviceEntry {
            device_id: Uuid::from_u128(0xD1),
            dsk_public: device.verifying_key(),
            dek_public: None,
            added_at: "2026-07-09T00:00:00Z".into(),
            revoked_at: None,
        }],
    }
    .sign(ik);
    capsule_core::cbor::to_canonical_vec(&directory).expect("directory serializes")
}

/// Register a fresh user and return `(access_token, account_id)`.
async fn register(
    ctx: &TestContext,
    service: &salvo::Service,
    email: &str,
    username: &str,
) -> (String, String) {
    let mut res = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": username,
            "name": "Directory Tester",
            "email": email,
            "password": "password123",
        }))
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CREATED));
    let tokens: auth::models::responses::TokenResponse =
        res.take_json().await.expect("token response");
    let user = service::user::Query::find_user_by_email(&ctx.db, email)
        .await
        .expect("db query")
        .expect("user exists");
    (tokens.access_token.expose_secret().to_string(), user.id)
}

fn publish(token: &str, bytes: Vec<u8>) -> RequestBuilder {
    TestClient::post("http://localhost/devices/directory")
        .add_header("Authorization", format!("Bearer {token}"), true)
        .add_header("Content-Type", "application/cbor", true)
        .bytes(bytes)
}

#[tokio::test]
async fn invariant_23_rejects_non_advancing_directory_version() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let ik = test_ik();
    let (token, _uid) = register(&ctx, &service, "inv23@example.com", "inv23user").await;

    // First publish at version 2 is accepted and becomes the high-water mark.
    let mut res = publish(&token, signed_directory_bytes(2, &ik))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body: serde_json::Value = res.take_json().await.expect("json");
    assert_eq!(body["directory_version"], 2);

    // A regressing version (1 < 2) is refused 409 with the stable code — the server must not
    // walk the directory back (invariant 23).
    let mut res = publish(&token, signed_directory_bytes(1, &ik))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CONFLICT));
    let err: ApiError = res.take_json().await.expect("error json");
    assert_eq!(
        err.code.as_deref(),
        Some(error_codes::DIRECTORY_VERSION_CONFLICT)
    );

    // A non-advancing (equal) version is likewise refused — strict monotonicity.
    let res = publish(&token, signed_directory_bytes(2, &ik))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CONFLICT));

    // The stored directory is unchanged: fetching still yields version 2.
    let uid = &_uid;
    let mut fetched = TestClient::get(format!("http://localhost/devices/directory/{uid}"))
        .add_header("Authorization", format!("Bearer {token}"), true)
        .send(&service)
        .await;
    assert_eq!(fetched.status_code, Some(StatusCode::OK));
    let bytes = fetched.take_bytes(None).await.expect("bytes");
    let dir: DeviceDirectory = capsule_core::cbor::from_slice(&bytes).expect("decode");
    assert_eq!(dir.core.directory_version, 2);

    // A strictly-advancing version (3 > 2) is accepted and advances the high-water mark.
    let mut res = publish(&token, signed_directory_bytes(3, &ik))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body: serde_json::Value = res.take_json().await.expect("json");
    assert_eq!(body["directory_version"], 3);
}

#[tokio::test]
async fn publish_fetch_verify_round_trip() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let ik = test_ik();
    let (token, uid) = register(&ctx, &service, "roundtrip@example.com", "rtuser").await;

    let published = signed_directory_bytes(5, &ik);

    let res = publish(&token, published.clone()).send(&service).await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    let mut fetched = TestClient::get(format!("http://localhost/devices/directory/{uid}"))
        .add_header("Authorization", format!("Bearer {token}"), true)
        .send(&service)
        .await;
    assert_eq!(fetched.status_code, Some(StatusCode::OK));
    let bytes = fetched.take_bytes(None).await.expect("bytes").to_vec();

    // The server serves the exact signed bytes it received — it never re-models the document.
    assert_eq!(
        bytes, published,
        "fetched bytes must equal published bytes verbatim"
    );

    // The pinned directory verifies against the signing IK end-to-end.
    let directory: DeviceDirectory =
        capsule_core::cbor::from_slice(&bytes).expect("decode signed directory");
    assert!(
        directory.verify(&ik.verifying_key()),
        "fetched directory must verify against the publishing IK"
    );
    assert_eq!(directory.core.directory_version, 5);
    // A different IK must not verify (the signature is real, not a rubber stamp).
    let other = HybridSigningKey::from_seed_bytes(&[9; 32], &[9; 32]);
    assert!(!directory.verify(&other.verifying_key()));
}

#[tokio::test]
async fn publish_rejects_malformed_document() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (token, _uid) = register(&ctx, &service, "malformed@example.com", "malformeduser").await;

    let mut res = publish(&token, b"not a signed directory".to_vec())
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
    let err: ApiError = res.take_json().await.expect("error json");
    assert_eq!(err.code.as_deref(), Some(error_codes::DIRECTORY_MALFORMED));
}

#[tokio::test]
async fn publish_and_fetch_require_authentication() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let ik = test_ik();

    // Publish without a bearer token is refused 401.
    let res = TestClient::post("http://localhost/devices/directory")
        .add_header("Content-Type", "application/cbor", true)
        .bytes(signed_directory_bytes(1, &ik))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));

    // Fetch without a bearer token is refused 401.
    let res = TestClient::get("http://localhost/devices/directory/whoever")
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}

#[tokio::test]
async fn fetch_unknown_user_is_not_found() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (token, _uid) = register(&ctx, &service, "notfound@example.com", "notfounduser").await;

    let res = TestClient::get("http://localhost/devices/directory/nonexistent-user-id")
        .add_header("Authorization", format!("Bearer {token}"), true)
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
}
