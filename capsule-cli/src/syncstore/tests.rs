use std::collections::BTreeSet;

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::{AmkVersion, HybridSigningKey};
use capsule_core::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use capsule_core::crypto::provenance::{ASSET_MANIFEST_VERSION, KeyMode, ManifestCore};
use capsule_sdk::sync::{BlobManifest, FeedEntry};
use entity::peering_tombstone;
use migration::{Migrator, MigratorTrait};
use sea_orm::{Database, DatabaseConnection};

use super::*;
use crate::peering_state::load_tombstones;

/// The device clock the round-trip tests land pages at.
const EPOCH: Timestamp = Timestamp::UNIX_EPOCH;

async fn connect(path: &std::path::Path) -> DatabaseConnection {
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let db = Database::connect(&url).await.expect("connect sqlite");
    Migrator::up(&db, None).await.expect("migrate");
    db
}

async fn temp_db() -> (DatabaseConnection, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "capsule-cli-syncstore-{}.sqlite",
        nanoid::nanoid!()
    ));
    (connect(&path).await, path)
}

fn ts(text: &str) -> Timestamp {
    text.parse().expect("a timestamp")
}

/// The canonical CBOR of `asset`'s signed provenance record for `action`, naming `retention_until`.
fn record(asset: Uuid, action: Action, retention_until: Option<&str>) -> Vec<u8> {
    let signer = HybridSigningKey::from_seed64(&[7; 64]);
    let core = ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL_VERSION.into(),
        file_id: asset,
        album_id: Uuid::from_u128(0xA1),
        amk_version: AmkVersion(1),
        ciphertext_hash: Hash32([1; 32]),
        plaintext_size: 1,
        chunk_size: 1,
        nonce_prefix: [0; 7],
        key_mode: KeyMode::Derived,
        wrapped_file_key: None,
        metadata_blob_hash: None,
        created_by_user: Uuid::from_u128(0xB1),
        created_by_device: Uuid::from_u128(0xD1),
        client_version: "t".into(),
        timestamp: "2026-01-01T00:00:00Z".into(),
        action,
        prior_provenance_hash: Some(Hash32([2; 32])),
        retention_until: retention_until.map(str::to_string),
        upgraded_from: None,
        unknown: Default::default(),
    };
    let manifest = core.sign(&signer, &signer).expect("signs");
    let record = ProvenanceRecord::new(asset, manifest, Some(Hash32([2; 32])));
    capsule_core::cbor::to_canonical_vec(&record).expect("encodes")
}

/// A `Deleted` entry for `asset` carrying `manifest_cbor`, changed at `changed_at`.
fn delete(asset: Uuid, seq: u64, manifest_cbor: Vec<u8>, changed_at: &str) -> FeedEntry {
    FeedEntry {
        manifest_cbor,
        changed_at: changed_at.to_string(),
        ..entry(b"album", asset.as_bytes(), seq, ChangeKind::Deleted)
    }
}

fn retention_stored(rows: &[synced_asset::Model], asset: Uuid) -> Option<i64> {
    rows.iter()
        .find(|row| row.asset_id == encode_id(asset.as_bytes()))
        .expect("a row")
        .retention_until
}

fn entry(album: &[u8], asset: &[u8], seq: u64, kind: ChangeKind) -> FeedEntry {
    FeedEntry {
        album_id: album.to_vec(),
        sync_seq: seq,
        protocol_version: "2026-05-31".to_string(),
        kind,
        asset_id: asset.to_vec(),
        manifest_cbor: vec![0xa0],
        metadata_blob: Vec::new(),
        blobs: BlobManifest::default(),
        derivative_manifests: Vec::new(),
        original_held: true,
        changed_at: "1970-01-01T00:00:00Z".to_string(),
    }
}

fn page(entries: Vec<FeedEntry>, cursor_tag: u8) -> SyncPage {
    SyncPage {
        entries,
        next_cursor: SyncCursor::from_bytes(vec![cursor_tag; 41]),
        has_more: false,
    }
}

