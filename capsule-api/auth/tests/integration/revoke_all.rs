//! Global revocation with master-key proof (slice `S-C23`).
//!
//! Exercises the authentication doc's Explicit Revocation item 3 end to end against a real
//! testcontainer Postgres + Valkey:
//!
//! - **Valid proof revokes everything, caller included.** The session ledger is empty
//!   afterwards and the caller's own refresh token no longer works — a global revoke that
//!   spared the caller would not be global.
//! - **No confirmation without proof.** A missing, undecodable, wrongly-keyed, unanchored, or
//!   replayed proof is refused with its stable `error.*` code and revokes *nothing*: the
//!   ledger is asserted intact after every refusal, so there is no partial success for a
//!   client to optimistically mirror.
//! - **Not session-authed.** The revoke endpoint takes no bearer token; the identity-key
//!   signature is the whole credential, which is what stops a stolen session token from
//!   logging the legitimate user out of every device.

use auth::models::errors::ApiError;
use auth::models::responses::{SessionListingResponse, TokenResponse};
use auth::revocation::{RevokeAllProof, signing_bytes};
use capsule_core::crypto::keys::hybrid_sig::HybridSigningKey;
use capsule_core::crypto::keys::{DeviceEntry, DirectoryCore};
use capsule_i18n::error_codes;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use secrecy::ExposeSecret;
use uuid::Uuid;

use crate::common::{TestContext, build_service, setup};

/// The account's identity key. Deterministic seeds so the published directory and the proof
/// are signed by the same key across a test.
fn account_ik() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[11; 32], &[12; 32])
}

/// A different, perfectly valid keypair — an attacker's, not the account's.
fn foreign_ik() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[91; 32], &[92; 32])
}

fn signed_directory_bytes(ik: &HybridSigningKey) -> Vec<u8> {
    let device = HybridSigningKey::from_seed_bytes(&[13; 32], &[14; 32]);
    let directory = DirectoryCore {
        user_id: Uuid::from_u128(7),
        directory_version: 1,
        updated_at: "2026-08-22T00:00:00Z".into(),
        devices: vec![DeviceEntry {
            device_id: Uuid::from_u128(0xD7),
            dsk_public: device.verifying_key(),
            dek_public: None,
            added_at: "2026-08-21T00:00:00Z".into(),
            revoked_at: None,
        }],
    }
    .sign(ik);
    capsule_core::cbor::to_canonical_vec(&directory).expect("directory serializes")
}

async fn register(service: &salvo::Service, email: &str, username: &str) -> TokenResponse {
    let mut res = TestClient::post("http://localhost/register")
        .json(&serde_json::json!({
            "username": username,
            "name": "Revoke All Tester",
            "email": email,
            "password": "password123",
        }))
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CREATED));
    res.take_json().await.expect("token response")
}

async fn login(service: &salvo::Service, email: &str) -> TokenResponse {
    let mut res = TestClient::post("http://localhost/login")
        .json(&serde_json::json!({ "email": email, "password": "password123" }))
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    res.take_json().await.expect("token response")
}

async fn account_id(ctx: &TestContext, email: &str) -> String {
    service::user::Query::find_user_by_email(&ctx.db, email)
        .await
        .expect("db query")
        .expect("user exists")
        .id
}

async fn publish_directory(service: &salvo::Service, access: &str, bytes: Vec<u8>) {
    let res = TestClient::post("http://localhost/devices/directory")
        .add_header("Authorization", format!("Bearer {access}"), true)
        .add_header("Content-Type", "application/cbor", true)
        .bytes(bytes)
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK), "directory published");
}

/// Ask for a single-use challenge with an active session token.
async fn challenge(service: &salvo::Service, access: &str) -> String {
    let mut res = TestClient::post("http://localhost/logout/all/challenge")
        .add_header("Authorization", format!("Bearer {access}"), true)
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body: serde_json::Value = res.take_json().await.expect("challenge body");
    assert!(
        !body["expires_at"].as_str().unwrap_or_default().is_empty(),
        "the challenge carries an expiry"
    );
    assert!(
        !body["user_id"].as_str().unwrap_or_default().is_empty(),
        "the challenge names the account it is bound to, so the client can build the \
         signing input without cracking open its token"
    );
    body["challenge"]
        .as_str()
        .expect("challenge value")
        .to_string()
}

