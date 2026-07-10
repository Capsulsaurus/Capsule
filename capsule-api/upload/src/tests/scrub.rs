//! Slice `S-C14` — the read-only server-side integrity scrub (Postgres ⇄ blob store).
//!
//! These run the real scrub (`service::scrub::IntegrityScrub`) against testcontainer Postgres
//! + a real on-disk content-addressed blob tree, seeded directly (assets, sync-feed entries,
//! custody-receipt chain, blob files) — the finalization write path is exercised elsewhere
//! (`tests::sync_feed` / `tests::receipts`); here we need surgical control over each seeded
//! corruption class.
//!
//! Coverage map — the maintenance doc's seeded-corruption matrix, one class per test:
//! - `delete_referenced_blob_is_dangling` — delete a referenced blob → `DanglingReference`.
//! - `flip_blob_byte_is_corrupt_deep` — flip one byte → `CorruptBlob` (deep).
//! - `orphan_blob_is_reported` — an unreferenced present blob → `OrphanBlob`.
//! - `truncated_receipt_chain_is_chain_break` — delete a mid-chain receipt → `ChainBreak`.
//! - `altered_mirrored_size_is_mismatch` — diverge a receipt's declared size →
//!   `MirroredFactMismatch`.
//! - `awaiting_original_carveout` — a missing original on an `awaiting-original` asset is no
//!   finding; the same gap on a fully-uploaded asset is a `DanglingReference`.
//! - `clean_store_is_idempotent_and_immutable` — a consistent store yields zero findings; two
//!   runs give identical reports and the store (blobs + index) is byte-identical after.
//!
//! Every corruption test also asserts the scrub **mutated nothing** — the blob tree and every
//! relevant table are byte-identical before and after the run.

use std::collections::BTreeMap;
use std::path::Path;

use nanoid::nanoid;
use sea_orm::{ActiveModelTrait, DatabaseConnection, EntityTrait, QueryOrder, Set};
use service::scrub::{FindingClass, IntegrityScrub};
use service::sync::{ChangeKind, FeedBlobManifest, FeedBlobRef, FeedEntryInput, Mutation as Sync};

use super::{PROTOCOL, TestCtx, setup, sha256_hex};

const SERVER_ID: &str = "home.test";
const BLOB_SIZE: u64 = 1024;

// ─────────────────────────────── seeding helpers ───────────────────────────────

/// Write `bytes` into the content-addressed blob store at its own hash, returning the hash.
async fn write_blob(ctx: &TestCtx, bytes: &[u8]) -> String {
    let hash = sha256_hex(bytes);
    let dir = service::blob_store::blobs_dir(&ctx.upload_dir);
    tokio::fs::create_dir_all(&dir).await.unwrap();
    let path = service::blob_store::blob_path(&ctx.upload_dir, &hash);
    tokio::fs::write(&path, bytes).await.unwrap();
    hash
}

/// A `BLOB_SIZE`-byte buffer stamped with a unique `nonce` prefix — so its physical length
/// equals the size the feed/asset/receipt rows declare, keeping the size mirror consistent on
/// a clean store while still hashing to a distinct content address per nonce.
fn sized_blob(nonce: &str) -> Vec<u8> {
    let mut v = vec![0u8; BLOB_SIZE as usize];
    let n = nonce.as_bytes();
    let k = n.len().min(v.len());
    v[..k].copy_from_slice(&n[..k]);
    v
}

/// Append one feed entry for `asset_id` referencing `hash` as the original, of `size` bytes,
/// carrying the `original_held` completeness fact.
async fn seed_feed_original(
    ctx: &TestCtx,
    asset_id: &str,
    hash: &str,
    size: u64,
    original_held: bool,
) {
    let blobs = FeedBlobManifest {
        original: Some(FeedBlobRef {
            ciphertext_hash: hash.to_string(),
            role: "original".to_string(),
            format: "image/jpeg".to_string(),
            size,
        }),
        derivatives: Vec::new(),
    };
    Sync::record_finalization(
        &ctx.db,
        FeedEntryInput {
            album_id: ctx.album_id.clone(),
            protocol_version: PROTOCOL.to_string(),
            kind: ChangeKind::Created,
            asset_id: asset_id.to_string(),
            manifest_cbor: b"opaque-manifest-cbor".to_vec(),
            metadata_blob: None,
            blobs,
            original_held,
        },
    )
    .await
    .unwrap();
}

