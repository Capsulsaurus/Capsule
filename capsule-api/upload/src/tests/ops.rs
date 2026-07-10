//! Slice `S-C16` — the generic lifecycle-write surface, `POST /albums/{album_id}/ops`.
//!
//! These run against the real router (salvo `TestClient` over the ops router) backed by
//! testcontainer Postgres. They assert the key-free structural battery uniformly for every
//! action: invariants 16 (closed action set), 17 (`prior_provenance_hash` chain match, `409`
//! stale-revival), 18 (monotonic `amk_version`), and 25 (metadata-blob hash binding) each
//! reject with their status **and** `error.*` code; the content-hash replay returns the
//! byte-identical prior response; and a delete → restore round-trip appears on the sync feed
//! in order.

use capsule_i18n::error_codes;
use nanoid::nanoid;
use salvo::Service;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, Set};
use serde_json::{Value, json};
use service::sync::{ChangeKind, FeedBlobManifest, FeedEntryInput, Mutation as SyncMutation};

use super::{PROTOCOL, TestCtx, error_code, setup};
use crate::models::requests::ManifestEnvelope;

/// Build a lifecycle-op manifest envelope for `action` over `(album_id, file_id)`.
fn op_envelope(
    album_id: &str,
    file_id: &str,
    action: &str,
    amk: u32,
    prior: Option<&str>,
    metadata_blob_hash: Option<&str>,
) -> Value {
    let mut env = json!({
        "crypto_suite_id": 1,
        "protocol_version": PROTOCOL,
        "album_id": album_id,
        "file_id": file_id,
        "amk_version": amk,
        // A lifecycle op references already-stored bytes; this is a placeholder content
        // address the key-free battery never inspects.
        "ciphertext_hash": "ab".repeat(32),
        "plaintext_size": 1024,
        "chunk_size": 65536,
        "key_mode": "derived",
        "created_by_user": nanoid!(),
        "created_by_device": nanoid!(),
        "client_version": "capsule-test/1.0",
        "timestamp": jiff::Timestamp::now().to_string(),
        "action": action,
    });
    if let Some(p) = prior {
        env["prior_provenance_hash"] = json!(p);
    }
    if let Some(h) = metadata_blob_hash {
        env["metadata_blob_hash"] = json!(h);
    }
    env
}

/// The canonical CBOR of an envelope value — byte-identical to what the server re-serializes.
fn envelope_cbor(env: &Value) -> Vec<u8> {
    let parsed: ManifestEnvelope = serde_json::from_value(env.clone()).expect("envelope");
    capsule_core::cbor::to_canonical_vec(&parsed).expect("cbor")
}

/// The provenance-chain head an op must chain onto: the content hash of a manifest's CBOR.
fn chain_head(env: &Value) -> String {
    capsule_core::crypto::hash::hash_bytes(&envelope_cbor(env)).to_hex()
}

