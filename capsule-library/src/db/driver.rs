use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::Path;

use capsule_core::domain::model_identity::TaskKind;
use rusqlite::{Connection, params};

use crate::db::migrate::{self, MigrationError};
use crate::db::rows::{AlbumRow, AssetRow, AssetStackRow, CachedRepresentationRow, StackMemberRow};

pub struct DatabaseDriver {
    pub(in crate::db) conn: Connection,
    /// Tasks whose `vec0` partition table this driver has already created. The DDL is idempotent,
    /// so this is purely a hot-path memo — see [`crate::db::VectorTableSpec`] for why the
    /// tables are created by their writer rather than at open time.
    pub(in crate::db) vector_tables: RefCell<BTreeSet<TaskKind>>,
}

impl DatabaseDriver {
    /// Open (or create) the catalog at `path` and bring it to the crate's `SCHEMA_VERSION`.
    ///
    /// The migrator's typed [`MigrationError`] is flattened into `rusqlite::Error` here
    /// because this signature is consumed by `capsule-core-ffi`; the crate's own library
    /// opener goes through the crate-private `open_typed` so a catalog newer than this build
    /// surfaces as a typed refusal rather than a message inside a `SqliteFailure`.
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        Self::open_typed(path).map_err(rusqlite::Error::from)
    }

    /// As [`open`](Self::open), keeping the migrator's typed error.
    ///
    /// [`MigrationError::CatalogTooNew`] is the one variant a caller acts on differently: the
    /// catalog was left untouched and the recovery is to update the app, so `library::open`
    /// maps it to [`LibraryError::CatalogTooNew`](crate::library::LibraryError::CatalogTooNew)
    /// instead of folding it into a generic database error.
    pub(crate) fn open_typed(path: &Path) -> Result<Self, MigrationError> {
        crate::db::vector::ensure_vec_extension();
        let conn = Connection::open(path)?;
        let driver = Self::new(conn);
        driver.migrate()?;
        Ok(driver)
    }

    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        crate::db::vector::ensure_vec_extension();
        let conn = Connection::open_in_memory()?;
        let driver = Self::new(conn);
        driver.init_schema()?;
        Ok(driver)
    }

    fn new(conn: Connection) -> Self {
        Self {
            conn,
            vector_tables: RefCell::new(BTreeSet::new()),
        }
    }

    /// Bring the catalog to the crate's `SCHEMA_VERSION`, creating it if the database is empty
    /// and otherwise migrating it forward (see the crate-private `db::migrate`).
    ///
    /// The migrator's typed [`MigrationError`] is flattened into `rusqlite::Error` here because
    /// this signature is consumed by `capsule-core-ffi` and `library::open`; callers that want
    /// the typed error (and the list of steps applied) call [`Self::migrate`] instead.
    pub fn init_schema(&self) -> Result<(), rusqlite::Error> {
        // The per-task `vec0` tables are deliberately NOT created here: their column width and
        // metric come from the model that writes them, which this layer knows nothing about.
        // Each is created on first insert/query from the spec its writer declares, or eagerly via
        // [`DatabaseDriver::create_vector_tables`].
        self.migrate()?;
        Ok(())
    }

    /// Bring the catalog to the crate's `SCHEMA_VERSION`, reporting exactly what ran.
    ///
    /// Refuses (without writing anything) a catalog stamped newer than this build supports —
    /// see [`MigrationError::CatalogTooNew`].
    pub fn migrate(&self) -> Result<migrate::Outcome, MigrationError> {
        migrate::migrate(&self.conn)
    }

    /// The underlying connection, for schema-level assertions inside `crate::db`'s tests.
    #[cfg(test)]
    pub(in crate::db) fn connection(&self) -> &Connection {
        &self.conn
    }

    pub fn schema_version(&self) -> Result<u32, rusqlite::Error> {
        let version: u32 = self
            .conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
        Ok(version)
    }

    pub fn insert_asset(&self, row: &AssetRow) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO assets (uuid, asset_type, capture_timestamp, capture_utc, capture_tz_source,
             import_timestamp, hash_sha256, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at, is_hidden,
             caption, latitude, longitude)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            params![
                row.uuid, row.asset_type, row.capture_timestamp, row.capture_utc,
                row.capture_tz_source, row.import_timestamp, row.hash_sha256,
                row.width, row.height, row.duration_ms, row.stack_id,
                row.is_stack_hidden as i64, row.chromahash, row.dominant_color,
                row.album_id, row.rating, row.is_deleted as i64, row.deleted_at,
                row.is_hidden as i64, stored_caption(row.caption.as_deref()),
                row.latitude, row.longitude,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_asset(&self, row: &AssetRow) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT OR REPLACE INTO assets (uuid, asset_type, capture_timestamp, capture_utc, capture_tz_source,
             import_timestamp, hash_sha256, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at, is_hidden,
             caption, latitude, longitude)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)",
            params![
                row.uuid, row.asset_type, row.capture_timestamp, row.capture_utc,
                row.capture_tz_source, row.import_timestamp, row.hash_sha256,
                row.width, row.height, row.duration_ms, row.stack_id,
                row.is_stack_hidden as i64, row.chromahash, row.dominant_color,
                row.album_id, row.rating, row.is_deleted as i64, row.deleted_at,
                row.is_hidden as i64, stored_caption(row.caption.as_deref()),
                row.latitude, row.longitude,
            ],
        )?;
        Ok(())
    }

    pub fn find_by_uuid(&self, uuid: &str) -> Result<Option<AssetRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, asset_type, capture_timestamp, capture_utc, capture_tz_source,
             import_timestamp, hash_sha256, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at, is_hidden,
             caption, latitude, longitude
             FROM assets WHERE uuid = ?1 LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![uuid], map_asset_row)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn find_by_hash(&self, hash: &str) -> Result<Option<AssetRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, asset_type, capture_timestamp, capture_utc, capture_tz_source,
             import_timestamp, hash_sha256, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at, is_hidden,
             caption, latitude, longitude
             FROM assets WHERE hash_sha256 = ?1 LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![hash], map_asset_row)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn query_timeline(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<AssetRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, asset_type, capture_timestamp, capture_utc, capture_tz_source,
             import_timestamp, hash_sha256, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at, is_hidden,
             caption, latitude, longitude
             FROM assets
             WHERE is_deleted = 0 AND is_stack_hidden = 0 AND is_hidden = 0
             ORDER BY COALESCE(capture_utc, capture_timestamp) DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], map_asset_row)?;
        rows.collect()
    }

    /// Query the timeline filtered by asset type and/or capture-time window.
    ///
    /// `after`/`before` bound `COALESCE(capture_utc, capture_timestamp)`
    /// (inclusive). Any filter argument left as `None` is not applied.
    pub fn query_timeline_filtered(
        &self,
        asset_type: Option<&str>,
        after: Option<i64>,
        before: Option<i64>,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<AssetRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, asset_type, capture_timestamp, capture_utc, capture_tz_source,
             import_timestamp, hash_sha256, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at, is_hidden,
             caption, latitude, longitude
             FROM assets
             WHERE is_deleted = 0 AND is_stack_hidden = 0 AND is_hidden = 0
               AND (?1 IS NULL OR asset_type = ?1)
               AND (?2 IS NULL OR COALESCE(capture_utc, capture_timestamp) >= ?2)
               AND (?3 IS NULL OR COALESCE(capture_utc, capture_timestamp) <= ?3)
             ORDER BY COALESCE(capture_utc, capture_timestamp) DESC
             LIMIT ?4 OFFSET ?5",
        )?;
        let rows = stmt.query_map(
            params![asset_type, after, before, limit as i64, offset as i64],
            map_asset_row,
        )?;
        rows.collect()
    }

    pub fn insert_stack(&self, row: &AssetStackRow) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO asset_stacks (id, stack_type, primary_asset_id, cover_asset_id,
             is_collapsed, is_auto_generated, created_at, modified_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                row.id,
                row.stack_type,
                row.primary_asset_id,
                row.cover_asset_id,
                row.is_collapsed as i64,
                row.is_auto_generated as i64,
                row.created_at,
                row.modified_at,
            ],
        )?;
        Ok(())
    }

    pub fn insert_stack_member(&self, row: &StackMemberRow) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO stack_members (id, stack_id, asset_id, sequence_order, member_role, created_at)
             VALUES (?1,?2,?3,?4,?5,?6)",
            params![row.id, row.stack_id, row.asset_id, row.sequence_order, row.member_role, row.created_at],
        )?;
        Ok(())
    }

    pub fn update_stack_hidden(&self, uuid: &str, hidden: bool) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE assets SET is_stack_hidden = ?1 WHERE uuid = ?2",
            params![hidden as i64, uuid],
        )?;
        Ok(())
    }

    pub fn update_stack_primary(
        &self,
        stack_id: &str,
        primary_uuid: &str,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE asset_stacks SET primary_asset_id = ?1, modified_at = ?2 WHERE id = ?3",
            params![primary_uuid, now_secs(), stack_id],
        )?;
        Ok(())
    }

    pub fn find_stack_by_detection(
        &self,
        key: &str,
        method: &str,
    ) -> Result<Option<AssetStackRow>, rusqlite::Error> {
        // Find a stack via a stack_member that has a matching detection key+method
        // Since detection key/method is stored in the sidecar, not in the DB,
        // we use a separate lookup table approach. For now, store detection key in
        // stack_members table isn't in the spec. Instead, we'll need to track this
        // in-memory during the import batch.
        //
        // The spec says: "Check if an asset_stacks row exists for this (detection_key, detection_method) pair
        // by looking up stack_members for the existing candidates in this batch."
        // This means the in-memory ImportCandidate tracks the key; DB lookup is by stack_id of existing members.
        // We expose this as a no-op for now - the executor tracks stack membership in-memory during the batch.
        let _ = (key, method);
        Ok(None)
    }

    pub fn list_stack_members(
        &self,
        stack_id: &str,
    ) -> Result<Vec<StackMemberRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, stack_id, asset_id, sequence_order, member_role, created_at
             FROM stack_members WHERE stack_id = ?1 ORDER BY sequence_order ASC",
        )?;
        let rows = stmt.query_map(params![stack_id], |row| {
            Ok(StackMemberRow {
                id: row.get(0)?,
                stack_id: row.get(1)?,
                asset_id: row.get(2)?,
                sequence_order: row.get(3)?,
                member_role: row.get(4)?,
                created_at: row.get(5)?,
            })
        })?;
        rows.collect()
    }

    pub fn soft_delete(&self, uuid: &str, deleted_at: i64) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE assets SET is_deleted = 1, deleted_at = ?1 WHERE uuid = ?2",
            params![deleted_at, uuid],
        )?;
        Ok(())
    }

    pub fn restore_asset(&self, uuid: &str) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE assets SET is_deleted = 0, deleted_at = NULL WHERE uuid = ?1",
            params![uuid],
        )?;
        Ok(())
    }

    pub fn query_expired_trash(
        &self,
        older_than_secs: i64,
    ) -> Result<Vec<AssetRow>, rusqlite::Error> {
        let threshold = now_secs() - older_than_secs;
        let mut stmt = self.conn.prepare(
            "SELECT uuid, asset_type, capture_timestamp, capture_utc, capture_tz_source,
             import_timestamp, hash_sha256, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at, is_hidden,
             caption, latitude, longitude
             FROM assets WHERE is_deleted = 1 AND deleted_at IS NOT NULL AND deleted_at < ?1",
        )?;
        let rows = stmt.query_map(params![threshold], map_asset_row)?;
        rows.collect()
    }

    /// All currently soft-deleted assets — the Recently Deleted listing,
    /// most-recently-deleted first.
    pub fn query_trash(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<AssetRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, asset_type, capture_timestamp, capture_utc, capture_tz_source,
             import_timestamp, hash_sha256, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at, is_hidden,
             caption, latitude, longitude
             FROM assets WHERE is_deleted = 1
             ORDER BY deleted_at DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], map_asset_row)?;
        rows.collect()
    }

    /// All user-hidden, non-deleted assets — the **Hidden** listing, newest first.
    ///
    /// Hidden assets are excluded from every default projection (timeline, album), so this
    /// is the only view that surfaces them; it is reached exclusively through
    /// [`GateKeeper::query_hidden`](crate::library::GateKeeper::query_hidden), which holds
    /// it behind the same fresh-local-auth gate as Recently Deleted (SSoT:
    /// design/organization § Hidden Assets, design/local-gallery SR1).
    ///
    /// Soft-deleted assets are excluded even when hidden: a trashed asset belongs to
    /// Recently Deleted, so the two gated views never show the same row.
    pub fn query_hidden(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<AssetRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, asset_type, capture_timestamp, capture_utc, capture_tz_source,
             import_timestamp, hash_sha256, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at, is_hidden,
             caption, latitude, longitude
             FROM assets WHERE is_hidden = 1 AND is_deleted = 0
             ORDER BY COALESCE(capture_utc, capture_timestamp) DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], map_asset_row)?;
        rows.collect()
    }

    /// Set the index projection of an asset's sidecar `hidden` register. Hiding is
    /// view-layer only — the asset keeps its album, its stack, and its sync state.
    pub fn update_asset_hidden(&self, uuid: &str, hidden: bool) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE assets SET is_hidden = ?1 WHERE uuid = ?2",
            params![hidden as i64, uuid],
        )?;
        Ok(())
    }

    /// Set (or, with `None` or an empty string, clear) the index projection of an asset's
    /// sidecar `caption` register. Returns whether a row was updated: `false` means no asset
    /// has `uuid`, and nothing was written.
    pub fn update_asset_caption(
        &self,
        uuid: &str,
        caption: Option<&str>,
    ) -> Result<bool, rusqlite::Error> {
        let changed = self.conn.execute(
            "UPDATE assets SET caption = ?1 WHERE uuid = ?2",
            params![stored_caption(caption), uuid],
        )?;
        Ok(changed > 0)
    }

    /// Permanently remove an asset's rows — its `assets` row, its indexed tags, its cached
    /// representations and its stack memberships — in one transaction, so no index row names
    /// it afterwards. A stack whose primary or cover it was passes that role to its first
    /// remaining member (by sequence, then id), and a stack it leaves empty is removed. An
    /// album cover naming it is cleared. The on-disk files are the caller's concern
    /// (`Workspace::purge` removes them first).
    pub fn purge_asset(&self, uuid: &str) -> Result<(), rusqlite::Error> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM asset_tags WHERE uuid = ?1", params![uuid])?;
        tx.execute(
            "DELETE FROM cached_representations WHERE uuid = ?1",
            params![uuid],
        )?;
        tx.execute(
            "DELETE FROM stack_members WHERE asset_id = ?1",
            params![uuid],
        )?;
        // The successor is the first remaining member; a stack with none keeps the purged id
        // only until the `DELETE` below removes it.
        let successor = "(SELECT m.asset_id FROM stack_members m WHERE m.stack_id = asset_stacks.id
                          ORDER BY m.sequence_order, m.asset_id LIMIT 1)";
        tx.execute(
            &format!(
                "UPDATE asset_stacks SET primary_asset_id = COALESCE({successor}, primary_asset_id)
                 WHERE primary_asset_id = ?1"
            ),
            params![uuid],
        )?;
        tx.execute(
            &format!(
                "UPDATE asset_stacks SET cover_asset_id = {successor} WHERE cover_asset_id = ?1"
            ),
            params![uuid],
        )?;
        tx.execute(
            "DELETE FROM asset_stacks
             WHERE primary_asset_id = ?1
               AND NOT EXISTS (SELECT 1 FROM stack_members m WHERE m.stack_id = asset_stacks.id)",
            params![uuid],
        )?;
        tx.execute(
            "UPDATE albums SET cover_asset_id = NULL WHERE cover_asset_id = ?1",
            params![uuid],
        )?;
        tx.execute("DELETE FROM assets WHERE uuid = ?1", params![uuid])?;
        tx.commit()
    }

    // ── Albums ───────────────────────────────────────────────────────────────

    pub fn insert_album(&self, row: &AlbumRow) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT INTO albums (id, name, created_at, modified_at, cover_asset_id)
             VALUES (?1,?2,?3,?4,?5)",
            params![
                row.id,
                row.name,
                row.created_at,
                row.modified_at,
                row.cover_asset_id,
            ],
        )?;
        Ok(())
    }

    pub fn update_album(&self, row: &AlbumRow) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE albums SET name = ?2, modified_at = ?3, cover_asset_id = ?4 WHERE id = ?1",
            params![row.id, row.name, row.modified_at, row.cover_asset_id],
        )?;
        Ok(())
    }

    pub fn delete_album(&self, id: &str) -> Result<(), rusqlite::Error> {
        // Detach member assets first — deleting an album never deletes assets.
        self.conn.execute(
            "UPDATE assets SET album_id = NULL WHERE album_id = ?1",
            params![id],
        )?;
        self.conn
            .execute("DELETE FROM albums WHERE id = ?1", params![id])?;
        Ok(())
    }

    pub fn find_album(&self, id: &str) -> Result<Option<AlbumRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, created_at, modified_at, cover_asset_id
             FROM albums WHERE id = ?1 LIMIT 1",
        )?;
        let mut rows = stmt.query_map(params![id], map_album_row)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn list_albums(&self) -> Result<Vec<AlbumRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, created_at, modified_at, cover_asset_id
             FROM albums ORDER BY created_at DESC",
        )?;
        let rows = stmt.query_map([], map_album_row)?;
        rows.collect()
    }

    /// Set (or clear, with `None`) the album an asset belongs to.
    pub fn set_asset_album(
        &self,
        uuid: &str,
        album_id: Option<&str>,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE assets SET album_id = ?1 WHERE uuid = ?2",
            params![album_id, uuid],
        )?;
        Ok(())
    }

    /// Query the non-deleted, non-hidden assets in an album, newest first.
    pub fn query_album_assets(
        &self,
        album_id: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<AssetRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, asset_type, capture_timestamp, capture_utc, capture_tz_source,
             import_timestamp, hash_sha256, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at, is_hidden,
             caption, latitude, longitude
             FROM assets
             WHERE is_deleted = 0 AND is_stack_hidden = 0 AND is_hidden = 0
               AND album_id = ?1
             ORDER BY COALESCE(capture_utc, capture_timestamp) DESC
             LIMIT ?2 OFFSET ?3",
        )?;
        let rows = stmt.query_map(
            params![album_id, limit as i64, offset as i64],
            map_asset_row,
        )?;
        rows.collect()
    }

    // ── Cached representations (adaptive cache eviction, issue #23) ──────────────

    /// Insert or replace a cached representation row.
    pub fn upsert_representation(
        &self,
        row: &CachedRepresentationRow,
    ) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT OR REPLACE INTO cached_representations
             (uuid, tier, format, bytes, path, last_accessed_at, pinned, is_owned_original)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                row.uuid,
                row.tier,
                row.format,
                row.bytes,
                row.path,
                row.last_accessed_at,
                row.pinned as i64,
                row.is_owned_original as i64,
            ],
        )?;
        Ok(())
    }

    /// Remove a cached representation row (after its file has been deleted).
    pub fn remove_representation(&self, uuid: &str, tier: &str) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "DELETE FROM cached_representations WHERE uuid = ?1 AND tier = ?2",
            params![uuid, tier],
        )?;
        Ok(())
    }

    /// Stamp a representation's last-access time (recency promotion) — viewing an asset keeps its
    /// representations from eviction. `now` is injected so the policy stays deterministic.
    pub fn record_access(&self, uuid: &str, tier: &str, now: i64) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "UPDATE cached_representations SET last_accessed_at = ?1 WHERE uuid = ?2 AND tier = ?3",
            params![now, uuid, tier],
        )?;
        Ok(())
    }

    /// The reclaimable representations to evict so the reclaimable set fits within `budget_bytes`.
    /// Pinned and device-owned originals are never candidates. Rows are ranked most-valuable-to-
    /// keep first (most-recently-accessed; thumbnail over preview over original at equal recency),
    /// then everything past the budget is evicted — i.e. least-recently-accessed first, original →
    /// preview → thumbnail at equal recency.
    pub fn eviction_candidates(
        &self,
        budget_bytes: i64,
    ) -> Result<Vec<CachedRepresentationRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, tier, format, bytes, path, last_accessed_at, pinned, is_owned_original
             FROM cached_representations
             WHERE pinned = 0 AND is_owned_original = 0
             ORDER BY last_accessed_at DESC,
                      CASE tier WHEN 'thumbnail' THEN 2 WHEN 'preview' THEN 1 WHEN 'original' THEN 0 ELSE -1 END DESC",
        )?;
        let keep_first = stmt.query_map([], map_cached_representation_row)?;

        let mut kept_bytes: i64 = 0;
        let mut evict = Vec::new();
        for row in keep_first {
            let row = row?;
            kept_bytes += row.bytes;
            if kept_bytes > budget_bytes {
                evict.push(row);
            }
        }
        Ok(evict)
    }

    /// All cached representations recorded for `uuid` (any tier).
    pub fn representations_for(
        &self,
        uuid: &str,
    ) -> Result<Vec<CachedRepresentationRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, tier, format, bytes, path, last_accessed_at, pinned, is_owned_original
             FROM cached_representations WHERE uuid = ?1 ORDER BY tier",
        )?;
        let rows = stmt.query_map(params![uuid], map_cached_representation_row)?;
        rows.collect()
    }

    // ── User tags (asset_tags index) ────────────────────────────────────────────

    /// Replace the indexed user tags for `uuid` with `tags` (the asset's current OR-set value).
    pub fn replace_asset_tags(&self, uuid: &str, tags: &[String]) -> Result<(), rusqlite::Error> {
        self.conn
            .execute("DELETE FROM asset_tags WHERE uuid = ?1", params![uuid])?;
        for tag in tags {
            self.conn.execute(
                "INSERT OR IGNORE INTO asset_tags (uuid, tag) VALUES (?1, ?2)",
                params![uuid, tag],
            )?;
        }
        Ok(())
    }

    /// The indexed user tags for `uuid`, sorted.
    pub fn tags_for(&self, uuid: &str) -> Result<Vec<String>, rusqlite::Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT tag FROM asset_tags WHERE uuid = ?1 ORDER BY tag")?;
        let rows = stmt.query_map(params![uuid], |r| r.get::<_, String>(0))?;
        rows.collect()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before the UNIX epoch")
        .as_secs() as i64
}