/// Persisting a page and reloading the state round-trips the cursor and the
/// derived per-album high-water marks across a simulated process restart.
#[tokio::test]
async fn persist_then_reload_round_trips_cursor_and_high_water() {
    let (db, path) = temp_db().await;
    let album_a = b"album-a".to_vec();
    let album_b = b"album-b".to_vec();

    // A fresh store starts at the sentinel with no high-water marks.
    let mut state = load_sync_state(&db, "2026-12-31").await.unwrap();
    assert!(state.cursor().is_start());

    let page1 = page(
        vec![
            entry(&album_a, b"a1", 1, ChangeKind::Created),
            entry(&album_a, b"a2", 2, ChangeKind::Created),
            entry(&album_b, b"b1", 7, ChangeKind::Created),
        ],
        0x11,
    );
    state.apply_page(&page1).unwrap();
    let written = persist_page(&db, &page1, EPOCH).await.unwrap();
    assert_eq!(written, 3);

    // Simulate a restart: reload the state from disk.
    let reloaded = load_sync_state(&db, "2026-12-31").await.unwrap();
    assert_eq!(reloaded.cursor().as_bytes(), &[0x11u8; 41]);
    assert_eq!(reloaded.high_water(&album_a), Some(2));
    assert_eq!(reloaded.high_water(&album_b), Some(7));

    // Landed assets are queryable client-side.
    let listed = list_assets(&db, false).await.unwrap();
    assert_eq!(listed.len(), 3);
    assert!(listed.iter().all(|row| !row.tombstoned));

    let _ = std::fs::remove_file(path);
}

/// A reset for a new sync epoch drops the cursor and the floor and keeps every row (#697).
#[tokio::test]
async fn a_reset_for_a_new_epoch_drops_the_floor_and_keeps_the_rows() {
    let (db, path) = temp_db().await;
    let album = b"album-e".to_vec();
    let held = page(
        vec![
            entry(&album, b"e1", 8, ChangeKind::Created),
            entry(&album, b"e2", 9, ChangeKind::Created),
        ],
        0x31,
    );
    persist_page(&db, &held, EPOCH).await.unwrap();

    reset_for_resync(&db).await.unwrap();

    let mut reloaded = load_sync_state(&db, "2026-12-31").await.unwrap();
    assert!(reloaded.cursor().is_start());
    assert_eq!(reloaded.high_water(&album), Some(0));
    assert_eq!(list_assets(&db, true).await.unwrap().len(), 2);

    // The rebuilt numbering, lower than what was held, applies.
    let rebuilt = page(vec![entry(&album, b"e1", 3, ChangeKind::Created)], 0x32);
    reloaded.apply_page(&rebuilt).unwrap();
    persist_page(&db, &rebuilt, EPOCH).await.unwrap();
    let reloaded = load_sync_state(&db, "2026-12-31").await.unwrap();
    assert_eq!(reloaded.high_water(&album), Some(3));

    let _ = std::fs::remove_file(path);
}