/// Seed a `create` feed entry for `file_id` (the asset's chain root) at `amk`, and a matching
/// live `assets` row keyed on `file_id`. Returns the create envelope so callers can chain onto
/// its content hash.
async fn seed_created_asset(ctx: &TestCtx, file_id: &str, amk: u32) -> Value {
    let create = op_envelope(&ctx.album_id, file_id, "create", amk, None, None);
    SyncMutation::record_finalization(
        &ctx.db,
        FeedEntryInput {
            album_id: ctx.album_id.clone(),
            protocol_version: PROTOCOL.to_string(),
            kind: ChangeKind::Created,
            asset_id: file_id.to_string(),
            manifest_cbor: envelope_cbor(&create),
            metadata_blob: None,
            blobs: FeedBlobManifest::default(),
            original_held: true,
        },
    )
    .await
    .expect("seed create feed entry");

    entity::asset::ActiveModel {
        id: Set(file_id.to_string()),
        owner_id: Set(ctx.user_id.clone()),
        album_id: Set(Some(ctx.album_id.clone())),
        width: Set(0),
        height: Set(0),
        asset_type: Set(entity::asset::AssetType::Photo),
        original_filename: Set(nanoid!()),
        file_size: Set(1024),
        file_hash: Set(capsule_core::utils::hash::hash_bytes(file_id.as_bytes())),
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
    .expect("seed asset row");

    create
}

/// POST a lifecycle op bundle (`{ manifest_envelope, metadata_blob? }`) to the ops surface.
async fn post_op(
    ctx: &TestCtx,
    svc: &Service,
    env: &Value,
    metadata_blob_b64: Option<&str>,
) -> (StatusCode, Value) {
    let mut bundle = json!({ "manifest_envelope": env });
    if let Some(b) = metadata_blob_b64 {
        bundle["metadata_blob"] = json!(b);
    }
    let mut res = TestClient::post(format!("http://localhost/albums/{}/ops", ctx.album_id))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .json(&bundle)
        .send(svc)
        .await;
    let status = res.status_code.unwrap_or(StatusCode::OK);
    let json = res.take_json::<Value>().await.unwrap_or(Value::Null);
    (status, json)
}

/// The full feed for the seeded album, oldest first.
async fn feed(ctx: &TestCtx) -> Vec<entity::sync_entry::Model> {
    entity::sync_entry::Entity::find()
        .filter(entity::sync_entry::Column::AlbumId.eq(&ctx.album_id))
        .order_by_asc(entity::sync_entry::Column::FeedSeq)
        .all(&ctx.db)
        .await
        .unwrap()
}

// ─────────────────────────────── Invariant 16 ────────────────────────────────

#[tokio::test]
async fn invariant_16_unknown_action_rejected() {
    let ctx = setup().await;
    let svc = ctx.ops_service();
    let file_id = nanoid!();
    let create = seed_created_asset(&ctx, &file_id, 1).await;

    let env = op_envelope(
        &ctx.album_id,
        &file_id,
        "future-action-not-yet-defined",
        1,
        Some(&chain_head(&create)),
        None,
    );
    let (status, body) = post_op(&ctx, &svc, &env, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body:?}");
    assert_eq!(error_code(&body), Some(error_codes::UPLOAD_INVALID_ACTION));
}

#[tokio::test]
async fn invariant_16_upload_only_action_rejected() {
    let ctx = setup().await;
    let svc = ctx.ops_service();
    let file_id = nanoid!();
    let create = seed_created_asset(&ctx, &file_id, 1).await;

    // `create`/`replace` ride the upload protocol, never this surface.
    for action in ["create", "replace"] {
        let env = op_envelope(
            &ctx.album_id,
            &file_id,
            action,
            1,
            Some(&chain_head(&create)),
            None,
        );
        let (status, body) = post_op(&ctx, &svc, &env, None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{action}: body={body:?}");
        assert_eq!(
            error_code(&body),
            Some(error_codes::UPLOAD_INVALID_ACTION),
            "{action}"
        );
    }
}

// ─────────────────────────────── Invariant 17 ────────────────────────────────

#[tokio::test]
async fn invariant_17_stale_prior_is_409_stale_revival() {
    let ctx = setup().await;
    let svc = ctx.ops_service();
    let file_id = nanoid!();
    let _create = seed_created_asset(&ctx, &file_id, 1).await;

    // A delete carrying a prior hash that is NOT the asset's chain head — the classic
    // stale-revival (a peer replaying an old-but-signed manifest).
    let env = op_envelope(
        &ctx.album_id,
        &file_id,
        "delete",
        1,
        Some(&"cd".repeat(32)),
        None,
    );
    let (status, body) = post_op(&ctx, &svc, &env, None).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body:?}");
    assert_eq!(error_code(&body), Some(error_codes::UPLOAD_STALE_REVIVAL));

    // Nothing was written: the feed still holds only the seeded create entry.
    assert_eq!(feed(&ctx).await.len(), 1, "rejection writes no feed row");
}

// ─────────────────────────────── Invariant 18 ────────────────────────────────

#[tokio::test]
async fn invariant_18_amk_regression_rejected() {
    let ctx = setup().await;
    let svc = ctx.ops_service();
    let file_id = nanoid!();
    // Album epoch recorded at amk 5 by the create entry.
    let create = seed_created_asset(&ctx, &file_id, 5).await;

    // A delete at amk 3 regresses the album epoch — refused even with a valid chain head.
    let env = op_envelope(
        &ctx.album_id,
        &file_id,
        "delete",
        3,
        Some(&chain_head(&create)),
        None,
    );
    let (status, body) = post_op(&ctx, &svc, &env, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body:?}");
    assert_eq!(error_code(&body), Some(error_codes::UPLOAD_AMK_REGRESSED));
}

// ─────────────────────────────── Invariant 25 ────────────────────────────────

#[tokio::test]
async fn invariant_25_metadata_blob_hash_mismatch_rejected() {
    let ctx = setup().await;
    let svc = ctx.ops_service();
    let file_id = nanoid!();
    let create = seed_created_asset(&ctx, &file_id, 1).await;

    // A metadata-update whose committed `metadata_blob_hash` does not match the blob it carries.
    let blob = b"an-encrypted-metadata-blob";
    let blob_b64 = base64_encode(blob);
    let env = op_envelope(
        &ctx.album_id,
        &file_id,
        "metadata-update",
        1,
        Some(&chain_head(&create)),
        Some(&"00".repeat(32)), // not the blob's hash
    );
    let (status, body) = post_op(&ctx, &svc, &env, Some(&blob_b64)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body:?}");
    assert_eq!(
        error_code(&body),
        Some(error_codes::UPLOAD_ENVELOPE_MISMATCH)
    );
    assert_eq!(feed(&ctx).await.len(), 1, "rejection writes no feed row");
}

#[tokio::test]
async fn valid_metadata_update_is_recorded_and_charged() {
    let ctx = setup().await;
    let svc = ctx.ops_service();
    let file_id = nanoid!();
    let create = seed_created_asset(&ctx, &file_id, 1).await;

    let blob = b"an-encrypted-metadata-blob";
    let committed = capsule_core::crypto::hash::hash_bytes(blob).to_hex();
    let env = op_envelope(
        &ctx.album_id,
        &file_id,
        "metadata-update",
        1,
        Some(&chain_head(&create)),
        Some(&committed),
    );
    let (status, body) = post_op(&ctx, &svc, &env, Some(&base64_encode(blob))).await;
    assert_eq!(status, StatusCode::OK, "body={body:?}");
    assert_eq!(body["action"], "metadata-update");

    // The feed carries a MetadataUpdated entry inlining the blob; quota charged the blob once.
    let entries = feed(&ctx).await;
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].kind, ChangeKind::MetadataUpdated.as_i16());
    assert_eq!(entries[1].metadata_blob.as_deref(), Some(&blob[..]));
    let ledgered = entity::quota_ledger::Entity::find()
        .filter(entity::quota_ledger::Column::ContentHash.eq(&committed))
        .one(&ctx.db)
        .await
        .unwrap();
    assert!(ledgered.is_some(), "metadata blob charged to quota");
}