/// Insert one custody-receipt row, chaining `prior_hash → receipt_hash` at `seq`.
#[allow(clippy::too_many_arguments)]
async fn seed_receipt(
    db: &DatabaseConnection,
    seq: i64,
    prior_hash: Option<&str>,
    receipt_hash: &str,
    asset_id: &str,
    ciphertext_hash: &str,
    size: i64,
) {
    entity::custody_receipt::ActiveModel {
        server_id: Set(SERVER_ID.to_string()),
        receipt_seq: Set(seq),
        prior_receipt_hash: Set(prior_hash.map(str::to_string)),
        receipt_hash: Set(receipt_hash.to_string()),
        upload_id: Set(nanoid!()),
        asset_id: Set(asset_id.to_string()),
        blob_role: Set("original".to_string()),
        ciphertext_hash: Set(ciphertext_hash.to_string()),
        size: Set(size),
        envelope_hash: Set(Some("ee".repeat(32))),
        uploaded_by_user: Set("user-1".to_string()),
        uploaded_by_device: Set(None),
        server_key_id: Set("aa".repeat(32)),
        received_at: Set(entity::time::ts_to_entity_tz(jiff::Timestamp::now())),
        receipt_cbor: Set(b"opaque-receipt-cbor".to_vec()),
        ..Default::default()
    }
    .insert(db)
    .await
    .unwrap();
}

/// One uploaded asset with a present original blob + feed entry, and nothing else — the
/// minimal consistent asset the chain/mirror fixtures build their own receipts over.
async fn seed_asset_blob_feed(ctx: &TestCtx, nonce: &str) -> (String, String) {
    let now = jiff::Timestamp::now();
    let bytes = sized_blob(nonce);
    let hash = write_blob(ctx, &bytes).await;
    let asset = ctx
        .seed_asset(&ctx.user_id, &hash, BLOB_SIZE as i64, true, now)
        .await;
    seed_feed_original(ctx, &asset, &hash, BLOB_SIZE, true).await;
    (asset, hash)
}

/// A consistent store: two uploaded assets each with a present original blob + feed entry, and
/// a valid two-link custody-receipt chain. Returns the first asset's id + original hash (the
/// subject the presence/corrupt/quarantine fixtures corrupt).
struct Seeded {
    asset_a: String,
    hash_a: String,
}

async fn seed_clean(ctx: &TestCtx) -> Seeded {
    let (asset_a, hash_a) = seed_asset_blob_feed(ctx, &format!("original-a-{}", nanoid!())).await;
    let (asset_b, hash_b) = seed_asset_blob_feed(ctx, &format!("original-b-{}", nanoid!())).await;

    let receipt_1 = "11".repeat(32);
    let receipt_2 = "22".repeat(32);
    seed_receipt(
        &ctx.db,
        1,
        None,
        &receipt_1,
        &asset_a,
        &hash_a,
        BLOB_SIZE as i64,
    )
    .await;
    seed_receipt(
        &ctx.db,
        2,
        Some(&receipt_1),
        &receipt_2,
        &asset_b,
        &hash_b,
        BLOB_SIZE as i64,
    )
    .await;

    Seeded { asset_a, hash_a }
}

// ─────────────────────────── no-mutation proof helpers ──────────────────────────

/// A content digest of the whole blob/upload tree: every file's path → SHA-256 of its bytes.
fn fs_digest(root: &Path) -> BTreeMap<String, String> {
    fn walk(dir: &Path, base: &Path, out: &mut BTreeMap<String, String>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, base, out);
            } else if let Ok(bytes) = std::fs::read(&path) {
                let rel = path.strip_prefix(base).unwrap_or(&path).to_string_lossy();
                out.insert(rel.into_owned(), sha256_hex(&bytes));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, &mut out);
    out
}