fn proof_bytes(ik: &HybridSigningKey, user_id: &str, challenge: &str) -> Vec<u8> {
    let proof = RevokeAllProof {
        challenge: challenge.to_string(),
        identity_key: ik.verifying_key(),
        signature: ik.sign(&signing_bytes(user_id, challenge)),
    };
    capsule_core::cbor::to_canonical_vec(&proof).expect("proof serializes")
}

/// Post a proof document. Deliberately sends **no** Authorization header: the signature is the
/// whole credential.
async fn post_revoke(service: &salvo::Service, body: Vec<u8>) -> salvo::http::Response {
    TestClient::post("http://localhost/logout/all")
        .add_header("Content-Type", "application/cbor", true)
        .bytes(body)
        .send(service)
        .await
}

async fn session_count(service: &salvo::Service, access: &str) -> usize {
    let mut res = TestClient::get("http://localhost/devices")
        .add_header("Authorization", format!("Bearer {access}"), true)
        .send(service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let listing: SessionListingResponse = res.take_json().await.expect("session listing");
    listing.devices.len()
}

/// Assert a refusal: the documented status, the documented `error.*` code, and — the
/// load-bearing half — that nothing at all was revoked.
async fn assert_refused(
    mut res: salvo::http::Response,
    expected_code: &str,
    service: &salvo::Service,
    access: &str,
    expected_sessions: usize,
) {
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
    let err: ApiError = res.take_json().await.expect("api error body");
    assert_eq!(err.code.as_deref(), Some(expected_code));
    assert_eq!(
        session_count(service, access).await,
        expected_sessions,
        "a refused revoke-all must leave every session in place"
    );
}

// ── the ceremony succeeds ────────────────────────────────────────────────────

/// The doc's success bullet: a valid proof revokes **everything**, the calling session
/// included, and the caller's refresh token stops working with it.
#[tokio::test]
async fn valid_proof_revokes_every_session_including_the_caller() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let email = "revoke_ok@example.com";

    let first = register(&service, email, "revokeok").await;
    let caller = login(&service, email).await;
    let caller_access = caller.access_token.expose_secret().to_string();
    let user_id = account_id(&ctx, email).await;
    let ik = account_ik();

    publish_directory(&service, &caller_access, signed_directory_bytes(&ik)).await;
    assert_eq!(session_count(&service, &caller_access).await, 2);

    let challenge = challenge(&service, &caller_access).await;
    let mut res = post_revoke(&service, proof_bytes(&ik, &user_id, &challenge)).await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body: serde_json::Value = res.take_json().await.expect("revoke body");
    assert_eq!(
        body["revoked_sessions"].as_u64(),
        Some(2),
        "both sessions were counted, the caller's among them"
    );

    // The ledger is empty — the calling session is gone, not exempted.
    assert_eq!(session_count(&service, &caller_access).await, 0);

    // And the caller cannot rotate its way back in: the session backing it is gone.
    let res = TestClient::post("http://localhost/refresh")
        .json(&serde_json::json!({
            "refresh_token": caller.refresh_token.expose_secret(),
        }))
        .send(&service)
        .await;
    assert!(
        res.status_code.is_some_and(|s| s.is_client_error()),
        "the caller's refresh token no longer resolves to a session"
    );

    // The other device's refresh token is equally dead.
    let res = TestClient::post("http://localhost/refresh")
        .json(&serde_json::json!({
            "refresh_token": first.refresh_token.expose_secret(),
        }))
        .send(&service)
        .await;
    assert!(res.status_code.is_some_and(|s| s.is_client_error()));
}

// ── refusal paths: no confirmation without proof ─────────────────────────────

/// An empty body presents no proof at all.
#[tokio::test]
async fn a_missing_proof_is_refused_and_revokes_nothing() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let email = "revoke_missing@example.com";
    let tokens = register(&service, email, "revokemissing").await;
    let access = tokens.access_token.expose_secret().to_string();
    publish_directory(&service, &access, signed_directory_bytes(&account_ik())).await;

    let res = post_revoke(&service, Vec::new()).await;
    assert_refused(
        res,
        error_codes::AUTH_REVOKE_PROOF_REQUIRED,
        &service,
        &access,
        1,
    )
    .await;
}