// ─────────────────────────────── Replay ────────────────────────────────

#[tokio::test]
async fn replay_returns_byte_identical_response() {
    let ctx = setup().await;
    let svc = ctx.ops_service();
    let file_id = nanoid!();
    let create = seed_created_asset(&ctx, &file_id, 1).await;

    let env = op_envelope(
        &ctx.album_id,
        &file_id,
        "delete",
        1,
        Some(&chain_head(&create)),
        None,
    );

    // First submission applies; the byte-identical resubmission short-circuits to the stored
    // response (the raw body bytes must match exactly).
    let first = post_op_raw(&ctx, &svc, &env, None).await;
    let second = post_op_raw(&ctx, &svc, &env, None).await;
    assert_eq!(first.0, StatusCode::OK);
    assert_eq!(second.0, StatusCode::OK);
    assert_eq!(first.1, second.1, "replay is byte-identical");

    // Applied at most once: exactly one delete entry beyond the seeded create.
    let entries = feed(&ctx).await;
    assert_eq!(entries.len(), 2, "replay minted no second feed row");
    assert_eq!(entries[1].kind, ChangeKind::Deleted.as_i16());
}

// ───────────────────────── Delete → restore round-trip ─────────────────────────

#[tokio::test]
async fn delete_then_restore_round_trip_on_feed() {
    let ctx = setup().await;
    let svc = ctx.ops_service();
    let file_id = nanoid!();
    let create = seed_created_asset(&ctx, &file_id, 1).await;

    // delete → tombstone.
    let delete = op_envelope(
        &ctx.album_id,
        &file_id,
        "delete",
        1,
        Some(&chain_head(&create)),
        None,
    );
    let (status, _) = post_op(&ctx, &svc, &delete, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        asset_deleted(&ctx, &file_id).await,
        "delete sets deleted_at"
    );

    // trash-restore chains onto the delete's manifest and returns the asset to the live set.
    let restore = op_envelope(
        &ctx.album_id,
        &file_id,
        "trash-restore",
        1,
        Some(&chain_head(&delete)),
        None,
    );
    let (status, _) = post_op(&ctx, &svc, &restore, None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !asset_deleted(&ctx, &file_id).await,
        "restore clears deleted_at"
    );

    // The feed carries create → delete → restore in strict sync_seq order.
    let entries = feed(&ctx).await;
    let kinds: Vec<i16> = entries.iter().map(|e| e.kind).collect();
    assert_eq!(
        kinds,
        vec![
            ChangeKind::Created.as_i16(),
            ChangeKind::Deleted.as_i16(),
            ChangeKind::Created.as_i16(),
        ]
    );
    let seqs: Vec<i64> = entries.iter().map(|e| e.sync_seq).collect();
    assert_eq!(seqs, vec![1, 2, 3], "gap-free, strictly increasing");
}

// ─────────────────────────────── helpers ────────────────────────────────

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Whether the asset row keyed on `file_id` is soft-deleted.
async fn asset_deleted(ctx: &TestCtx, file_id: &str) -> bool {
    entity::asset::Entity::find_by_id(file_id)
        .one(&ctx.db)
        .await
        .unwrap()
        .expect("asset row")
        .deleted_at
        .is_some()
}

/// POST returning the raw response body bytes (for byte-identity assertions).
async fn post_op_raw(
    ctx: &TestCtx,
    svc: &Service,
    env: &Value,
    metadata_blob_b64: Option<&str>,
) -> (StatusCode, Vec<u8>) {
    let mut bundle = json!({ "manifest_envelope": env });
    if let Some(b) = metadata_blob_b64 {
        bundle["metadata_blob"] = json!(b);
    }
    let mut res = TestClient::post(format!("http://localhost/albums/{}/ops", ctx.album_id))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .json(&bundle)
        .send(svc)
        .await;
    let status = res.status_code.unwrap_or(StatusCode::OK);
    let bytes = res.take_bytes(None).await.unwrap().to_vec();
    (status, bytes)
}
