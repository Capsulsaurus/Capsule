use capsule_sdk::sync::{BlobManifest, FeedEntry};
use migration::{Migrator, MigratorTrait};
use sea_orm::{Database, DatabaseConnection};

use super::*;

async fn temp_db() -> (DatabaseConnection, std::path::PathBuf) {
    let path = std::env::temp_dir().join(format!(
        "capsule-cli-syncstore-{}.sqlite",
        nanoid::nanoid!()
    ));
    let url = format!("sqlite://{}?mode=rwc", path.display());
    let db = Database::connect(&url).await.expect("connect sqlite");
    Migrator::up(&db, None).await.expect("migrate");
    (db, path)
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
    let written = persist_page(&db, &page1).await.unwrap();
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
    persist_page(&db, &held).await.unwrap();

    reset_for_resync(&db).await.unwrap();

    let mut reloaded = load_sync_state(&db, "2026-12-31").await.unwrap();
    assert!(reloaded.cursor().is_start());
    assert_eq!(reloaded.high_water(&album), Some(0));
    assert_eq!(list_assets(&db, true).await.unwrap().len(), 2);

    // The rebuilt numbering, lower than what was held, applies.
    let rebuilt = page(vec![entry(&album, b"e1", 3, ChangeKind::Created)], 0x32);
    reloaded.apply_page(&rebuilt).unwrap();
    persist_page(&db, &rebuilt).await.unwrap();
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
    persist_page(&db, &p1).await.unwrap();
    let p2 = page(vec![entry(&album, b"x", 2, ChangeKind::Updated)], 0x22);
    persist_page(&db, &p2).await.unwrap();

    // One row (upserted), now at seq 2.
    let all = list_assets(&db, true).await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].sync_seq, 2);
    assert_eq!(all[0].kind, "metadata_updated");

    // Delete tombstones it.
    let p3 = page(vec![entry(&album, b"x", 3, ChangeKind::Deleted)], 0x23);
    persist_page(&db, &p3).await.unwrap();

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
