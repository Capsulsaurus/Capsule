//! Slice `S-C11` — the refcount GC + retention purge worker.
//!
//! These run the real workers (`service::gc`) against testcontainer Postgres + a real
//! on-disk content-addressed blob tree, driving genuine `delete` manifests (carrying
//! `retention_until`) through the S-C16 lifecycle endpoint so the retention floor is read
//! back from a signed envelope exactly as production does. Every time-based assertion uses an
//! **injected clock** — no sleeps.
//!
//! Coverage map:
//! - `retention_window_honor_*` — the organization doc's retention smoke: a delete with
//!   `retention_until = now + 30d` is refused at `now + 15d` and proceeds at `now + 31d`.
//! - `hostile_server_purge_defense` — an early purge attempt is refused by the no-key
//!   envelope check; there is no local-config path to accelerate it.
//! - `mark_and_sweep_honors_grace_window` / `just_marked_blob_survives_release_window` — a
//!   just-marked blob is never byte-deleted inside `GC_GRACE_WINDOW` (the `S-C3` contract).
//! - `quarantined_blob_is_never_swept` — a quarantined blob survives arbitrarily far past any
//!   grace window.
//! - `dangling_reference_is_quarantined_never_deleted` — a committed row → missing blob is
//!   surfaced, never auto-deleted, never treated as collectable.
//! - `retention_purge_then_gc_reclaims_original` — the two workers compose end-to-end: purge
//!   drops the reference, GC reclaims the bytes after the grace window.

use jiff::{SignedDuration, Timestamp};
use nanoid::nanoid;
use salvo::Service;
use salvo::http::StatusCode;
use salvo::test::TestClient;
use sea_orm::{ActiveModelTrait, EntityTrait, Set};
use serde_json::{Value, json};
use service::gc::{Clock, GcWorker, RetentionPurgeWorker, reference_count};
use service::sync::{ChangeKind, FeedBlobManifest, FeedBlobRef, FeedEntryInput, Mutation as Sync};

use super::{PROTOCOL, TestCtx, setup};
use crate::models::requests::ManifestEnvelope;

// ─────────────────────────────── clock seam ────────────────────────────────

/// A deterministic [`Clock`] pinned to one instant — the injected time the workers reason
/// over, so the grace/retention windows are proven without sleeping.
struct FixedClock(Timestamp);

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

/// The fixed base instant every windowed test reasons from.
fn base() -> Timestamp {
    "2026-07-10T00:00:00Z".parse().unwrap()
}

fn days(n: i64) -> SignedDuration {
    SignedDuration::from_hours(24 * n)
}

// ─────────────────────────────── envelope helpers ──────────────────────────

