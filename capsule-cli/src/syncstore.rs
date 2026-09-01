//! The sync-fed local store (slice `S-D5`): the durable half of `capsule sync`.
//!
//! `capsule-sdk`'s [`SyncState`] is the *pure* anti-rewind / forward-version state
//! machine (slice `S-D2`); it is network-free and, by itself, in-memory. This
//! module is the CLI's durable backing for it: it rehydrates a `SyncState` from the
//! CLI's SQLite (the opaque cursor plus the per-album high-water marks, the latter
//! *derived* from `MAX(sync_seq)` so every applied entry re-establishes the mark),
//! and it lands each validated [`SyncPage`] into the `synced_assets` table that
//! `capsule list` queries client-side.
//!
//! Persistence is validate-then-apply, mirroring the SDK: the caller applies a page
//! to the in-memory `SyncState` first (which enforces anti-rewind), and only a page
//! that passed is handed here to persist. Each page is written in one transaction —
//! all its assets plus the advanced cursor — so a crash never leaves the cursor
//! ahead of the assets it names.

use std::collections::HashMap;

use base64::Engine as _;
use capsule_sdk::sync::{ChangeKind, SyncCursor, SyncPage, SyncState};
use entity::{sync_cursor, synced_asset};
use sea_orm::ActiveValue::Set;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder, TransactionTrait,
};
use thiserror::Error;

/// The singleton row id for the one opaque feed cursor.
const CURSOR_ROW_ID: i32 = 0;

/// Everything the sync store can fail with.
#[derive(Debug, Error)]
pub enum SyncStoreError {
    /// A database operation failed.
    #[error("sync store database error: {0}")]
    Db(#[from] DbErr),
    /// A stored id was not valid base64 (store corruption).
    #[error("corrupt stored id {value:?}: {source}")]
    CorruptId {
        /// The offending stored value.
        value: String,
        /// The decode error.
        source: base64::DecodeError,
    },
}

/// Base64 the feed's opaque id bytes for lossless text storage.
fn encode_id(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Recover the feed's opaque id bytes from their stored base64 form.
fn decode_id(value: &str) -> Result<Vec<u8>, SyncStoreError> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|source| SyncStoreError::CorruptId {
            value: value.to_string(),
            source,
        })
}

/// The stable string form of a change kind, as stored and displayed.
fn kind_label(kind: ChangeKind) -> &'static str {
    match kind {
        ChangeKind::Created => "created",
        ChangeKind::Updated => "metadata_updated",
        ChangeKind::Deleted => "deleted",
    }
}

/// Rehydrate a [`SyncState`] from the durable store, pinned to the running build's
/// `max_known_protocol`. A fresh store yields the first-sync sentinel.
pub async fn load_sync_state<C: ConnectionTrait>(
    db: &C,
    max_known_protocol: &str,
) -> Result<SyncState, SyncStoreError> {
    let cursor = sync_cursor::Entity::find_by_id(CURSOR_ROW_ID)
        .one(db)
        .await?
        .map_or_else(SyncCursor::start, |row| SyncCursor::from_bytes(row.cursor));

    // The per-album high-water mark is MAX(sync_seq) — every applied entry was
    // recorded, so this reconstructs the anti-rewind floor exactly.
    let mut high_water: HashMap<Vec<u8>, u64> = HashMap::new();
    for row in synced_asset::Entity::find().all(db).await? {
        let album = decode_id(&row.album_id)?;
        let seq = row.sync_seq as u64;
        let slot = high_water.entry(album).or_insert(0);
        *slot = (*slot).max(seq);
    }

    Ok(SyncState::restore(max_known_protocol, cursor, high_water))
}

/// Land a validated page: upsert every asset and advance the stored cursor, all in
/// one transaction. Returns the number of entries written.
pub async fn persist_page<C: ConnectionTrait + TransactionTrait>(
    db: &C,
    page: &SyncPage,
) -> Result<usize, SyncStoreError> {
    let txn = db.begin().await?;

    for entry in &page.entries {
        let model = synced_asset::ActiveModel {
            asset_id: Set(encode_id(&entry.asset_id)),
            album_id: Set(encode_id(&entry.album_id)),
            sync_seq: Set(entry.sync_seq as i64),
            kind: Set(kind_label(entry.kind).to_string()),
            original_held: Set(entry.original_held),
            tombstoned: Set(entry.kind == ChangeKind::Deleted),
        };
        synced_asset::Entity::insert(model)
            .on_conflict(
                OnConflict::column(synced_asset::Column::AssetId)
                    .update_columns([
                        synced_asset::Column::AlbumId,
                        synced_asset::Column::SyncSeq,
                        synced_asset::Column::Kind,
                        synced_asset::Column::OriginalHeld,
                        synced_asset::Column::Tombstoned,
                    ])
                    .to_owned(),
            )
            .exec(&txn)
            .await?;
    }

    sync_cursor::Entity::insert(sync_cursor::ActiveModel {
        id: Set(CURSOR_ROW_ID),
        cursor: Set(page.next_cursor.as_bytes().to_vec()),
    })
    .on_conflict(
        OnConflict::column(sync_cursor::Column::Id)
            .update_column(sync_cursor::Column::Cursor)
            .to_owned(),
    )
    .exec(&txn)
    .await?;

    txn.commit().await?;
    Ok(page.entries.len())
}

/// A row of the sync-fed local store, decoded back to feed-native shapes.
#[derive(Debug, Clone)]
pub struct SyncedAssetView {
    /// The asset id (opaque feed bytes).
    pub asset_id: Vec<u8>,
    /// The owning album id (opaque feed bytes).
    pub album_id: Vec<u8>,
    /// The per-album feed sequence.
    pub sync_seq: u64,
    /// The stored change-kind label.
    pub kind: String,
    /// Whether the original blob is finalized server-side.
    pub original_held: bool,
    /// Whether the asset is tombstoned.
    pub tombstoned: bool,
}

/// Query the sync-fed local store, ordered by album then sequence. Tombstoned
/// assets are included only when `include_tombstoned` is set.
pub async fn list_assets<C: ConnectionTrait>(
    db: &C,
    include_tombstoned: bool,
) -> Result<Vec<SyncedAssetView>, SyncStoreError> {
    let mut query = synced_asset::Entity::find();
    if !include_tombstoned {
        query = query.filter(synced_asset::Column::Tombstoned.eq(false));
    }
    let rows = query
        .order_by_asc(synced_asset::Column::AlbumId)
        .order_by_asc(synced_asset::Column::SyncSeq)
        .all(db)
        .await?;

    rows.into_iter()
        .map(|row| {
            Ok(SyncedAssetView {
                asset_id: decode_id(&row.asset_id)?,
                album_id: decode_id(&row.album_id)?,
                sync_seq: row.sync_seq as u64,
                kind: row.kind,
                original_held: row.original_held,
                tombstoned: row.tombstoned,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
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
}