/// A body that is not a proof document is likewise "no proof presented".
#[tokio::test]
async fn an_undecodable_proof_is_refused_and_revokes_nothing() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let email = "revoke_garbage@example.com";
    let tokens = register(&service, email, "revokegarbage").await;
    let access = tokens.access_token.expose_secret().to_string();
    publish_directory(&service, &access, signed_directory_bytes(&account_ik())).await;

    let res = post_revoke(&service, b"not a proof document".to_vec()).await;
    assert_refused(
        res,
        error_codes::AUTH_REVOKE_PROOF_REQUIRED,
        &service,
        &access,
        1,
    )
    .await;
}

/// A structurally perfect proof signed by a key the account never published: refused, and
/// nothing is revoked. This is the stolen-token escalation the asymmetry exists to stop.
#[tokio::test]
async fn a_proof_from_a_foreign_key_is_refused_and_revokes_nothing() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let email = "revoke_foreign@example.com";
    let tokens = register(&service, email, "revokeforeign").await;
    let access = tokens.access_token.expose_secret().to_string();
    let user_id = account_id(&ctx, email).await;
    publish_directory(&service, &access, signed_directory_bytes(&account_ik())).await;

    let challenge = challenge(&service, &access).await;
    let res = post_revoke(&service, proof_bytes(&foreign_ik(), &user_id, &challenge)).await;
    assert_refused(
        res,
        error_codes::AUTH_REVOKE_PROOF_INVALID,
        &service,
        &access,
        1,
    )
    .await;
}

/// A signature over a challenge the server never issued is refused.
#[tokio::test]
async fn a_forged_challenge_is_refused_and_revokes_nothing() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let email = "revoke_forged@example.com";
    let tokens = register(&service, email, "revokeforged").await;
    let access = tokens.access_token.expose_secret().to_string();
    let user_id = account_id(&ctx, email).await;
    publish_directory(&service, &access, signed_directory_bytes(&account_ik())).await;

    let res = post_revoke(
        &service,
        proof_bytes(&account_ik(), &user_id, "a-challenge-nobody-issued"),
    )
    .await;
    assert_refused(
        res,
        error_codes::AUTH_REVOKE_PROOF_INVALID,
        &service,
        &access,
        1,
    )
    .await;
}

/// The challenge is single-use: a captured proof cannot be replayed to log the user out again.
#[tokio::test]
async fn a_challenge_cannot_be_replayed() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let email = "revoke_replay@example.com";
    register(&service, email, "revokereplay").await;
    let caller = login(&service, email).await;
    let access = caller.access_token.expose_secret().to_string();
    let user_id = account_id(&ctx, email).await;
    let ik = account_ik();
    publish_directory(&service, &access, signed_directory_bytes(&ik)).await;

    let challenge = challenge(&service, &access).await;
    let proof = proof_bytes(&ik, &user_id, &challenge);

    let res = post_revoke(&service, proof.clone()).await;
    assert_eq!(res.status_code, Some(StatusCode::OK));

    // A fresh session, then the same proof again: the challenge is spent.
    let next = login(&service, email).await;
    let next_access = next.access_token.expose_secret().to_string();
    let res = post_revoke(&service, proof).await;
    assert_refused(
        res,
        error_codes::AUTH_REVOKE_PROOF_INVALID,
        &service,
        &next_access,
        1,
    )
    .await;
}

/// With no published device directory there is nothing to anchor the identity key against, so
/// the proof is refused rather than trusted on its own say-so.
#[tokio::test]
async fn an_account_without_a_published_directory_cannot_revoke() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let email = "revoke_unanchored@example.com";
    let tokens = register(&service, email, "revokeunanchored").await;
    let access = tokens.access_token.expose_secret().to_string();
    let user_id = account_id(&ctx, email).await;

    let challenge = challenge(&service, &access).await;
    let res = post_revoke(&service, proof_bytes(&account_ik(), &user_id, &challenge)).await;
    assert_refused(
        res,
        error_codes::AUTH_REVOKE_PROOF_INVALID,
        &service,
        &access,
        1,
    )
    .await;
}

/// Issuing a challenge still needs an active session — it names the account. (Authorizing the
/// revoke does not, which is the asymmetry the doc requires.)
#[tokio::test]
async fn challenge_issuance_requires_an_active_session() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    let res = TestClient::post("http://localhost/logout/all/challenge")
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}