/// A lifecycle-op manifest envelope for `action`, optionally carrying `prior_provenance_hash`
/// and a signed `retention_until`.
fn op_envelope(
    album_id: &str,
    file_id: &str,
    action: &str,
    amk: u32,
    prior: Option<&str>,
    retention_until: Option<&str>,
) -> Value {
    let mut env = json!({
        "crypto_suite_id": 1,
        "protocol_version": PROTOCOL,
        "album_id": album_id,
        "file_id": file_id,
        "amk_version": amk,
        "ciphertext_hash": "ab".repeat(32),
        "plaintext_size": 1024,
        "chunk_size": 65536,
        "key_mode": "derived",
        "created_by_user": nanoid!(),
        "created_by_device": nanoid!(),
        "client_version": "capsule-test/1.0",
        "timestamp": Timestamp::now().to_string(),
        "action": action,
    });
    if let Some(p) = prior {
        env["prior_provenance_hash"] = json!(p);
    }
    if let Some(r) = retention_until {
        env["retention_until"] = json!(r);
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

// ─────────────────────────────── seeding + probes ──────────────────────────

/// Seed a finalized asset whose original ciphertext blob physically exists in the store:
/// writes the blob file, seeds a `create` feed entry referencing it, and inserts the live
/// `assets` row keyed on `file_id` (so the delete op finds it). Returns `(content_hash,
/// create_envelope)`.
async fn seed_stored_asset(ctx: &TestCtx, file_id: &str, original: &[u8]) -> (String, Value) {
    let hash = capsule_core::crypto::hash::hash_bytes(original).to_hex();
    write_blob(ctx, &hash, original);

    let create = op_envelope(&ctx.album_id, file_id, "create", 1, None, None);
    Sync::record_finalization(
        &ctx.db,
        FeedEntryInput {
            album_id: ctx.album_id.clone(),
            protocol_version: PROTOCOL.to_string(),
            kind: ChangeKind::Created,
            asset_id: file_id.to_string(),
            manifest_cbor: envelope_cbor(&create),
            metadata_blob: None,
            blobs: FeedBlobManifest {
                original: Some(FeedBlobRef {
                    ciphertext_hash: hash.clone(),
                    role: "original".to_string(),
                    format: "image/jpeg".to_string(),
                    size: original.len() as u64,
                }),
                derivatives: Vec::new(),
            },
            original_held: true,
        },
    )
    .await
    .expect("seed create feed entry");

    insert_asset_row(ctx, file_id, &hash, original.len() as i64).await;
    (hash, create)
}

/// Insert a live `assets` row keyed on `file_id` referencing `hash`.
async fn insert_asset_row(ctx: &TestCtx, file_id: &str, hash: &str, size: i64) {
    entity::asset::ActiveModel {
        id: Set(file_id.to_string()),
        owner_id: Set(ctx.user_id.clone()),
        album_id: Set(Some(ctx.album_id.clone())),
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
    .expect("insert asset row");
}

/// Write `bytes` to the content-addressed path for `hash`.
fn write_blob(ctx: &TestCtx, hash: &str, bytes: &[u8]) {
    std::fs::create_dir_all(service::blob_store::blobs_dir(&ctx.upload_dir)).unwrap();
    std::fs::write(service::blob_store::blob_path(&ctx.upload_dir, hash), bytes).unwrap();
}

/// Whether the blob file for `hash` exists on disk.
fn blob_exists(ctx: &TestCtx, hash: &str) -> bool {
    service::blob_store::blob_path(&ctx.upload_dir, hash).exists()
}

/// Whether the `assets` row keyed on `file_id` still exists.
async fn asset_present(ctx: &TestCtx, file_id: &str) -> bool {
    entity::asset::Entity::find_by_id(file_id)
        .one(&ctx.db)
        .await
        .unwrap()
        .is_some()
}

/// POST a `delete`/`trash-restore` op bundle to the S-C16 lifecycle surface.
async fn post_op(ctx: &TestCtx, svc: &Service, env: &Value) -> StatusCode {
    let res = TestClient::post(format!("http://localhost/albums/{}/ops", ctx.album_id))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .json(&json!({ "manifest_envelope": env }))
        .send(svc)
        .await;
    res.status_code.unwrap_or(StatusCode::OK)
}

/// Drive a real `delete` op with `retention_until` against the lifecycle endpoint.
async fn soft_delete(
    ctx: &TestCtx,
    svc: &Service,
    file_id: &str,
    create: &Value,
    until: Timestamp,
) {
    let delete = op_envelope(
        &ctx.album_id,
        file_id,
        "delete",
        1,
        Some(&chain_head(create)),
        Some(&until.to_string()),
    );
    let status = post_op(ctx, svc, &delete).await;
    assert_eq!(status, StatusCode::OK, "delete op should be accepted");
    assert!(
        asset_present(ctx, file_id).await,
        "soft delete keeps the row"
    );
}

// ─────────────────────────── retention smokes ──────────────────────────────

#[tokio::test]
async fn retention_window_honor_refuses_early_and_proceeds_after() {
    let ctx = setup().await;
    let svc = ctx.ops_service();
    let file_id = nanoid!();
    let (hash, create) = seed_stored_asset(&ctx, &file_id, b"retention-original-bytes").await;

    // delete with retention_until = base + 30d.
    soft_delete(&ctx, &svc, &file_id, &create, base() + days(30)).await;

    // now + 15d: inside the signed window → refused, row and bytes retained.
    let early = RetentionPurgeWorker::with_clock(FixedClock(base() + days(15)));
    let report = early.purge_expired(&ctx.db, false).await.unwrap();
    assert_eq!(report.refused_in_window, vec![file_id.clone()]);
    assert!(report.purged.is_empty());
    assert!(
        asset_present(&ctx, &file_id).await,
        "refused purge keeps the row"
    );
    assert!(blob_exists(&ctx, &hash), "refused purge keeps the bytes");

    // now + 31d: past the signed floor → hard-purged, reference dropped.
    let late = RetentionPurgeWorker::with_clock(FixedClock(base() + days(31)));
    let report = late.purge_expired(&ctx.db, false).await.unwrap();
    assert_eq!(report.purged, vec![file_id.clone()]);
    assert!(
        !asset_present(&ctx, &file_id).await,
        "past-window purge drops the row"
    );
    assert_eq!(
        reference_count(&ctx.db, &hash).await.unwrap(),
        0,
        "original blob is now unreferenced"
    );
    // The bytes still exist — the purge drops the reference; GC reclaims the bytes later.
    assert!(
        blob_exists(&ctx, &hash),
        "purge does not itself byte-delete"
    );
}

#[tokio::test]
async fn hostile_server_purge_defense_refuses_before_retention() {
    let ctx = setup().await;
    let svc = ctx.ops_service();
    let file_id = nanoid!();
    let (hash, create) = seed_stored_asset(&ctx, &file_id, b"hostile-defense-bytes").await;

    // A long window the user chose; a hostile operator wants the asset gone now.
    let retention = base() + days(30);
    soft_delete(&ctx, &svc, &file_id, &create, retention).await;

    // The worker reads `retention_until` from the signed manifest — there is no config knob to
    // accelerate it — so an attempt one second before the floor is refused.
    let hostile =
        RetentionPurgeWorker::with_clock(FixedClock(retention - SignedDuration::from_secs(1)));
    let report = hostile.purge_expired(&ctx.db, false).await.unwrap();
    assert_eq!(report.refused_in_window, vec![file_id.clone()]);
    assert!(report.purged.is_empty(), "hostile early purge refused");
    assert!(asset_present(&ctx, &file_id).await);
    assert!(
        blob_exists(&ctx, &hash),
        "the bytes the user expected to be recoverable survive"
    );
}

#[tokio::test]
async fn retention_purge_skips_delete_without_signed_floor() {
    let ctx = setup().await;
    let svc = ctx.ops_service();
    let file_id = nanoid!();
    let (hash, create) = seed_stored_asset(&ctx, &file_id, b"no-floor-bytes").await;

    // A delete carrying no `retention_until` (an anomaly) — bias toward keeping bytes.
    let delete = op_envelope(
        &ctx.album_id,
        &file_id,
        "delete",
        1,
        Some(&chain_head(&create)),
        None,
    );
    assert_eq!(post_op(&ctx, &svc, &delete).await, StatusCode::OK);

    let worker = RetentionPurgeWorker::with_clock(FixedClock(base() + days(3650)));
    let report = worker.purge_expired(&ctx.db, false).await.unwrap();
    assert_eq!(report.skipped_no_floor, vec![file_id.clone()]);
    assert!(report.purged.is_empty(), "no signed floor → never purged");
    assert!(asset_present(&ctx, &file_id).await);
    assert!(blob_exists(&ctx, &hash));
}

// ─────────────────────── refcount mark-and-sweep + grace ────────────────────

#[tokio::test]
async fn mark_and_sweep_honors_grace_window() {
    let ctx = setup().await;
    // A committed-blob orphan: bytes in the store referenced by zero rows (finalization crash).
    let bytes = b"orphan-ciphertext-blob";
    let hash = capsule_core::crypto::hash::hash_bytes(bytes).to_hex();
    write_blob(&ctx, &hash, bytes);

    // Mark at base.
    let marker = GcWorker::with_clock(ctx.upload_dir.clone(), FixedClock(base()));
    let report = marker.mark_and_sweep(&ctx.db, false).await.unwrap();
    assert_eq!(report.marked, 1, "zero-reference orphan is marked");
    assert_eq!(report.swept, 0, "not swept in the same pass");
    assert!(blob_exists(&ctx, &hash));

    // Sweep 1 h later — still inside the 24 h grace window.
    let inside = GcWorker::with_clock(
        ctx.upload_dir.clone(),
        FixedClock(base() + SignedDuration::from_hours(1)),
    );
    let report = inside.mark_and_sweep(&ctx.db, false).await.unwrap();
    assert_eq!(report.marked, 0, "already marked, clock not reset");
    assert_eq!(report.retained_in_grace, 1);
    assert_eq!(report.swept, 0);
    assert!(blob_exists(&ctx, &hash), "bytes survive the grace window");

    // A dry run past the window reports the would-be sweep but deletes nothing.
    let dry = GcWorker::with_clock(
        ctx.upload_dir.clone(),
        FixedClock(base() + SignedDuration::from_hours(25)),
    );
    let report = dry.mark_and_sweep(&ctx.db, true).await.unwrap();
    assert_eq!(report.swept, 1, "dry run reports the would-be sweep");
    assert!(blob_exists(&ctx, &hash), "dry run deletes nothing");

    // The real sweep 25 h after the mark — past grace — byte-deletes.
    let past = GcWorker::with_clock(
        ctx.upload_dir.clone(),
        FixedClock(base() + SignedDuration::from_hours(25)),
    );
    let report = past.mark_and_sweep(&ctx.db, false).await.unwrap();
    assert_eq!(report.swept, 1);
    assert!(report.swept_bytes > 0);
    assert!(!blob_exists(&ctx, &hash), "past grace, the bytes are gone");
}

#[tokio::test]
async fn just_marked_blob_survives_the_client_release_window() {
    let ctx = setup().await;
    // Mirrors the S-C3 seam: a blob that answered `durable` had `collectable_since = None`, so
    // any later mark is strictly after the verdict; deletion is a full grace window out, and
    // the client's bounded 60 s verify→release window fits comfortably inside it.
    let bytes = b"just-marked-durable-blob";
    let hash = capsule_core::crypto::hash::hash_bytes(bytes).to_hex();
    write_blob(&ctx, &hash, bytes);

    let mark_at = base();
    GcWorker::with_clock(ctx.upload_dir.clone(), FixedClock(mark_at))
        .mark_and_sweep(&ctx.db, false)
        .await
        .unwrap();

    // 60 s after the mark (the client's release deadline) the bytes are still present.
    let release = GcWorker::with_clock(
        ctx.upload_dir.clone(),
        FixedClock(mark_at + SignedDuration::from_secs(60)),
    );
    let report = release.mark_and_sweep(&ctx.db, false).await.unwrap();
    assert_eq!(report.swept, 0);
    assert_eq!(report.retained_in_grace, 1);
    assert!(
        blob_exists(&ctx, &hash),
        "a just-marked blob survives the release window"
    );
}

#[tokio::test]
async fn reference_reappearing_in_grace_cancels_the_mark() {
    let ctx = setup().await;
    // Seed a stored asset, then hard-delete just the row to simulate a purge, so the original
    // blob is temporarily unreferenced and gets marked.
    let file_id = nanoid!();
    let (hash, _create) = seed_stored_asset(&ctx, &file_id, b"reappearing-reference").await;
    entity::asset::Entity::delete_by_id(&file_id)
        .exec(&ctx.db)
        .await
        .unwrap();

    // Mark the now-orphan blob.
    GcWorker::with_clock(ctx.upload_dir.clone(), FixedClock(base()))
        .mark_and_sweep(&ctx.db, false)
        .await
        .unwrap();
    let marked = entity::blob_gc::Entity::find_by_id(&hash)
        .one(&ctx.db)
        .await
        .unwrap()
        .unwrap();
    assert!(marked.collectable_since.is_some(), "orphan is marked");

    // A reference reappears (an in-flight finalization retry landing the row again).
    insert_asset_row(&ctx, &file_id, &hash, 20).await;

    // The next pass cancels the mark and the sweep never fires, even past the grace window.
    let report = GcWorker::with_clock(ctx.upload_dir.clone(), FixedClock(base() + days(2)))
        .mark_and_sweep(&ctx.db, false)
        .await
        .unwrap();
    assert_eq!(
        report.cancelled, 1,
        "reappearing reference cancels the mark"
    );
    assert_eq!(report.swept, 0);
    assert!(
        blob_exists(&ctx, &hash),
        "a re-referenced blob is never swept"
    );
    assert!(
        entity::blob_gc::Entity::find_by_id(&hash)
            .one(&ctx.db)
            .await
            .unwrap()
            .is_none(),
        "the cancelled mark row is cleared"
    );
}

#[tokio::test]
async fn quarantined_blob_is_never_swept() {
    let ctx = setup().await;
    let bytes = b"quarantined-ciphertext";
    let hash = capsule_core::crypto::hash::hash_bytes(bytes).to_hex();
    write_blob(&ctx, &hash, bytes);

    // Integrity-quarantined with a mark far in the past — well past any conceivable grace.
    entity::blob_gc::ActiveModel {
        content_hash: Set(hash.clone()),
        collectable_since: Set(Some(entity::time::ts_to_entity_tz(
            "2020-01-01T00:00:00Z".parse().unwrap(),
        ))),
        quarantined: Set(true),
    }
    .insert(&ctx.db)
    .await
    .unwrap();

    let report = GcWorker::with_clock(
        ctx.upload_dir.clone(),
        FixedClock("2030-01-01T00:00:00Z".parse().unwrap()),
    )
    .mark_and_sweep(&ctx.db, false)
    .await
    .unwrap();
    assert_eq!(report.swept, 0, "a quarantined blob is never swept");
    assert!(blob_exists(&ctx, &hash), "quarantined bytes are preserved");
}

#[tokio::test]
async fn dangling_reference_is_quarantined_never_deleted() {
    let ctx = setup().await;
    // A committed row referencing a blob hash absent from the store.
    let file_id = nanoid!();
    let missing = "de".repeat(32);
    insert_asset_row(&ctx, &file_id, &missing, 1024).await;

    let report = GcWorker::new(ctx.upload_dir.clone())
        .mark_and_sweep(&ctx.db, false)
        .await
        .unwrap();
    assert_eq!(report.dangling_quarantined, 1);
    assert_eq!(report.swept, 0);

    // The row is preserved — erasing it would destroy the only record the asset should exist.
    assert!(
        asset_present(&ctx, &file_id).await,
        "dangling ref never deletes the row"
    );
    let row = entity::blob_gc::Entity::find_by_id(&missing)
        .one(&ctx.db)
        .await
        .unwrap()
        .unwrap();
    assert!(row.quarantined, "dangling blob is quarantined");
    assert!(
        row.collectable_since.is_none(),
        "a missing blob is never treated as collectable"
    );
}

// ─────────────────────── end-to-end composition ─────────────────────────────

#[tokio::test]
async fn retention_purge_then_gc_reclaims_the_original_blob() {
    let ctx = setup().await;
    let svc = ctx.ops_service();
    let file_id = nanoid!();
    let (hash, create) = seed_stored_asset(&ctx, &file_id, b"lifecycle-original-bytes").await;

    // delete with a 30-day window.
    soft_delete(&ctx, &svc, &file_id, &create, base() + days(30)).await;

    // Retention purge past the window drops the reference (bytes still on disk).
    RetentionPurgeWorker::with_clock(FixedClock(base() + days(31)))
        .purge_expired(&ctx.db, false)
        .await
        .unwrap();
    assert!(!asset_present(&ctx, &file_id).await);
    assert_eq!(reference_count(&ctx.db, &hash).await.unwrap(), 0);
    assert!(blob_exists(&ctx, &hash), "bytes await GC");

    // GC marks the now-orphan original, then sweeps it a grace window later.
    let purged_at = base() + days(31);
    GcWorker::with_clock(ctx.upload_dir.clone(), FixedClock(purged_at))
        .mark_and_sweep(&ctx.db, false)
        .await
        .unwrap();
    assert!(
        blob_exists(&ctx, &hash),
        "still inside grace right after marking"
    );

    let report = GcWorker::with_clock(
        ctx.upload_dir.clone(),
        FixedClock(purged_at + SignedDuration::from_hours(25)),
    )
    .mark_and_sweep(&ctx.db, false)
    .await
    .unwrap();
    assert_eq!(report.swept, 1);
    assert!(
        !blob_exists(&ctx, &hash),
        "the deleted asset's bytes are finally reclaimed"
    );
}
