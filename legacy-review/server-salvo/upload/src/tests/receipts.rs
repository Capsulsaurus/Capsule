//! Slice `S-C15` — custody receipts issued inside the finalization transaction.
//!
//! Run against the real finalization path (testcontainer Postgres + Valkey). Coverage — the
//! storage-verification doc's receipt Validation bullets that live on the upload server:
//! - `receipt_issuance` — finalize to `Completed`, fetch the receipt over
//!   `GET /upload/{id}/receipt`, verify the hybrid signature under the server keyring, and
//!   assert `ciphertext_hash` / `size` / `blob_role` / `envelope_hash` / `received_at` match.
//! - `no_receipt_without_custody` — a finalization transaction rolled back before commit
//!   leaves neither a receipt nor an advanced `receipt_seq` (issuance atomicity, invariant 33).
//! - `receipt_log_is_monotonic_and_chained` — two finalizations yield strictly increasing
//!   `receipt_seq` with correct `prior_receipt_hash` chaining.
//! - `receipt_log_is_append_only` — any UPDATE/DELETE on a receipt row is rejected at the
//!   structural (database trigger) layer.
//! - `receipt_not_available_before_completed` — the fetch is `409
//!   error.upload.receipt_not_available` before the session reaches `Completed`.

use base64::Engine as _;
use bytes::Bytes;
use salvo::Service;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use sea_orm::{ConnectionTrait, EntityTrait, QueryOrder, Statement};
use service::attestation::{CustodyReceipt, Mutation, ReceiptInput};

use super::{PROTOCOL, TestCtx, error_code, setup, sha256_hex, valid_create_body};

/// POST a create session, returning the upload id.
async fn create(ctx: &TestCtx, svc: &Service, hash: &str, size: u64) -> String {
    let body = valid_create_body(&ctx.album_id, hash, size);
    let mut res = TestClient::post("http://localhost/upload")
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .json(&body)
        .send(svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CREATED));
    res.take_json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Create a session and fill it with one chunk (no auto-finalize), returning `(id, bytes)`.
async fn create_and_fill(ctx: &TestCtx, svc: &Service, fill: u8) -> (String, Vec<u8>) {
    let bytes = vec![fill; 4096];
    let hash = sha256_hex(&bytes);
    let id = create(ctx, svc, &hash, bytes.len() as u64).await;
    ctx.upload_service
        .append_chunk(&id, Bytes::from(bytes.clone()), 0, &hash)
        .await
        .unwrap();
    (id, bytes)
}

async fn receipt_rows(ctx: &TestCtx) -> Vec<entity::custody_receipt::Model> {
    entity::custody_receipt::Entity::find()
        .order_by_asc(entity::custody_receipt::Column::ReceiptSeq)
        .all(&ctx.db)
        .await
        .unwrap()
}

#[tokio::test]
async fn receipt_issuance() {
    let ctx = setup().await;
    let svc = ctx.service();

    let (id, bytes) = create_and_fill(&ctx, &svc, 1).await;
    let hash = sha256_hex(&bytes);
    ctx.upload_service.finalize_upload(&id).await.unwrap();

    // Fetch the receipt over the real HTTP surface.
    let mut res = TestClient::get(format!("http://localhost/upload/{id}/receipt"))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body = res.take_json::<serde_json::Value>().await.unwrap();

    // The signed CBOR verifies under the server's attestation keyring.
    let cbor = base64::engine::general_purpose::STANDARD
        .decode(body["receipt_cbor"].as_str().unwrap())
        .unwrap();
    let receipt = CustodyReceipt::from_canonical_cbor(&cbor).unwrap();
    assert!(
        ctx.config.attestation.verify_receipt(&receipt),
        "the receipt verifies under the published attestation key"
    );

    // The receipt matches the finalized state.
    assert_eq!(receipt.core.ciphertext_hash.to_hex(), hash);
    assert_eq!(receipt.core.size, bytes.len() as u64);
    assert_eq!(receipt.core.blob_role, "original");
    assert_eq!(receipt.core.receipt_seq, 1);
    assert!(
        receipt.core.envelope_hash.is_some(),
        "envelope hash is bound"
    );
    assert!(
        receipt.core.received_at.parse::<jiff::Timestamp>().is_ok(),
        "received_at is a real server timestamp"
    );
    assert_eq!(receipt.core.server_id, "localhost");
}

