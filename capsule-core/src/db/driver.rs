use crate::db::rows::{AiTagRow, AssetRow, AssetStackRow, CachedRepresentationRow, StackMemberRow};
use crate::db::schema;
use rusqlite::{Connection, params};
use std::path::Path;

pub struct DatabaseDriver {
    pub(in crate::db) conn: Connection,
}

impl DatabaseDriver {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        crate::db::vector::ensure_vec_extension();
        let conn = Connection::open(path)?;
        let driver = Self { conn };
        driver.init_schema()?;
        Ok(driver)
    }

    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        crate::db::vector::ensure_vec_extension();
        let conn = Connection::open_in_memory()?;
        let driver = Self { conn };
        driver.init_schema()?;
        Ok(driver)
    }

    pub fn init_schema(&self) -> Result<(), rusqlite::Error> {
        self.conn.execute_batch(schema::DDL)?;
        // The per-task vector tables are sized from the canonical model registry (their `vec0`
        // dimension is registry-declared), so they are created here rather than in the static DDL.
        crate::db::vector::create_vector_tables(&self.conn, &crate::ml::Registry::canonical())?;
        self.conn.execute_batch(&format!(
            "PRAGMA user_version = {};",
            schema::SCHEMA_VERSION
        ))?;
        Ok(())
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
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                row.uuid, row.asset_type, row.capture_timestamp, row.capture_utc,
                row.capture_tz_source, row.import_timestamp, row.hash_sha256,
                row.width, row.height, row.duration_ms, row.stack_id,
                row.is_stack_hidden as i64, row.chromahash, row.dominant_color,
                row.album_id, row.rating, row.is_deleted as i64, row.deleted_at,
            ],
        )?;
        Ok(())
    }

    pub fn upsert_asset(&self, row: &AssetRow) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT OR REPLACE INTO assets (uuid, asset_type, capture_timestamp, capture_utc, capture_tz_source,
             import_timestamp, hash_sha256, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                row.uuid, row.asset_type, row.capture_timestamp, row.capture_utc,
                row.capture_tz_source, row.import_timestamp, row.hash_sha256,
                row.width, row.height, row.duration_ms, row.stack_id,
                row.is_stack_hidden as i64, row.chromahash, row.dominant_color,
                row.album_id, row.rating, row.is_deleted as i64, row.deleted_at,
            ],
        )?;
        Ok(())
    }

    pub fn find_by_uuid(&self, uuid: &str) -> Result<Option<AssetRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, asset_type, capture_timestamp, capture_utc, capture_tz_source,
             import_timestamp, hash_sha256, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at
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
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at
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
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at
             FROM assets
             WHERE is_deleted = 0 AND is_stack_hidden = 0
             ORDER BY COALESCE(capture_utc, capture_timestamp) DESC
             LIMIT ?1 OFFSET ?2",
        )?;
        let rows = stmt.query_map(params![limit as i64, offset as i64], map_asset_row)?;
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
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at
             FROM assets WHERE is_deleted = 1 AND deleted_at IS NOT NULL AND deleted_at < ?1",
        )?;
        let rows = stmt.query_map(params![threshold], map_asset_row)?;
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

    // ── AI tags (ai_tags index — structurally separate from user tags) ────────────

    /// Replace the indexed AI tags for `uuid` with `rows` (the asset's current `tags_ai` value).
    /// A projection of the sidecar OR-set; the sidecar remains the source of truth.
    pub fn replace_ai_tags(&self, uuid: &str, rows: &[AiTagRow]) -> Result<(), rusqlite::Error> {
        self.conn
            .execute("DELETE FROM ai_tags WHERE uuid = ?1", params![uuid])?;
        for row in rows {
            self.conn.execute(
                "INSERT OR IGNORE INTO ai_tags (uuid, tag, model_id, model_version) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![row.uuid, row.tag, row.model_id, row.model_version],
            )?;
        }
        Ok(())
    }

    /// The indexed AI tags for `uuid`, ordered by tag then model.
    pub fn ai_tags_for(&self, uuid: &str) -> Result<Vec<AiTagRow>, rusqlite::Error> {
        let mut stmt = self.conn.prepare(
            "SELECT uuid, tag, model_id, model_version FROM ai_tags \
             WHERE uuid = ?1 ORDER BY tag, model_id",
        )?;
        let rows = stmt.query_map(params![uuid], |r| {
            Ok(AiTagRow {
                uuid: r.get(0)?,
                tag: r.get(1)?,
                model_id: r.get(2)?,
                model_version: r.get(3)?,
            })
        })?;
        rows.collect()
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
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
mod tests {
    use super::*;
    use crate::db::rows::{AssetRow, AssetStackRow, StackMemberRow};

    fn make_asset(uuid: &str, hash: &str) -> AssetRow {
        AssetRow {
            uuid: uuid.to_string(),
            asset_type: "photo".to_string(),
            capture_timestamp: 1720000000,
            capture_utc: Some(1719997200),
            capture_tz_source: Some("offset_exif".to_string()),
            import_timestamp: 1720000000,
            hash_sha256: hash.to_string(),
            width: Some(4032),
            height: Some(3024),
            duration_ms: None,
            stack_id: None,
            is_stack_hidden: false,
            chromahash: None,
            dominant_color: None,
            album_id: None,
            rating: 0,
            is_deleted: false,
            deleted_at: None,
        }
    }

    #[test]
    fn test_init_schema_idempotent() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        db.init_schema().unwrap(); // second call — should not fail
        assert_eq!(db.schema_version().unwrap(), 4);
    }

    #[test]
    fn test_insert_and_find_by_hash() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        let asset = make_asset("uuid-1", &"a".repeat(64));
        db.insert_asset(&asset).unwrap();
        let found = db.find_by_hash(&"a".repeat(64)).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().uuid, "uuid-1");
    }

    #[test]
    fn test_find_by_hash_not_found() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        let found = db.find_by_hash(&"b".repeat(64)).unwrap();
        assert!(found.is_none());
    }

    #[test]
    fn test_query_timeline_excludes_deleted_and_hidden() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        let a1 = make_asset("uuid-1", &"a".repeat(64));
        let mut a2 = make_asset("uuid-2", &"b".repeat(64));
        let mut a3 = make_asset("uuid-3", &"c".repeat(64));
        a2.is_deleted = true;
        a3.is_stack_hidden = true;
        db.insert_asset(&a1).unwrap();
        db.insert_asset(&a2).unwrap();
        db.insert_asset(&a3).unwrap();
        let timeline = db.query_timeline(0, 100).unwrap();
        assert_eq!(timeline.len(), 1);
        assert_eq!(timeline[0].uuid, "uuid-1");
    }

    #[test]
    fn test_soft_delete() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        let asset = make_asset("uuid-1", &"a".repeat(64));
        db.insert_asset(&asset).unwrap();
        db.soft_delete("uuid-1", 1720000100).unwrap();
        let timeline = db.query_timeline(0, 100).unwrap();
        assert!(timeline.is_empty());
    }

    #[test]
    fn test_query_expired_trash() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        let asset = make_asset("uuid-1", &"a".repeat(64));
        db.insert_asset(&asset).unwrap();
        // Delete with a timestamp far in the past
        db.soft_delete("uuid-1", 100).unwrap();
        let expired = db.query_expired_trash(30 * 86400).unwrap();
        assert_eq!(expired.len(), 1);
    }

    #[test]
    fn test_update_stack_hidden() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        let asset = make_asset("uuid-1", &"a".repeat(64));
        db.insert_asset(&asset).unwrap();
        db.update_stack_hidden("uuid-1", true).unwrap();
        let timeline = db.query_timeline(0, 100).unwrap();
        assert!(timeline.is_empty());
        db.update_stack_hidden("uuid-1", false).unwrap();
        let timeline = db.query_timeline(0, 100).unwrap();
        assert_eq!(timeline.len(), 1);
    }

    #[test]
    fn test_insert_stack_and_members() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        let a1 = make_asset("uuid-1", &"a".repeat(64));
        let a2 = make_asset("uuid-2", &"b".repeat(64));
        db.insert_asset(&a1).unwrap();
        db.insert_asset(&a2).unwrap();
        let stack = AssetStackRow {
            id: "stack-1".to_string(),
            stack_type: "raw_jpeg".to_string(),
            primary_asset_id: "uuid-1".to_string(),
            cover_asset_id: Some("uuid-1".to_string()),
            is_collapsed: true,
            is_auto_generated: true,
            created_at: 1720000000,
            modified_at: 1720000000,
        };
        db.insert_stack(&stack).unwrap();
        let m1 = StackMemberRow {
            id: "m-1".to_string(),
            stack_id: "stack-1".to_string(),
            asset_id: "uuid-1".to_string(),
            sequence_order: 0,
            member_role: "primary".to_string(),
            created_at: 1720000000,
        };
        let m2 = StackMemberRow {
            id: "m-2".to_string(),
            stack_id: "stack-1".to_string(),
            asset_id: "uuid-2".to_string(),
            sequence_order: 1,
            member_role: "raw".to_string(),
            created_at: 1720000000,
        };
        db.insert_stack_member(&m1).unwrap();
        db.insert_stack_member(&m2).unwrap();
        let members = db.list_stack_members("stack-1").unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].member_role, "primary");
        assert_eq!(members[1].member_role, "raw");
    }

    #[test]
    fn test_upsert_asset() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        let mut asset = make_asset("uuid-1", &"a".repeat(64));
        db.insert_asset(&asset).unwrap();
        asset.rating = 5;
        db.upsert_asset(&asset).unwrap();
        let found = db.find_by_hash(&"a".repeat(64)).unwrap().unwrap();
        assert_eq!(found.rating, 5);
    }

    fn rep(uuid: &str, tier: &str, bytes: i64, last: i64) -> CachedRepresentationRow {
        CachedRepresentationRow {
            uuid: uuid.to_string(),
            tier: tier.to_string(),
            format: None,
            bytes,
            path: format!("/cache/{uuid}.{tier}"),
            last_accessed_at: last,
            pinned: false,
            is_owned_original: false,
        }
    }

    fn evicted_ids(rows: &[CachedRepresentationRow]) -> Vec<&str> {
        rows.iter().map(|r| r.uuid.as_str()).collect()
    }

    #[test]
    fn eviction_is_least_recently_accessed_first() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        db.upsert_representation(&rep("a", "thumbnail", 100, 10))
            .unwrap();
        db.upsert_representation(&rep("b", "thumbnail", 100, 20))
            .unwrap();
        db.upsert_representation(&rep("c", "thumbnail", 100, 30))
            .unwrap();
        // 300 B reclaimable, budget 250 → evict only the least-recently-accessed.
        let evicted = db.eviction_candidates(250).unwrap();
        assert_eq!(evicted_ids(&evicted), ["a"]);
    }

    #[test]
    fn eviction_tier_order_breaks_recency_ties() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        // Equal recency, three tiers — original (biggest, most regenerable) is evicted first.
        db.upsert_representation(&rep("x", "original", 100, 50))
            .unwrap();
        db.upsert_representation(&rep("x", "preview", 100, 50))
            .unwrap();
        db.upsert_representation(&rep("x", "thumbnail", 100, 50))
            .unwrap();
        let evicted = db.eviction_candidates(250).unwrap();
        assert_eq!(
            evicted.iter().map(|r| r.tier.as_str()).collect::<Vec<_>>(),
            ["original"]
        );
    }

    #[test]
    fn recency_promotion_protects_touched_representation() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        db.upsert_representation(&rep("a", "thumbnail", 100, 10))
            .unwrap();
        db.upsert_representation(&rep("b", "thumbnail", 100, 20))
            .unwrap();
        db.upsert_representation(&rep("c", "thumbnail", 100, 30))
            .unwrap();
        // Viewing the oldest makes it newest, so the next-oldest is evicted instead.
        db.record_access("a", "thumbnail", 100).unwrap();
        let evicted = db.eviction_candidates(250).unwrap();
        assert_eq!(evicted_ids(&evicted), ["b"]);
    }

    #[test]
    fn pinned_and_owned_originals_are_exempt() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        let mut pinned = rep("p", "original", 1000, 10);
        pinned.pinned = true;
        let mut owned = rep("o", "original", 1000, 10);
        owned.is_owned_original = true;
        db.upsert_representation(&pinned).unwrap();
        db.upsert_representation(&owned).unwrap();
        db.upsert_representation(&rep("c", "thumbnail", 100, 30))
            .unwrap();
        // Budget 0 reclaims everything reclaimable, but the exempt rows survive the sweep.
        let evicted = db.eviction_candidates(0).unwrap();
        assert_eq!(evicted_ids(&evicted), ["c"]);
    }

    #[test]
    fn eviction_empty_when_within_budget() {
        let db = DatabaseDriver::open_in_memory().unwrap();
        db.upsert_representation(&rep("a", "thumbnail", 100, 10))
            .unwrap();
        assert!(db.eviction_candidates(1000).unwrap().is_empty());
    }
}
