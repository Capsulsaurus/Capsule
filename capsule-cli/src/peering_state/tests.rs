//! The durable peering store: the floor only rises and survives a restart, an unreadable store
//! is refused rather than read as empty, a lost floor is detected, a delete stays restorable, and
//! a purge's tombstone outlives the process.

use std::path::PathBuf;

use capsule_sdk::sync::{BlobManifest, ChangeKind, FeedEntry, SyncCursor, SyncPage};
use migration::{Migrator, MigratorTrait};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection};

use super::*;
use crate::syncstore::persist_page;

/// A migrated SQLite file, removed when dropped.
struct TempDb(PathBuf);

impl TempDb {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("capsule-cli-peering-{}.sqlite", Uuid::new_v4())))
    }

    /// A fresh connection to the file: what a restarted process opens.
    async fn connect(&self) -> DatabaseConnection {
        let url = format!("sqlite://{}?mode=rwc", self.0.display());
        let db = Database::connect(&url).await.expect("connect sqlite");
        Migrator::up(&db, None).await.expect("migrate");
        db
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

const DEVICE: Uuid = Uuid::from_u128(0xD1);

async fn exec(db: &DatabaseConnection, sql: &str) -> Result<(), DbErr> {
    db.execute_unprepared(sql).await.map(|_| ())
}

#[tokio::test]
async fn a_fresh_device_starts_at_zero_and_the_floor_only_rises() {
    let file = TempDb::new();
    let db = file.connect().await;
    assert_eq!(load_floor(&db, DEVICE).await.expect("reads"), 0);
    assert_eq!(raise_floor(&db, DEVICE, 7).await.expect("raises"), 7);
    assert_eq!(
        raise_floor(&db, DEVICE, 3)
            .await
            .expect("a lower version is not an error"),
        7
    );
    assert_eq!(raise_floor(&db, DEVICE, 9).await.expect("raises"), 9);
    assert_eq!(
        load_floor(&db, Uuid::from_u128(0xD2)).await.expect("reads"),
        0,
        "each device holds its own floor"
    );
}

#[tokio::test]
async fn a_floor_recorded_before_a_restart_still_refuses_a_lower_directory_after_it() {
    let file = TempDb::new();
    {
        let versions = DbDirectoryVersions::open(file.connect().await, DEVICE)
            .await
            .expect("opens");
        versions.admit(5).await.expect("admits");
    }
    let versions = DbDirectoryVersions::open(file.connect().await, DEVICE)
        .await
        .expect("reopens");
    assert_eq!(versions.highest().expect("reads"), 5);
    assert!(matches!(
        versions.admit(4).await,
        Err(PeeringError::DirectoryRollback {
            pinned: 4,
            highest: 5
        })
    ));
    versions
        .admit(5)
        .await
        .expect("the same version is admitted");
    versions
        .admit(6)
        .await
        .expect("a newer version is admitted");
    assert_eq!(
        load_floor(&file.connect().await, DEVICE)
            .await
            .expect("reads"),
        6
    );
}

#[tokio::test]
async fn admit_rereads_the_store_so_another_process_raise_counts() {
    let file = TempDb::new();
    let versions = DbDirectoryVersions::open(file.connect().await, DEVICE)
        .await
        .expect("opens");
    raise_floor(&file.connect().await, DEVICE, 8)
        .await
        .expect("another process raises");
    assert!(matches!(
        versions.admit(7).await,
        Err(PeeringError::DirectoryRollback { highest: 8, .. })
    ));
    assert_eq!(versions.highest().expect("reads"), 8);
}

#[tokio::test]
async fn a_raise_between_admits_load_and_its_raise_still_refuses_a_stale_directory() {
    let file = TempDb::new();
    let versions = DbDirectoryVersions::open(file.connect().await, DEVICE)
        .await
        .expect("opens");
    raise_floor(&file.connect().await, DEVICE, 5)
        .await
        .expect("raises");
    // `admit` loads the floor...
    let loaded = load_floor(&file.connect().await, DEVICE)
        .await
        .expect("reads");
    assert_eq!(loaded, 5);
    // ...another process raises it before `admit` raises...
    raise_floor(&file.connect().await, DEVICE, 9)
        .await
        .expect("another process raises");
    // ...and a directory that cleared the loaded floor is refused against the stored one.
    assert!(matches!(
        versions.admit_over(7, loaded).await,
        Err(PeeringError::DirectoryRollback {
            pinned: 7,
            highest: 9
        })
    ));
    assert_eq!(versions.highest().expect("reads"), 9);
    assert_eq!(
        load_floor(&file.connect().await, DEVICE)
            .await
            .expect("reads"),
        9
    );
    versions
        .admit_over(9, loaded)
        .await
        .expect("the stored floor itself is admitted");
}

/// A scratch peer state directory beside the database file, removed when dropped.
struct StateDir(PathBuf);

impl StateDir {
    fn beside(file: &TempDb) -> Self {
        Self(file.0.with_extension("peer"))
    }
}

impl Drop for StateDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn peer_state_that_outlives_its_floor_is_refused_not_started_at_zero() {
    let file = TempDb::new();
    let state = StateDir::beside(&file);
    let versions = DbDirectoryVersions::open(file.connect().await, DEVICE)
        .await
        .expect("opens");
    versions
        .refuse_lost_floor(&state.0)
        .await
        .expect("a device with no peer state starts at zero");

    // The peer state a run left, with the database it wrote its floor to gone.
    std::fs::create_dir_all(state.0.join("inbox")).expect("creates");
    std::fs::write(state.0.join("origin.json"), "{}").expect("writes");
    assert!(matches!(
        versions.refuse_lost_floor(&state.0).await,
        Err(PeeringStateError::FloorLost { .. })
    ));

    versions.admit(0).await.expect("admits");
    versions
        .refuse_lost_floor(&state.0)
        .await
        .expect("a stored floor, even zero, is continuity");
}

#[tokio::test]
async fn a_legacy_floor_file_in_the_peer_state_is_continuity_once_adopted() {
    let file = TempDb::new();
    let state = StateDir::beside(&file);
    std::fs::create_dir_all(&state.0).expect("creates");
    let legacy = state.0.join("directory_version");
    std::fs::write(&legacy, "3").expect("writes");
    let versions = DbDirectoryVersions::open(file.connect().await, DEVICE)
        .await
        .expect("opens");
    versions.adopt_legacy_file(&legacy).await.expect("adopts");
    versions
        .refuse_lost_floor(&state.0)
        .await
        .expect("the adopted floor is the stored one");
    assert_eq!(versions.highest().expect("reads"), 3);
}

#[tokio::test]
async fn record_never_lowers_and_refuses_a_raise_it_has_not_persisted() {
    let file = TempDb::new();
    let versions = DbDirectoryVersions::open(file.connect().await, DEVICE)
        .await
        .expect("opens");
    versions.admit(4).await.expect("admits");
    versions.record(4).expect("the persisted floor is recorded");
    versions.record(2).expect("a lower version is not an error");
    assert_eq!(versions.highest().expect("reads"), 4);
    assert!(matches!(versions.record(5), Err(PeeringError::Io(_))));
    assert_eq!(versions.highest().expect("reads"), 4);
    assert_eq!(
        load_floor(&file.connect().await, DEVICE)
            .await
            .expect("reads"),
        4
    );
}

#[tokio::test]
async fn the_store_refuses_to_lower_or_delete_the_floor() {
    let file = TempDb::new();
    let db = file.connect().await;
    raise_floor(&db, DEVICE, 7).await.expect("raises");
    let device = DEVICE.hyphenated();
    assert!(
        exec(
            &db,
            &format!("UPDATE peering_floor SET directory_version = 1 WHERE device_id = '{device}'")
        )
        .await
        .is_err()
    );
    assert!(exec(&db, "DELETE FROM peering_floor").await.is_err());
    assert!(
        exec(
            &db,
            "INSERT INTO peering_floor VALUES ('00000000-0000-0000-0000-0000000000d3', -1)"
        )
        .await
        .is_err(),
        "a negative floor is refused"
    );
    // A replace deletes the row without firing a delete trigger; the lower insert is skipped.
    exec(
        &db,
        &format!("INSERT OR REPLACE INTO peering_floor VALUES ('{device}', 2)"),
    )
    .await
    .expect("a lower replace is skipped, not applied");
    assert_eq!(load_floor(&db, DEVICE).await.expect("reads"), 7);
    exec(
        &db,
        &format!("INSERT OR REPLACE INTO peering_floor VALUES ('{device}', 9)"),
    )
    .await
    .expect("a higher replace raises");
    assert_eq!(load_floor(&db, DEVICE).await.expect("reads"), 9);
}

#[tokio::test]
async fn an_out_of_range_version_is_refused_not_wrapped() {
    let file = TempDb::new();
    let db = file.connect().await;
    assert!(matches!(
        raise_floor(&db, DEVICE, u64::MAX).await,
        Err(PeeringStateError::OutOfRange(u64::MAX))
    ));
    assert_eq!(load_floor(&db, DEVICE).await.expect("reads"), 0);
}

#[tokio::test]
async fn an_unreadable_store_is_refused_rather_than_read_as_zero() {
    let file = TempDb::new();
    let db = file.connect().await;
    raise_floor(&db, DEVICE, 3).await.expect("raises");
    exec(&db, "DROP TABLE peering_floor").await.expect("drops");
    assert!(matches!(
        DbDirectoryVersions::open(db, DEVICE).await,
        Err(PeeringStateError::Db(_))
    ));
}

#[tokio::test]
async fn a_corrupt_stored_floor_is_refused() {
    let file = TempDb::new();
    let db = file.connect().await;
    // A row the CHECK constraint cannot catch once the table is rebuilt without it.
    exec(
        &db,
        "DROP TABLE peering_floor; \
         CREATE TABLE peering_floor (device_id TEXT PRIMARY KEY, directory_version BIGINT)",
    )
    .await
    .expect("rebuilds");
    exec(
        &db,
        &format!(
            "INSERT INTO peering_floor VALUES ('{}', -5)",
            DEVICE.hyphenated()
        ),
    )
    .await
    .expect("writes");
    assert!(matches!(
        load_floor(&db, DEVICE).await,
        Err(PeeringStateError::Corrupt(_))
    ));
}

#[tokio::test]
async fn a_legacy_floor_file_is_adopted_once_and_a_corrupt_one_refused() {
    let file = TempDb::new();
    let legacy = file.0.with_extension("directory_version");
    std::fs::write(&legacy, "11\n").expect("writes");
    let versions = DbDirectoryVersions::open(file.connect().await, DEVICE)
        .await
        .expect("opens");
    versions.adopt_legacy_file(&legacy).await.expect("adopts");
    assert_eq!(versions.highest().expect("reads"), 11);
    assert!(!legacy.exists(), "the adopted file is removed");
    versions
        .adopt_legacy_file(&legacy)
        .await
        .expect("no file is nothing to adopt");

    // A legacy floor below the stored one leaves it where it is.
    std::fs::write(&legacy, "2").expect("writes");
    versions.adopt_legacy_file(&legacy).await.expect("adopts");
    assert_eq!(
        load_floor(&file.connect().await, DEVICE)
            .await
            .expect("reads"),
        11
    );

    std::fs::write(&legacy, "not a number").expect("writes");
    assert!(matches!(
        versions.adopt_legacy_file(&legacy).await,
        Err(PeeringStateError::Corrupt(_))
    ));
    assert!(legacy.exists(), "a corrupt file is kept for recovery");
    let _ = std::fs::remove_file(&legacy);
}

fn entry(kind: ChangeKind, asset: &[u8], seq: u64) -> FeedEntry {
    FeedEntry {
        album_id: b"album".to_vec(),
        sync_seq: seq,
        protocol_version: "2026-05-31".to_string(),
        kind,
        asset_id: asset.to_vec(),
        manifest_cbor: vec![0xa0],
        metadata_blob: Vec::new(),
        blobs: BlobManifest::default(),
        derivative_manifests: Vec::new(),
        original_held: false,
        changed_at: "1970-01-01T00:00:00Z".to_string(),
    }
}

fn page(entries: Vec<FeedEntry>) -> SyncPage {
    SyncPage {
        entries,
        next_cursor: SyncCursor::from_bytes(vec![0x22; 41]),
        has_more: false,
    }
}

#[test]
fn a_feed_id_names_a_peering_asset_only_when_it_is_a_uuid() {
    let asset = Uuid::from_u128(0xA55E7);
    assert_eq!(feed_asset_uuid(asset.as_bytes()), Some(asset));
    assert_eq!(
        feed_asset_uuid(asset.hyphenated().to_string().as_bytes()),
        Some(asset)
    );
    assert_eq!(feed_asset_uuid(b"V1StGXR8_Z5jdHi6B-myT"), None);
}

#[tokio::test]
async fn a_deleted_then_restored_asset_is_never_a_tombstone() {
    let file = TempDb::new();
    let db = file.connect().await;
    let raw = Uuid::from_u128(5);
    let text = Uuid::from_u128(6);
    let text_id = text.hyphenated().to_string();
    persist_page(
        &db,
        &page(vec![
            entry(ChangeKind::Created, raw.as_bytes(), 1),
            entry(ChangeKind::Created, text_id.as_bytes(), 2),
        ]),
    )
    .await
    .expect("lands");
    persist_page(
        &db,
        &page(vec![
            entry(ChangeKind::Deleted, raw.as_bytes(), 3),
            entry(ChangeKind::Deleted, text_id.as_bytes(), 4),
        ]),
    )
    .await
    .expect("lands the deletes");
    assert!(
        load_tombstones(&db).await.expect("loads").is_empty(),
        "an asset in trash is still restorable, so it is not a tombstone"
    );
    // The trash-restore, which the feed carries as an update.
    persist_page(
        &db,
        &page(vec![
            entry(ChangeKind::Updated, raw.as_bytes(), 5),
            entry(ChangeKind::Updated, text_id.as_bytes(), 6),
        ]),
    )
    .await
    .expect("lands the restores");
    assert!(
        load_tombstones(&file.connect().await)
            .await
            .expect("loads")
            .is_empty()
    );
    assert!(
        peering_tombstone::Entity::find()
            .all(&db)
            .await
            .expect("reads")
            .is_empty(),
        "a delete records nothing permanent"
    );
}

#[tokio::test]
async fn a_purged_asset_stays_tombstoned_after_a_restart() {
    let file = TempDb::new();
    let purged = Uuid::from_u128(1);
    let by_text = Uuid::from_u128(2);
    let by_text_id = by_text.hyphenated().to_string();
    {
        let db = file.connect().await;
        persist_page(
            &db,
            &page(vec![
                entry(ChangeKind::Created, purged.as_bytes(), 1),
                entry(ChangeKind::Created, by_text_id.as_bytes(), 2),
                entry(ChangeKind::Created, b"not-a-uuid", 3),
            ]),
        )
        .await
        .expect("lands");
        persist_page(
            &db,
            &page(vec![
                entry(ChangeKind::Deleted, purged.as_bytes(), 4),
                entry(ChangeKind::Deleted, by_text_id.as_bytes(), 5),
                entry(ChangeKind::Deleted, b"not-a-uuid", 6),
            ]),
        )
        .await
        .expect("lands the deletes");
        record_purged(&db, &[purged, by_text], Timestamp::now())
            .await
            .expect("purges");
    }
    let db = file.connect().await;
    assert_eq!(
        load_tombstones(&db).await.expect("loads"),
        BTreeSet::from([purged, by_text])
    );
    assert!(
        exec(&db, "DELETE FROM peering_tombstones").await.is_err(),
        "the store refuses to forget a tombstone"
    );
    assert!(
        exec(&db, "UPDATE peering_tombstones SET asset_id = 'x'")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_purged_asset_the_sync_store_holds_live_is_not_returned() {
    let file = TempDb::new();
    let db = file.connect().await;
    let asset = Uuid::from_u128(3);
    record_purged(&db, &[asset], Timestamp::now())
        .await
        .expect("purges");
    persist_page(
        &db,
        &page(vec![entry(ChangeKind::Updated, asset.as_bytes(), 1)]),
    )
    .await
    .expect("lands");
    assert!(load_tombstones(&db).await.expect("loads").is_empty());
}

#[tokio::test]
async fn a_tombstone_keeps_its_first_record() {
    let file = TempDb::new();
    let db = file.connect().await;
    let asset = Uuid::from_u128(4);
    let first: Timestamp = "2026-01-01T00:00:00Z".parse().expect("a timestamp");
    record_purged(&db, &[asset], first).await.expect("records");
    record_purged(&db, &[asset], Timestamp::now())
        .await
        .expect("recording again is not an error");
    let row = peering_tombstone::Entity::find_by_id(asset.hyphenated().to_string())
        .one(&db)
        .await
        .expect("reads")
        .expect("a row");
    assert_eq!(row.recorded_at, first.to_string());
}

#[tokio::test]
async fn a_corrupt_tombstone_refuses_the_whole_set() {
    let file = TempDb::new();
    let db = file.connect().await;
    exec(
        &db,
        "INSERT INTO peering_tombstones VALUES ('not-a-uuid', '2026-01-01T00:00:00Z')",
    )
    .await
    .expect("writes");
    assert!(matches!(
        load_tombstones(&db).await,
        Err(PeeringStateError::Corrupt(_))
    ));
}

#[tokio::test]
async fn an_unreadable_synced_id_refuses_the_whole_set() {
    let file = TempDb::new();
    let db = file.connect().await;
    exec(
        &db,
        "INSERT INTO synced_assets (asset_id, album_id, sync_seq, kind, original_held, tombstoned) \
         VALUES ('!!not base64!!', 'YQ==', 1, 'created', 0, 0)",
    )
    .await
    .expect("writes");
    assert!(matches!(
        load_tombstones(&db).await,
        Err(PeeringStateError::Corrupt(_))
    ));
}