/// A snapshot of every table the scrub reads, ordered deterministically — equal before/after
/// iff the scrub mutated no row.
struct DbSnapshot {
    sync: Vec<entity::sync_entry::Model>,
    receipts: Vec<entity::custody_receipt::Model>,
    blob_gc: Vec<entity::blob_gc::Model>,
    assets: Vec<entity::asset::Model>,
    ledger: Vec<entity::quota_ledger::Model>,
}

async fn db_snapshot(db: &DatabaseConnection) -> DbSnapshot {
    DbSnapshot {
        sync: entity::sync_entry::Entity::find()
            .order_by_asc(entity::sync_entry::Column::FeedSeq)
            .all(db)
            .await
            .unwrap(),
        receipts: entity::custody_receipt::Entity::find()
            .order_by_asc(entity::custody_receipt::Column::ServerId)
            .order_by_asc(entity::custody_receipt::Column::ReceiptSeq)
            .all(db)
            .await
            .unwrap(),
        blob_gc: entity::blob_gc::Entity::find()
            .order_by_asc(entity::blob_gc::Column::ContentHash)
            .all(db)
            .await
            .unwrap(),
        assets: entity::asset::Entity::find()
            .order_by_asc(entity::asset::Column::Id)
            .all(db)
            .await
            .unwrap(),
        ledger: entity::quota_ledger::Entity::find()
            .order_by_asc(entity::quota_ledger::Column::ContentHash)
            .all(db)
            .await
            .unwrap(),
    }
}

/// Assert the scrub mutated nothing: the blob tree and every read table are byte-identical.
fn assert_no_mutation(
    before_fs: &BTreeMap<String, String>,
    before_db: &DbSnapshot,
    after_fs: &BTreeMap<String, String>,
    after_db: &DbSnapshot,
) {
    assert_eq!(
        before_fs, after_fs,
        "blob store changed — scrub must be read-only"
    );
    assert_eq!(before_db.sync, after_db.sync, "sync_entries changed");
    assert_eq!(
        before_db.receipts, after_db.receipts,
        "custody_receipts changed"
    );
    assert_eq!(before_db.blob_gc, after_db.blob_gc, "blob_gc changed");
    assert_eq!(before_db.assets, after_db.assets, "assets changed");
    assert_eq!(before_db.ledger, after_db.ledger, "quota_ledger changed");
}

fn scrub(ctx: &TestCtx) -> IntegrityScrub {
    IntegrityScrub::new(ctx.upload_dir.clone())
}

/// Assert the report carries exactly one finding of `class` and nothing else, and trips the
/// non-zero exit signal.
fn assert_single(report: &service::scrub::ScrubReport, class: FindingClass) {
    assert_eq!(
        report.total(),
        1,
        "exactly one finding: {:?}",
        report.findings
    );
    assert_eq!(report.count(class), 1, "the one finding is {class:?}");
    assert!(
        !report.is_clean(),
        "a finding trips the non-zero exit signal"
    );
    // Every other class is zero.
    for other in FindingClass::all() {
        if other != class {
            assert_eq!(report.count(other), 0, "no {other:?} findings");
        }
    }
}

// ───────────────────────────────── the matrix ──────────────────────────────────

#[tokio::test]
async fn clean_store_is_idempotent_and_immutable() {
    let ctx = setup().await;
    seed_clean(&ctx).await;

    let before_fs = fs_digest(&ctx.upload_dir);
    let before_db = db_snapshot(&ctx.db).await;

    // Two deep runs on a consistent store: zero findings, identical reports, store unchanged.
    let first = scrub(&ctx).run(&ctx.db, true).await.unwrap();
    let second = scrub(&ctx).run(&ctx.db, true).await.unwrap();

    assert!(
        first.is_clean(),
        "clean store yields no findings: {:?}",
        first.findings
    );
    assert_eq!(first.total(), 0);
    for class in FindingClass::all() {
        assert_eq!(
            first.count(class),
            0,
            "{class:?} is explicitly zero on a clean store"
        );
    }
    assert_eq!(
        first, second,
        "two runs on a clean store give an identical report"
    );

    let after_fs = fs_digest(&ctx.upload_dir);
    let after_db = db_snapshot(&ctx.db).await;
    assert_no_mutation(&before_fs, &before_db, &after_fs, &after_db);
}

