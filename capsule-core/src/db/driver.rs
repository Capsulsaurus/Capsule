use crate::db::rows::{AssetRow, AssetStackRow, StackMemberRow};
use crate::db::schema;
use rusqlite::{Connection, params};
use std::path::Path;

pub struct DatabaseDriver {
    conn: Connection,
}

impl DatabaseDriver {
    pub fn open(path: &Path) -> Result<Self, rusqlite::Error> {
        let conn = Connection::open(path)?;
        let driver = Self { conn };
        driver.init_schema()?;
        Ok(driver)
    }

    pub fn open_in_memory() -> Result<Self, rusqlite::Error> {
        let conn = Connection::open_in_memory()?;
        let driver = Self { conn };
        driver.init_schema()?;
        Ok(driver)
    }

    pub fn init_schema(&self) -> Result<(), rusqlite::Error> {
        self.conn.execute_batch(schema::DDL)?;
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
             import_timestamp, hash_blake3, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                row.uuid, row.asset_type, row.capture_timestamp, row.capture_utc,
                row.capture_tz_source, row.import_timestamp, row.hash_blake3,
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
             import_timestamp, hash_blake3, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
            params![
                row.uuid, row.asset_type, row.capture_timestamp, row.capture_utc,
                row.capture_tz_source, row.import_timestamp, row.hash_blake3,
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
             import_timestamp, hash_blake3, width, height, duration_ms, stack_id, is_stack_hidden,
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
             import_timestamp, hash_blake3, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at
             FROM assets WHERE hash_blake3 = ?1 LIMIT 1",
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
             import_timestamp, hash_blake3, width, height, duration_ms, stack_id, is_stack_hidden,
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
             import_timestamp, hash_blake3, width, height, duration_ms, stack_id, is_stack_hidden,
             chromahash, dominant_color, album_id, rating, is_deleted, deleted_at
             FROM assets WHERE is_deleted = 1 AND deleted_at IS NOT NULL AND deleted_at < ?1",
        )?;
        let rows = stmt.query_map(params![threshold], map_asset_row)?;
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
        hash_blake3: row.get(6)?,
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
            hash_blake3: hash.to_string(),
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
        assert_eq!(db.schema_version().unwrap(), 1);
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
}
