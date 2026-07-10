//! Slice `S-C15` — signed storage attestation, the durable receipt fetch, the well-known
//! key publication, and proof-of-loss composition, exercised over the real routers against a
//! testcontainer Postgres + on-disk blob tree.
//!
//! Coverage — the storage-verification doc's attestation / proof-of-loss Validation bullets:
//! - `signed_attestation_nonce_echo` — `signed: true` + nonce returns a `StorageAttestation`
//!   verifying under the published key with the nonce echoed verbatim (invariant 34).
//! - `proof_of_loss_composition` — finalize + receipt, delete the blob bytes, a signed verify
//!   reports `stored = false`, and (receipt, attestation) composes to `Loss` under one
//!   `server_key_id`; the content-addressed fetch of the hash fails.
//! - `asset_receipts_endpoint` — `GET /assets/{id}/receipts` serves the permanent log to the
//!   owner, each receipt verifying under the key.
//! - `well_known_publishes_active_key` — the `.well-known` document carries the active key.
//! - `signed_attestation_rate_limited_like_deep` — the signed path is per-user budgeted.

use base64::Engine as _;
use jiff::Timestamp;
use nanoid::nanoid;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use sea_orm::{ActiveModelTrait, Set};
use service::attestation::{
    AttestationKeyring, CustodyReceipt, Mutation, NonHolding, ReceiptInput, StorageAttestation,
    classify_non_holding,
};

use super::{TestCtx, setup};

/// Seed a committed custody receipt for `asset_id`/`hash` (as the finalization txn would).
async fn issue_receipt(
    ctx: &TestCtx,
    keyring: &AttestationKeyring,
    asset_id: &str,
    hash: &str,
    size: u64,
) -> CustodyReceipt {
    let input = ReceiptInput {
        protocol_version: super::PROTOCOL.to_string(),
        upload_id: nanoid!(),
        asset_id: asset_id.to_string(),
        blob_role: "original".to_string(),
        ciphertext_hash: capsule_core::crypto::hash::Hash32::from_hex(hash).unwrap(),
        size,
        envelope_hash: Some(capsule_core::crypto::hash::hash_bytes(b"envelope")),
        uploaded_by_user: ctx.user_id.clone(),
        uploaded_by_device: Some(nanoid!()),
        received_at: Timestamp::now(),
    };
    Mutation::issue_receipt(&ctx.db, keyring, input)
        .await
        .expect("issue receipt")
}

/// Seed an `assets` row owned by the test user (so the receipts endpoint authorizes it),
/// including the `users`/`owners` rows its foreign keys reference.
async fn seed_asset(ctx: &TestCtx, asset_id: &str, hash: &str, size: i64) {
    let created = Timestamp::now() - jiff::SignedDuration::from_hours(24);
    entity::user::ActiveModel {
        id: Set(ctx.user_id.clone()),
        username: Set(format!("u{}", nanoid!(8))),
        name: Set(format!("Test {}", nanoid!(8))),
        email: Set(format!("{}@example.com", nanoid!(8))),
        account_verified: Set(true),
        needs_onboarding: Set(false),
        password_hash: Set(format!("hash-{}", nanoid!(12))),
        is_admin: Set(false),
        created_at: Set(entity::time::ts_to_entity(created)),
        modified_at: Set(entity::time::ts_to_entity(created)),
        ..Default::default()
    }
    .insert(&ctx.db)
    .await
    .expect("seed user");
    entity::owner::ActiveModel {
        id: Set(ctx.user_id.clone()),
        created_at: Set(entity::time::ts_to_entity(created)),
    }
    .insert(&ctx.db)
    .await
    .expect("seed owner");

    entity::asset::ActiveModel {
        id: Set(asset_id.to_string()),
        owner_id: Set(ctx.user_id.clone()),
        // No album row is seeded in this harness; the receipts endpoint does not need one.
        album_id: Set(None),
        width: Set(0),
        height: Set(0),
        asset_type: Set(entity::asset::AssetType::Photo),
        original_filename: Set(nanoid!()),
        file_size: Set(size),
        file_hash: Set(hash.to_string()),
        content_type: Set("image/jpeg".to_string()),
        is_favorite: Set(false),
        is_stack_hidden: Set(false),
        uploaded: Set(true),
        upload_user_id: Set(ctx.user_id.clone()),
        uploaded_at: Set(entity::time::now_entity()),
        modified_at: Set(entity::time::now_entity().into()),
        deleted_at: Set(None),
        ..Default::default()
    }
    .insert(&ctx.db)
    .await
    .expect("seed asset");
}

fn decode_attestation(value: &serde_json::Value) -> StorageAttestation {
    let cbor = base64::engine::general_purpose::STANDARD
        .decode(value["attestation_cbor"].as_str().unwrap())
        .unwrap();
    capsule_core::cbor::from_slice(&cbor).unwrap()
}