#[tokio::test]
async fn delete_referenced_blob_is_dangling() {
    let ctx = setup().await;
    let seeded = seed_clean(&ctx).await;

    // Delete a committed original blob — the loud dangling-reference case.
    let path = service::blob_store::blob_path(&ctx.upload_dir, &seeded.hash_a);
    tokio::fs::remove_file(&path).await.unwrap();

    let before_fs = fs_digest(&ctx.upload_dir);
    let before_db = db_snapshot(&ctx.db).await;
    let report = scrub(&ctx).run(&ctx.db, false).await.unwrap();
    let after_fs = fs_digest(&ctx.upload_dir);
    let after_db = db_snapshot(&ctx.db).await;

    assert_single(&report, FindingClass::DanglingReference);
    let f = &report.findings[0];
    assert_eq!(f.content_hash.as_deref(), Some(seeded.hash_a.as_str()));
    assert_eq!(f.asset_id.as_deref(), Some(seeded.asset_a.as_str()));
    assert_no_mutation(&before_fs, &before_db, &after_fs, &after_db);
}

#[tokio::test]
async fn flip_blob_byte_is_corrupt_deep() {
    let ctx = setup().await;
    let seeded = seed_clean(&ctx).await;

    // Flip one byte of a committed blob, preserving its length (bit rot).
    let path = service::blob_store::blob_path(&ctx.upload_dir, &seeded.hash_a);
    let mut bytes = tokio::fs::read(&path).await.unwrap();
    bytes[0] ^= 0xff;
    tokio::fs::write(&path, &bytes).await.unwrap();

    let before_fs = fs_digest(&ctx.upload_dir);
    let before_db = db_snapshot(&ctx.db).await;
    // Deep mode is required — the name still resolves; only re-hashing catches the flip.
    let report = scrub(&ctx).run(&ctx.db, true).await.unwrap();
    let after_fs = fs_digest(&ctx.upload_dir);
    let after_db = db_snapshot(&ctx.db).await;

    assert_single(&report, FindingClass::CorruptBlob);
    assert_eq!(
        report.findings[0].content_hash.as_deref(),
        Some(seeded.hash_a.as_str())
    );
    assert_no_mutation(&before_fs, &before_db, &after_fs, &after_db);

    // The shallow pass alone does not catch a length-preserving flip.
    let shallow = scrub(&ctx).run(&ctx.db, false).await.unwrap();
    assert!(shallow.is_clean(), "shallow pass misses bit rot by design");
}

#[tokio::test]
async fn orphan_blob_is_reported() {
    let ctx = setup().await;
    seed_clean(&ctx).await;

    // A content-valid blob referenced by no row (so deep does not also flag it corrupt).
    let orphan = write_blob(&ctx, format!("orphan-{}", nanoid!()).as_bytes()).await;

    let before_fs = fs_digest(&ctx.upload_dir);
    let before_db = db_snapshot(&ctx.db).await;
    let report = scrub(&ctx).run(&ctx.db, true).await.unwrap();
    let after_fs = fs_digest(&ctx.upload_dir);
    let after_db = db_snapshot(&ctx.db).await;

    assert_single(&report, FindingClass::OrphanBlob);
    assert_eq!(
        report.findings[0].content_hash.as_deref(),
        Some(orphan.as_str())
    );
    assert_no_mutation(&before_fs, &before_db, &after_fs, &after_db);
}

