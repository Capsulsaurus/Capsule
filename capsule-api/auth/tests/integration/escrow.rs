//! Master-key escrow store/fetch/replace server surface (slice `S-C12`).
//!
//! Covers the two "Done when" cases against a real testcontainer Postgres, using real
//! `capsule-core` wrap types so the round-trip exercises the already-tested passphrase path:
//! - `escrow_round_trips_and_unwraps_with_core`: a real `capsule_core::backup` wrap is stored,
//!   fetched back byte-for-byte, and unwrapped to the original master key with core's tested
//!   `recover_master_key` / `verify_recovery_secret`.
//! - `replace_deletes_prior_blob_which_unwraps_nothing`: after a replace (guided re-wrap), the
//!   fetch returns only the new blob; the prior ciphertext is not retrievable and the old
//!   secret unwraps nothing (single active escrow).

use auth::models::errors::ApiError;
use capsule_core::backup::{VerifyOutcome, recover_master_key, verify_recovery_secret};
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::pwkdf::{self, WrappedSecret};
use capsule_i18n::error_codes;
use salvo::http::StatusCode;
use salvo::test::{RequestBuilder, ResponseExt, TestClient};
use secrecy::ExposeSecret;

use crate::common::{TestContext, build_service, setup};

/// Fast Argon2id params — the production tier table is asserted in core; tests must not pay
/// the 128–512 MiB hashing cost.
fn fast() -> Argon2Params {
    Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    }
}

/// Build the opaque escrow blob exactly as a client does: wrap a 32-byte master key under the
/// recovery passphrase (`capsule_core::backup` wrap format), then serialize to canonical CBOR.
fn escrow_blob(master: &[u8; 32], passphrase: &[u8]) -> Vec<u8> {
    let wrapped = pwkdf::wrap_with(master, passphrase, fast()).expect("wrap");
    capsule_core::cbor::to_canonical_vec(&wrapped).expect("escrow serializes")
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
            "name": "Escrow Tester",
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

fn store(token: &str, bytes: Vec<u8>) -> RequestBuilder {
    TestClient::put("http://localhost/backup/escrow")
        .add_header("Authorization", format!("Bearer {token}"), true)
        .add_header("Content-Type", "application/octet-stream", true)
        .bytes(bytes)
}

fn fetch(token: &str) -> RequestBuilder {
    TestClient::get("http://localhost/backup/escrow").add_header(
        "Authorization",
        format!("Bearer {token}"),
        true,
    )
}

#[tokio::test]
async fn escrow_round_trips_and_unwraps_with_core() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (token, _uid) = register(&ctx, &service, "escrow-rt@example.com", "escrowrt").await;

    let master = [0x42u8; 32];
    let passphrase = b"correct horse battery staple";
    let stored = escrow_blob(&master, passphrase);

    // Store is accepted (single active escrow) with no body.
    let res = store(&token, stored.clone()).send(&service).await;
    assert_eq!(res.status_code, Some(StatusCode::NO_CONTENT));

    // Fetch returns the exact opaque bytes the client stored — the server never re-models them.
    let mut fetched = fetch(&token).send(&service).await;
    assert_eq!(fetched.status_code, Some(StatusCode::OK));
    let bytes = fetched.take_bytes(None).await.expect("bytes").to_vec();
    assert_eq!(
        bytes, stored,
        "fetched escrow bytes must equal stored bytes verbatim"
    );

    // Decode and unwrap via core's already-tested passphrase path: the round-tripped blob
    // recovers the original master key, and the derived-tag verify confirms it.
    let blob: WrappedSecret = capsule_core::cbor::from_slice(&bytes).expect("decode escrow");
    assert_eq!(
        recover_master_key(&blob, passphrase).expect("recover"),
        master,
        "escrow must unwrap to the original master key after the server round-trip"
    );
    assert_eq!(
        verify_recovery_secret(&blob, passphrase, &master),
        VerifyOutcome::Verified
    );
    // A wrong passphrase against the round-tripped blob recovers nothing.
    assert!(recover_master_key(&blob, b"wrong passphrase").is_err());
}

