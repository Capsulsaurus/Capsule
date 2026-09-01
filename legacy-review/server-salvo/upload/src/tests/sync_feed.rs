//! Slice `S-C2` — the per-album `sync_seq` mint inside the finalization transaction.
//!
//! These run against the real finalization path (testcontainer Postgres + Valkey) and assert
//! the download-sync doc's **sync-feed monotonicity** Validation bullet: every `sync_seq`
//! advance over an album is strictly increasing, and concurrent finalizations are linearised
//! by the per-album counter row lock the mint takes inside the finalization transaction.

use bytes::Bytes;
use salvo::Service;
use salvo::http::StatusCode;
use salvo::test::TestClient;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use super::{PROTOCOL, TestCtx, setup, sha256_hex, valid_create_body};

/// POST a create session, returning the upload id.
async fn create(ctx: &TestCtx, svc: &Service, hash: &str, size: u64) -> String {
    use salvo::test::ResponseExt;
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

/// Create a session, fill it with one chunk via the service (no auto-finalize), and finalize.
async fn finalize_one(ctx: &TestCtx, svc: &Service, fill: u8) -> String {
    let bytes = vec![fill; 4096];
    let hash = sha256_hex(&bytes);
    let id = create(ctx, svc, &hash, bytes.len() as u64).await;
    ctx.upload_service
        .append_chunk(&id, Bytes::from(bytes), 0, &hash)
        .await
        .unwrap();
    id
}

async fn feed_seqs(ctx: &TestCtx) -> Vec<i64> {
    entity::sync_entry::Entity::find()
        .filter(entity::sync_entry::Column::AlbumId.eq(&ctx.album_id))
        .order_by_asc(entity::sync_entry::Column::FeedSeq)
        .all(&ctx.db)
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.sync_seq)
        .collect()
}

#[tokio::test]
async fn sync_feed_monotonicity() {
    let ctx = setup().await;
    let svc = ctx.service();

    // Finalize three assets in the same album, one after another.
    for fill in 1u8..=3 {
        let id = finalize_one(&ctx, &svc, fill).await;
        ctx.upload_service.finalize_upload(&id).await.unwrap();
    }

    // Every advance is strictly increasing, gap-free, starting at 1.
    assert_eq!(feed_seqs(&ctx).await, vec![1, 2, 3]);

    // The feed row carries the finalized facts: canonical-CBOR manifest, the album pin, the
    // derived original_held (true for an original-role blob), and the Created change kind.
    let entries = entity::sync_entry::Entity::find()
        .filter(entity::sync_entry::Column::AlbumId.eq(&ctx.album_id))
        .order_by_asc(entity::sync_entry::Column::FeedSeq)
        .all(&ctx.db)
        .await
        .unwrap();
    for entry in &entries {
        assert!(!entry.manifest_cbor.is_empty(), "manifest travels as CBOR");
        assert_eq!(entry.protocol_version, PROTOCOL);
        assert!(entry.original_held, "original-role blob ⇒ original_held");
        assert_eq!(entry.kind, 1, "CHANGE_KIND_CREATED");
    }
}

#[tokio::test]
async fn sync_feed_mint_is_linearised_under_concurrency() {
    let ctx = setup().await;
    let svc = ctx.service();

    // Two sessions filled and ready to finalize concurrently.
    let a = finalize_one(&ctx, &svc, 10).await;
    let b = finalize_one(&ctx, &svc, 20).await;

    let svc_a = ctx.upload_service.clone();
    let svc_b = ctx.upload_service.clone();
    let (ra, rb) = tokio::join!(async move { svc_a.finalize_upload(&a).await }, async move {
        svc_b.finalize_upload(&b).await
    },);
    ra.unwrap();
    rb.unwrap();

    // The per-album counter row lock serialises the two mints: distinct, gap-free {1, 2}.
    let mut seqs = feed_seqs(&ctx).await;
    seqs.sort_unstable();
    assert_eq!(seqs, vec![1, 2], "concurrent mints are linearised");
}