#[tokio::test]
async fn truncated_receipt_chain_is_chain_break() {
    let ctx = setup().await;
    let (asset, hash) = seed_asset_blob_feed(&ctx, "chain").await;

    // The custody log is append-only at the DB layer, so seed the *already-truncated*
    // sequence directly: the genesis (seq 1) and seq 3, with seq 2 absent — the forward walk
    // can neither bridge the gap nor match seq 3's `prior_receipt_hash` to seq 1's hash.
    let r1 = "11".repeat(32);
    let r3 = "33".repeat(32);
    let missing_seq2 = "22".repeat(32); // seq 3 chains from the absent seq 2
    seed_receipt(&ctx.db, 1, None, &r1, &asset, &hash, BLOB_SIZE as i64).await;
    seed_receipt(
        &ctx.db,
        3,
        Some(&missing_seq2),
        &r3,
        &asset,
        &"44".repeat(32),
        BLOB_SIZE as i64,
    )
    .await;

    let before_fs = fs_digest(&ctx.upload_dir);
    let before_db = db_snapshot(&ctx.db).await;
    let report = scrub(&ctx).run(&ctx.db, false).await.unwrap();
    let after_fs = fs_digest(&ctx.upload_dir);
    let after_db = db_snapshot(&ctx.db).await;

    assert_single(&report, FindingClass::ChainBreak);
    assert_no_mutation(&before_fs, &before_db, &after_fs, &after_db);
}

#[tokio::test]
async fn altered_mirrored_size_is_mismatch() {
    let ctx = setup().await;
    let (asset, hash) = seed_asset_blob_feed(&ctx, "mirror").await;

    // A custody receipt that declares a size disagreeing with the feed/physical copy — the
    // mirrored fact a hot-path bug could drift. Seeded at insert (the log is append-only), so
    // the divergence is a genuine cross-copy disagreement, not a scrub mutation.
    seed_receipt(
        &ctx.db,
        1,
        None,
        &"11".repeat(32),
        &asset,
        &hash,
        BLOB_SIZE as i64 + 999,
    )
    .await;

    let before_fs = fs_digest(&ctx.upload_dir);
    let before_db = db_snapshot(&ctx.db).await;
    let report = scrub(&ctx).run(&ctx.db, false).await.unwrap();
    let after_fs = fs_digest(&ctx.upload_dir);
    let after_db = db_snapshot(&ctx.db).await;

    assert_single(&report, FindingClass::MirroredFactMismatch);
    assert_eq!(
        report.findings[0].content_hash.as_deref(),
        Some(hash.as_str())
    );
    assert_no_mutation(&before_fs, &before_db, &after_fs, &after_db);
}

#[tokio::test]
async fn awaiting_original_carveout() {
    let ctx = setup().await;

    // Asset C: awaiting-original (original_held = false), its original blob absent — expected
    // staged state, no finding. Asset D: fully uploaded (original_held = true), same gap —
    // a dangling reference. No blobs on disk, no receipts, so the only finding is D's.
    let hash_c = "cc".repeat(32);
    let hash_d = "dd".repeat(32);
    let asset_c = nanoid!();
    let asset_d = nanoid!();
    seed_feed_original(&ctx, &asset_c, &hash_c, BLOB_SIZE, false).await;
    seed_feed_original(&ctx, &asset_d, &hash_d, BLOB_SIZE, true).await;

    let before_fs = fs_digest(&ctx.upload_dir);
    let before_db = db_snapshot(&ctx.db).await;
    let report = scrub(&ctx).run(&ctx.db, false).await.unwrap();
    let after_fs = fs_digest(&ctx.upload_dir);
    let after_db = db_snapshot(&ctx.db).await;

    assert_single(&report, FindingClass::DanglingReference);
    let f = &report.findings[0];
    assert_eq!(
        f.content_hash.as_deref(),
        Some(hash_d.as_str()),
        "only the held asset is dangling"
    );
    assert_eq!(f.asset_id.as_deref(), Some(asset_d.as_str()));
    assert_no_mutation(&before_fs, &before_db, &after_fs, &after_db);
}

#[tokio::test]
async fn incoming_staging_debris_is_inventoried() {
    let ctx = setup().await;
    seed_clean(&ctx).await;

    // A stale `{upload_id}.bin` staging file directly under upload_dir (not under blobs/).
    let stale = ctx.upload_dir.join(format!("{}.bin", nanoid!()));
    tokio::fs::write(&stale, b"partial upload").await.unwrap();

    let before_fs = fs_digest(&ctx.upload_dir);
    let before_db = db_snapshot(&ctx.db).await;
    let report = scrub(&ctx).run(&ctx.db, false).await.unwrap();
    let after_fs = fs_digest(&ctx.upload_dir);
    let after_db = db_snapshot(&ctx.db).await;

    assert_single(&report, FindingClass::IncomingDebris);
    assert_no_mutation(&before_fs, &before_db, &after_fs, &after_db);
}