fn map_asset_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AssetRow> {
    Ok(AssetRow {
        uuid: row.get(0)?,
        asset_type: row.get(1)?,
        capture_timestamp: row.get(2)?,
        capture_utc: row.get(3)?,
        capture_tz_source: row.get(4)?,
        import_timestamp: row.get(5)?,
        hash_sha256: row.get(6)?,
        width: row.get(7)?,
        height: row.get(8)?,
        duration_ms: row.get(9)?,
        stack_id: row.get(10)?,
        is_stack_hidden: row.get::<_, i64>(11)? != 0,
        chromahash: row.get(12)?,
        dominant_color: row.get(13)?,
        album_id: row.get(14)?,
        rating: row.get(15)?,
        is_deleted: row.get::<_, i64>(16)? != 0,
        deleted_at: row.get(17)?,
        is_hidden: row.get::<_, i64>(18)? != 0,
        caption: row.get(19)?,
        latitude: row.get(20)?,
        longitude: row.get(21)?,
    })
}

/// The stored form of a caption: an empty caption is no caption, so it is written as `NULL`
/// rather than as a second spelling of "none" a reader would have to know about.
fn stored_caption(caption: Option<&str>) -> Option<&str> {
    caption.filter(|c| !c.is_empty())
}

fn map_album_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AlbumRow> {
    Ok(AlbumRow {
        id: row.get(0)?,
        name: row.get(1)?,
        created_at: row.get(2)?,
        modified_at: row.get(3)?,
        cover_asset_id: row.get(4)?,
    })
}

fn map_cached_representation_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<CachedRepresentationRow> {
    Ok(CachedRepresentationRow {
        uuid: row.get(0)?,
        tier: row.get(1)?,
        format: row.get(2)?,
        bytes: row.get(3)?,
        path: row.get(4)?,
        last_accessed_at: row.get(5)?,
        pinned: row.get::<_, i64>(6)? != 0,
        is_owned_original: row.get::<_, i64>(7)? != 0,
    })
}

#[cfg(test)]
mod tests;