/// A metadata update advances the same asset row (upsert, not duplicate), and a
/// delete tombstones it — excluded from the default listing but still counted
/// toward the album high-water mark.
#[tokio::test]
async fn upsert_advances_and_delete_tombstones() {
    let (db, path) = temp_db().await;
    let album = b"album".to_vec();

    let p1 = page(vec![entry(&album, b"x", 1, ChangeKind::Created)], 0x21);
    persist_page(&db, &p1, EPOCH).await.unwrap();
    let p2 = page(vec![entry(&album, b"x", 2, ChangeKind::Updated)], 0x22);
    persist_page(&db, &p2, EPOCH).await.unwrap();

    // One row (upserted), now at seq 2.
    let all = list_assets(&db, true).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].sync_seq, 2);
    assert_eq!(all[0].kind, "metadata_updated");

    // Delete tombstones it.
    let p3 = page(vec![entry(&album, b"x", 3, ChangeKind::Deleted)], 0x23);
    persist_page(&db, &p3, EPOCH).await.unwrap();

    assert!(
        list_assets(&db, false).await.unwrap().is_empty(),
        "tombstoned assets are hidden by default"
    );
    assert_eq!(
        list_assets(&db, true).await.unwrap().len(),
        1,
        "tombstoned assets are still present when requested"
    );
    // The high-water mark still reflects the delete's sequence.
    let reloaded = load_sync_state(&db, "2026-12-31").await.unwrap();
    assert_eq!(reloaded.high_water(&album), Some(3));

    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn a_delete_whose_window_has_closed_is_a_tombstone_after_a_restart() {
    let (db, path) = temp_db().await;
    let closed = Uuid::from_u128(1);
    let open = Uuid::from_u128(2);
    let by_text = Uuid::from_u128(3);
    let mut by_text_entry = delete(
        by_text,
        3,
        record(by_text, Action::Delete, Some("2026-01-01T00:00:00Z")),
        "2025-12-01T00:00:00Z",
    );
    by_text_entry.asset_id = by_text.hyphenated().to_string().into_bytes();
    let landed = page(
        vec![
            delete(
                closed,
                1,
                record(closed, Action::Delete, Some("2026-01-01T00:00:00Z")),
                "2025-12-01T00:00:00Z",
            ),
            delete(
                open,
                2,
                record(open, Action::Delete, Some("2026-03-01T00:00:00Z")),
                "2025-12-01T00:00:00Z",
            ),
            by_text_entry,
        ],
        0x31,
    );
    persist_page(&db, &landed, ts("2026-02-01T00:00:00Z"))
        .await
        .unwrap();
    drop(db);

    let db = connect(&path).await;
    assert_eq!(
        load_tombstones(&db).await.unwrap(),
        BTreeSet::from([closed, by_text]),
        "only the deletes whose window has closed are purged"
    );
    let rows = synced_asset::Entity::find().all(&db).await.unwrap();
    assert_eq!(
        retention_stored(&rows, open),
        Some(ts("2026-03-01T00:00:00Z").as_second())
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn a_restore_inside_the_window_clears_the_deadline_and_leaves_no_tombstone() {
    let (db, path) = temp_db().await;
    let asset = Uuid::from_u128(4);
    let deleted = page(
        vec![delete(
            asset,
            1,
            record(asset, Action::Delete, Some("2026-03-01T00:00:00Z")),
            "2026-01-01T00:00:00Z",
        )],
        0x41,
    );
    persist_page(&db, &deleted, ts("2026-01-01T00:00:00Z"))
        .await
        .unwrap();
    let restored = page(
        vec![entry(b"album", asset.as_bytes(), 2, ChangeKind::Updated)],
        0x42,
    );
    persist_page(&db, &restored, ts("2026-01-02T00:00:00Z"))
        .await
        .unwrap();

    let rows = synced_asset::Entity::find().all(&db).await.unwrap();
    assert_eq!(retention_stored(&rows, asset), None);
    assert_eq!(
        record_elapsed_purges(&db, ts("2027-01-01T00:00:00Z"))
            .await
            .unwrap(),
        0,
        "a restored asset is out of the sweep however late it runs"
    );
    assert!(
        peering_tombstone::Entity::find()
            .all(&db)
            .await
            .unwrap()
            .is_empty()
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn a_window_closes_at_its_deadline_and_not_before() {
    let (db, path) = temp_db().await;
    let asset = Uuid::from_u128(5);
    let deleted = page(
        vec![delete(
            asset,
            1,
            record(asset, Action::Delete, Some("2026-01-01T00:00:00.5Z")),
            "1970-01-01T00:00:00Z",
        )],
        0x51,
    );
    persist_page(&db, &deleted, ts("2026-01-01T00:00:00Z"))
        .await
        .unwrap();
    assert!(load_tombstones(&db).await.unwrap().is_empty());
    assert_eq!(
        record_elapsed_purges(&db, ts("2026-01-01T00:00:00.9Z"))
            .await
            .unwrap(),
        0,
        "a fractional deadline is rounded up, never down"
    );
    assert_eq!(
        record_elapsed_purges(&db, ts("2026-01-01T00:00:01Z"))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        record_elapsed_purges(&db, ts("2026-01-02T00:00:00Z"))
            .await
            .unwrap(),
        0,
        "a recorded purge leaves the sweep"
    );
    let rows = synced_asset::Entity::find().all(&db).await.unwrap();
    assert_eq!(retention_stored(&rows, asset), None);
    assert_eq!(load_tombstones(&db).await.unwrap(), BTreeSet::from([asset]));
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn the_server_clock_in_the_page_floors_a_slow_device_clock() {
    let (db, path) = temp_db().await;
    let asset = Uuid::from_u128(6);
    let other = Uuid::from_u128(7);
    let landed = page(
        vec![
            delete(
                asset,
                1,
                record(asset, Action::Delete, Some("2026-02-01T00:00:00Z")),
                "2026-01-01T00:00:00Z",
            ),
            FeedEntry {
                changed_at: "2026-03-01T00:00:00Z".to_string(),
                ..entry(b"album", other.as_bytes(), 2, ChangeKind::Created)
            },
        ],
        0x61,
    );
    persist_page(&db, &landed, EPOCH).await.unwrap();
    assert_eq!(load_tombstones(&db).await.unwrap(), BTreeSet::from([asset]));
    let tombstone = peering_tombstone::Entity::find_by_id(asset.hyphenated().to_string())
        .one(&db)
        .await
        .unwrap()
        .expect("a row");
    assert_eq!(
        tombstone.recorded_at,
        ts("2026-03-01T00:00:00Z").to_string(),
        "the purge is recorded at the time it was judged at"
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn a_delete_served_without_a_manifest_is_already_purged() {
    let (db, path) = temp_db().await;
    let asset = Uuid::from_u128(8);
    let landed = page(
        vec![
            delete(asset, 1, Vec::new(), "2026-01-01T00:00:00Z"),
            FeedEntry {
                manifest_cbor: Vec::new(),
                ..entry(b"album", b"not-a-uuid", 2, ChangeKind::Deleted)
            },
        ],
        0x71,
    );
    persist_page(&db, &landed, EPOCH).await.unwrap();
    assert_eq!(
        load_tombstones(&db).await.unwrap(),
        BTreeSet::from([asset]),
        "an id no peer can name is skipped, not an error"
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn a_delete_this_device_cannot_read_stays_in_trash() {
    let (db, path) = temp_db().await;
    let unreadable = Uuid::from_u128(9);
    let foreign = Uuid::from_u128(10);
    let not_a_delete = Uuid::from_u128(11);
    let no_deadline = Uuid::from_u128(12);
    let bad_deadline = Uuid::from_u128(13);
    let past = Some("2000-01-01T00:00:00Z");
    let landed = page(
        vec![
            delete(unreadable, 1, vec![0xa0], "2026-01-01T00:00:00Z"),
            delete(
                foreign,
                2,
                record(Uuid::from_u128(99), Action::Delete, past),
                "2026-01-01T00:00:00Z",
            ),
            delete(
                not_a_delete,
                3,
                record(not_a_delete, Action::MetadataUpdate, past),
                "2026-01-01T00:00:00Z",
            ),
            delete(
                no_deadline,
                4,
                record(no_deadline, Action::Delete, None),
                "2026-01-01T00:00:00Z",
            ),
            delete(
                bad_deadline,
                5,
                record(bad_deadline, Action::Delete, Some("soon")),
                "2026-01-01T00:00:00Z",
            ),
        ],
        0x81,
    );
    persist_page(&db, &landed, ts("2030-01-01T00:00:00Z"))
        .await
        .unwrap();
    assert!(
        load_tombstones(&db).await.unwrap().is_empty(),
        "a tombstone is permanent, so a delete with no readable deadline records none"
    );
    let rows = synced_asset::Entity::find().all(&db).await.unwrap();
    assert!(
        rows.iter()
            .all(|row| row.tombstoned && row.retention_until.is_none())
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_whole_second_deadline_is_kept_as_it_is() {
    assert_eq!(ceil_second(ts("2026-01-01T00:00:00Z")), 1_767_225_600);
    assert_eq!(
        ceil_second(ts("2026-01-01T00:00:00.000000001Z")),
        1_767_225_601
    );
}