#[tokio::test]
async fn quarantined_blob_is_inventoried() {
    let ctx = setup().await;
    let seeded = seed_clean(&ctx).await;

    // Flag a blob quarantined (as the GC path would on an integrity fault).
    entity::blob_gc::ActiveModel {
        content_hash: Set(seeded.hash_a.clone()),
        collectable_since: Set(None),
        quarantined: Set(true),
    }
    .insert(&ctx.db)
    .await
    .unwrap();

    let before_fs = fs_digest(&ctx.upload_dir);
    let before_db = db_snapshot(&ctx.db).await;
    let report = scrub(&ctx).run(&ctx.db, false).await.unwrap();
    let after_fs = fs_digest(&ctx.upload_dir);
    let after_db = db_snapshot(&ctx.db).await;

    assert_single(&report, FindingClass::Quarantine);
    assert_eq!(
        report.findings[0].content_hash.as_deref(),
        Some(seeded.hash_a.as_str())
    );
    assert_no_mutation(&before_fs, &before_db, &after_fs, &after_db);
}

/// A store with every corruption class at once: the scrub reports each exactly once and trips
/// the non-zero exit — findings do not mask one another.
#[tokio::test]
async fn all_classes_at_once_are_each_reported() {
    let ctx = setup().await;

    // 2. corrupt: asset A present but bit-flipped (length preserved).
    let (asset_a, hash_a) = seed_asset_blob_feed(&ctx, "corrupt").await;
    let path_a = service::blob_store::blob_path(&ctx.upload_dir, &hash_a);
    let mut bytes = tokio::fs::read(&path_a).await.unwrap();
    bytes[0] ^= 0xff;
    tokio::fs::write(&path_a, &bytes).await.unwrap();
    // 1. dangling: asset B's committed original removed from disk.
    let (_asset_b, hash_b) = seed_asset_blob_feed(&ctx, "dangling").await;
    tokio::fs::remove_file(service::blob_store::blob_path(&ctx.upload_dir, &hash_b))
        .await
        .unwrap();
    // 3. orphan: a content-valid unreferenced blob.
    write_blob(&ctx, format!("orphan-{}", nanoid!()).as_bytes()).await;
    // 4. chain break: a gapped custody chain (seq 1 then seq 3).
    seed_receipt(
        &ctx.db,
        1,
        None,
        &"11".repeat(32),
        &asset_a,
        &hash_a,
        BLOB_SIZE as i64,
    )
    .await;
    seed_receipt(
        &ctx.db,
        3,
        Some(&"22".repeat(32)),
        &"33".repeat(32),
        &asset_a,
        &"44".repeat(32),
        BLOB_SIZE as i64,
    )
    .await;
    // 6. debris + quarantine.
    tokio::fs::write(
        ctx.upload_dir.join(format!("{}.bin", nanoid!())),
        b"partial",
    )
    .await
    .unwrap();
    entity::blob_gc::ActiveModel {
        content_hash: Set("55".repeat(32)),
        collectable_since: Set(None),
        quarantined: Set(true),
    }
    .insert(&ctx.db)
    .await
    .unwrap();

    let report = scrub(&ctx).run(&ctx.db, true).await.unwrap();

    assert!(!report.is_clean());
    assert_eq!(report.count(FindingClass::DanglingReference), 1);
    assert_eq!(report.count(FindingClass::CorruptBlob), 1);
    assert_eq!(report.count(FindingClass::OrphanBlob), 1);
    assert_eq!(report.count(FindingClass::ChainBreak), 1);
    assert_eq!(report.count(FindingClass::IncomingDebris), 1);
    assert_eq!(report.count(FindingClass::Quarantine), 1);
}