#[tokio::test]
async fn replace_deletes_prior_blob_which_unwraps_nothing() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (token, _uid) = register(&ctx, &service, "escrow-rw@example.com", "escrowrw").await;

    // Enroll an initial escrow, then confirm it is fetchable and unwraps.
    let master = [0x11u8; 32];
    let old_secret = b"old recovery secret one two three";
    let old_blob = escrow_blob(&master, old_secret);
    assert_eq!(
        store(&token, old_blob.clone())
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );

    // Guided re-wrap: mint a fresh secret, re-wrap the *same* master key, and replace the
    // server escrow. Store-or-replace is a single upsert — the old blob is overwritten in the
    // same transaction.
    let new_secret = b"fresh recovery secret four five six";
    let new_blob = escrow_blob(&master, new_secret);
    assert_ne!(
        new_blob, old_blob,
        "re-wrap must produce distinct ciphertext"
    );
    assert_eq!(
        store(&token, new_blob.clone())
            .send(&service)
            .await
            .status_code,
        Some(StatusCode::NO_CONTENT)
    );

    // Fetch returns ONLY the new blob — the prior ciphertext is gone and not retrievable.
    let mut fetched = fetch(&token).send(&service).await;
    assert_eq!(fetched.status_code, Some(StatusCode::OK));
    let bytes = fetched.take_bytes(None).await.expect("bytes").to_vec();
    assert_eq!(
        bytes, new_blob,
        "fetch must yield the new blob after replace"
    );
    assert_ne!(
        bytes, old_blob,
        "the prior ciphertext must not be retrievable after a replace"
    );

    // The single active escrow unwraps with the new secret and rejects the old one — the lost
    // secret reaches nothing.
    let blob: WrappedSecret = capsule_core::cbor::from_slice(&bytes).expect("decode escrow");
    assert_eq!(
        recover_master_key(&blob, new_secret).expect("recover with new secret"),
        master
    );
    assert!(
        recover_master_key(&blob, old_secret).is_err(),
        "the old secret must unwrap nothing after the guided re-wrap"
    );
    assert_eq!(
        verify_recovery_secret(&blob, old_secret, &master),
        VerifyOutcome::NotVerified
    );
}

#[tokio::test]
async fn store_and_fetch_require_authentication() {
    let ctx = setup().await;
    let service = build_service(&ctx);

    // Store without a bearer token is refused 401.
    let res = TestClient::put("http://localhost/backup/escrow")
        .add_header("Content-Type", "application/octet-stream", true)
        .bytes(escrow_blob(&[7u8; 32], b"some recovery secret value here"))
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));

    // Fetch without a bearer token is refused 401.
    let res = TestClient::get("http://localhost/backup/escrow")
        .send(&service)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}

#[tokio::test]
async fn fetch_without_escrow_is_not_found() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (token, _uid) = register(&ctx, &service, "escrow-nf@example.com", "escrownf").await;

    let res = fetch(&token).send(&service).await;
    assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
}

#[tokio::test]
async fn empty_escrow_blob_is_rejected() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (token, _uid) = register(&ctx, &service, "escrow-empty@example.com", "escrowempty").await;

    let mut res = store(&token, Vec::new()).send(&service).await;
    assert_eq!(res.status_code, Some(StatusCode::BAD_REQUEST));
    let err: ApiError = res.take_json().await.expect("error json");
    assert_eq!(err.code.as_deref(), Some(error_codes::ESCROW_MALFORMED));
}

/// Escrow is strictly owner-scoped: one user's stored escrow is never visible to another. Two
/// users each store a distinct blob; each fetches only their own.
#[tokio::test]
async fn escrow_is_owner_scoped() {
    let ctx = setup().await;
    let service = build_service(&ctx);
    let (token_a, _a) = register(&ctx, &service, "escrow-a@example.com", "escrowa").await;
    let (token_b, _b) = register(&ctx, &service, "escrow-b@example.com", "escrowb").await;

    let blob_a = escrow_blob(&[0xAAu8; 32], b"alice recovery secret value one");
    let blob_b = escrow_blob(&[0xBBu8; 32], b"bob recovery secret value two three");
    assert_ne!(blob_a, blob_b);

    store(&token_a, blob_a.clone()).send(&service).await;
    store(&token_b, blob_b.clone()).send(&service).await;

    let mut fa = fetch(&token_a).send(&service).await;
    assert_eq!(fa.take_bytes(None).await.expect("bytes").to_vec(), blob_a);
    let mut fb = fetch(&token_b).send(&service).await;
    assert_eq!(fb.take_bytes(None).await.expect("bytes").to_vec(), blob_b);
}