#[tokio::test]
async fn no_receipt_without_custody() {
    // Inject a failure between hash verification and commit by running the receipt insert in a
    // transaction that is rolled back: neither the receipt nor the sequence must survive.
    let ctx = setup().await;
    let keyring = &ctx.config.attestation;

    let input = ReceiptInput {
        protocol_version: PROTOCOL.to_string(),
        upload_id: "u-abort".to_string(),
        asset_id: "a-abort".to_string(),
        blob_role: "original".to_string(),
        ciphertext_hash: capsule_core::crypto::hash::hash_bytes(b"aborted"),
        size: 7,
        envelope_hash: None,
        uploaded_by_user: ctx.user_id.clone(),
        uploaded_by_device: None,
        received_at: jiff::Timestamp::now(),
    };

    use sea_orm::TransactionTrait;
    let txn = ctx.db.begin().await.unwrap();
    let r = Mutation::issue_receipt(&txn, keyring, input).await.unwrap();
    assert_eq!(r.core.receipt_seq, 1, "the aborted txn saw seq 1");
    txn.rollback().await.unwrap();

    // Nothing committed: no receipt row and the sequence counter did not advance.
    assert!(
        receipt_rows(&ctx).await.is_empty(),
        "no receipt survives the rollback"
    );

    // A subsequent real finalization takes seq 1 — proof the aborted mint left no gap.
    let svc = ctx.service();
    let (id, _) = create_and_fill(&ctx, &svc, 2).await;
    ctx.upload_service.finalize_upload(&id).await.unwrap();
    let rows = receipt_rows(&ctx).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].receipt_seq, 1,
        "the committed receipt is seq 1, no gap"
    );
}

#[tokio::test]
async fn receipt_log_is_monotonic_and_chained() {
    let ctx = setup().await;
    let svc = ctx.service();

    for fill in 1u8..=2 {
        let (id, _) = create_and_fill(&ctx, &svc, fill).await;
        ctx.upload_service.finalize_upload(&id).await.unwrap();
    }

    let rows = receipt_rows(&ctx).await;
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].receipt_seq, 1);
    assert_eq!(rows[1].receipt_seq, 2, "strictly increasing, gap-free");

    // The first receipt has no prior; the second chains from the first's content hash.
    assert!(rows[0].prior_receipt_hash.is_none());
    assert_eq!(
        rows[1].prior_receipt_hash.as_deref(),
        Some(rows[0].receipt_hash.as_str()),
        "prior_receipt_hash matches the preceding receipt's content hash"
    );

    // And the stored receipt_hash actually equals the signed receipt's content hash.
    let receipt = CustodyReceipt::from_canonical_cbor(&rows[0].receipt_cbor).unwrap();
    assert_eq!(receipt.content_hash().to_hex(), rows[0].receipt_hash);
}

#[tokio::test]
async fn receipt_log_is_append_only() {
    let ctx = setup().await;
    let svc = ctx.service();
    let (id, _) = create_and_fill(&ctx, &svc, 3).await;
    ctx.upload_service.finalize_upload(&id).await.unwrap();

    // The migration trigger rejects both UPDATE and DELETE at the structural layer.
    let update = ctx
        .db
        .execute(Statement::from_string(
            ctx.db.get_database_backend(),
            "UPDATE custody_receipts SET size = 0".to_string(),
        ))
        .await;
    assert!(update.is_err(), "UPDATE on a receipt row is rejected");

    let delete = ctx
        .db
        .execute(Statement::from_string(
            ctx.db.get_database_backend(),
            "DELETE FROM custody_receipts".to_string(),
        ))
        .await;
    assert!(delete.is_err(), "DELETE on a receipt row is rejected");

    // The row is still there and unchanged.
    let rows = receipt_rows(&ctx).await;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].size > 0);
}

#[tokio::test]
async fn receipt_not_available_before_completed() {
    let ctx = setup().await;
    let svc = ctx.service();

    // A created-but-not-finalized session has no receipt yet.
    let (id, _) = create_and_fill(&ctx, &svc, 4).await;

    let mut res = TestClient::get(format!("http://localhost/upload/{id}/receipt"))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CONFLICT));
    let body = res.take_json::<serde_json::Value>().await.unwrap();
    assert_eq!(
        error_code(&body),
        Some("error.upload.receipt_not_available"),
        "the pre-Completed fetch carries the receipt_not_available code"
    );

    // A bogus session id is a 404, not a 409.
    let res = TestClient::get("http://localhost/upload/does-not-exist/receipt")
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
}