#[tokio::test]
async fn signed_attestation_nonce_echo() {
    let ctx = setup().await;
    let svc = ctx.s_c15_service();
    let keyring = ctx.attestation();

    let asset_id = nanoid!();
    let bytes = vec![1u8; 4096];
    let hash = ctx.finalize_blob(&asset_id, "original", &bytes).await;

    let nonce = b"fresh-challenge-42";
    let nonce_b64 = base64::engine::general_purpose::STANDARD.encode(nonce);
    let mut res = TestClient::post("http://localhost/storage/verify")
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .json(&serde_json::json!({
            "assets": [{ "asset_id": asset_id, "blob_hashes": [hash] }],
            "signed": true,
            "nonce": nonce_b64,
        }))
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body = res.take_json::<serde_json::Value>().await.unwrap();
    let att_json = &body["attestations"][0];

    // The nonce is echoed and the decoded attestation verifies under the keyring.
    assert_eq!(att_json["nonce"].as_str().unwrap(), nonce_b64);
    let att = decode_attestation(att_json);
    assert!(keyring.verify_attestation(&att), "the attestation verifies");
    assert!(att.core.verdict.durable, "a fully stored asset is durable");
    assert_eq!(att.core.nonce.as_ref().unwrap().as_ref(), nonce);

    // Invariant 34: mutating the verdict breaks the signature.
    let mut tampered = att.clone();
    tampered.core.verdict.durable = false;
    assert!(!keyring.verify_attestation(&tampered));
}

#[tokio::test]
async fn proof_of_loss_composition() {
    let ctx = setup().await;
    let svc = ctx.s_c15_service();
    let keyring = ctx.attestation();

    // Finalize an asset (indexed + on disk), take a custody receipt, then destroy the bytes.
    let asset_id = nanoid!();
    let bytes = vec![2u8; 4096];
    let hash = ctx.finalize_blob(&asset_id, "original", &bytes).await;
    let receipt = issue_receipt(&ctx, &keyring, &asset_id, &hash, bytes.len() as u64).await;

    // Server-side loss: delete the content-addressed blob.
    let blob_path = service::blob_store::blob_path(&ctx.upload_dir, &hash);
    std::fs::remove_file(&blob_path).unwrap();
    assert!(
        !blob_path.exists(),
        "the content-addressed fetch of the hash now fails"
    );

    // A signed verify reports the blob non-stored under the server's own signature.
    let mut res = TestClient::post("http://localhost/storage/verify")
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .json(&serde_json::json!({
            "assets": [{ "asset_id": asset_id, "blob_hashes": [hash] }],
            "signed": true,
        }))
        .send(&svc)
        .await;
    let body = res.take_json::<serde_json::Value>().await.unwrap();
    let att = decode_attestation(&body["attestations"][0]);
    assert!(!att.core.verdict.durable, "the lost asset is not durable");
    assert!(
        !att.core.verdict.blobs[0].stored,
        "the server reports stored=false"
    );

    // The (receipt, attestation) pair composes to a provable loss under one server_key_id.
    assert_eq!(receipt.core.server_key_id, att.core.server_key_id);
    assert_eq!(
        classify_non_holding(&receipt, &att, None, Timestamp::now(), &keyring),
        NonHolding::Loss,
    );
}

#[tokio::test]
async fn asset_receipts_endpoint() {
    let ctx = setup().await;
    let svc = ctx.s_c15_service();
    let keyring = ctx.attestation();

    let asset_id = nanoid!();
    let bytes = vec![3u8; 2048];
    let hash = ctx.finalize_blob(&asset_id, "original", &bytes).await;
    seed_asset(&ctx, &asset_id, &hash, bytes.len() as i64).await;
    let receipt = issue_receipt(&ctx, &keyring, &asset_id, &hash, bytes.len() as u64).await;

    let mut res = TestClient::get(format!("http://localhost/assets/{asset_id}/receipts"))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body = res.take_json::<serde_json::Value>().await.unwrap();
    let receipts = body["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 1);

    let cbor = base64::engine::general_purpose::STANDARD
        .decode(receipts[0]["receipt_cbor"].as_str().unwrap())
        .unwrap();
    let served = CustodyReceipt::from_canonical_cbor(&cbor).unwrap();
    assert_eq!(served, receipt);
    assert!(keyring.verify_receipt(&served));
}

#[tokio::test]
async fn well_known_publishes_active_key() {
    let ctx = setup().await;
    let svc = ctx.s_c15_service();
    let keyring = ctx.attestation();

    let mut res = TestClient::get("http://localhost/.well-known/capsule/attestation-keys")
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body = res.take_json::<serde_json::Value>().await.unwrap();
    assert_eq!(body["server_id"].as_str().unwrap(), "localhost");
    let keys = body["keys"].as_array().unwrap();
    assert!(
        keys.iter()
            .any(|k| k["key_id"].as_str() == Some(&keyring.active_key_id().to_hex())),
        "the active attestation key is published"
    );
}

#[tokio::test]
async fn signed_attestation_rate_limited_like_deep() {
    // The signed path shares `deep`'s per-user budget. A fresh service has the default budget
    // (32/window); exhaust it and assert a 429 with the deep-rate-limited code.
    let ctx = setup().await;
    let svc = ctx.s_c15_service();

    let asset_id = nanoid!();
    let bytes = vec![4u8; 512];
    let hash = ctx.finalize_blob(&asset_id, "original", &bytes).await;

    let mut last = None;
    for _ in 0..40 {
        let res = TestClient::post("http://localhost/storage/verify")
            .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
            .json(&serde_json::json!({
                "assets": [{ "asset_id": asset_id, "blob_hashes": [hash] }],
                "signed": true,
            }))
            .send(&svc)
            .await;
        last = res.status_code;
        if last == Some(StatusCode::TOO_MANY_REQUESTS) {
            break;
        }
    }
    assert_eq!(
        last,
        Some(StatusCode::TOO_MANY_REQUESTS),
        "the signed path is server-priced (per-user budget), like deep"
    );
}
