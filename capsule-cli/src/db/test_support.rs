//! Shared helpers for the tests that read the CLI database's rows directly: the sync store's,
//! peering state's and peer sync's.

use std::path::Path;

use super::{DbError, LocalDb};
use crate::syncstore::{SyncedAssetRow, query_synced_assets};

/// A fresh, migrated handle on the database file at `path`: what a restarted process opens.
pub(crate) async fn open(path: &Path) -> LocalDb {
    LocalDb::open_async(path.to_path_buf())
        .await
        .expect("opens and migrates the CLI database")
}

/// Run raw SQL, as a test does to corrupt or probe the store.
pub(crate) async fn exec(db: &LocalDb, sql: &str) -> Result<(), DbError> {
    let sql = sql.to_owned();
    db.call(move |conn| conn.execute_batch(&sql)).await
}

/// Every `synced_assets` row, as stored.
pub(crate) async fn synced_rows(db: &LocalDb) -> Vec<SyncedAssetRow> {
    db.call(|conn| query_synced_assets(conn, "SELECT * FROM synced_assets", []))
        .await
        .expect("reads synced_assets")
}

/// A `peering_tombstones` row, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TombstoneRow {
    pub(crate) asset_id: String,
    pub(crate) recorded_at: String,
}

/// Every `peering_tombstones` row.
pub(crate) async fn tombstone_rows(db: &LocalDb) -> Vec<TombstoneRow> {
    db.call(|conn| {
        let mut stmt = conn.prepare("SELECT asset_id, recorded_at FROM peering_tombstones")?;
        stmt.query_map([], |row| {
            Ok(TombstoneRow {
                asset_id: row.get(0)?,
                recorded_at: row.get(1)?,
            })
        })?
        .collect()
    })
    .await
    .expect("reads peering_tombstones")
}

/// The `peering_tombstones` row for `asset_id`, if any.
pub(crate) async fn tombstone_row(db: &LocalDb, asset_id: &str) -> Option<TombstoneRow> {
    tombstone_rows(db)
        .await
        .into_iter()
        .find(|row| row.asset_id == asset_id)
}
