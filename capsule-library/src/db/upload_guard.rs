//! The `upload_guards` table: an original's source guard, recorded by the upload bundle's
//! hashing pass (SSoT: design/cryptography/encryption § Encryption Workflow).
//!
//! The row is plain columns over what `capsule_library::lifecycle::OriginalCiphertext` holds:
//! the lifecycle owns the meaning (what a guard proves and when it is released); this module
//! only stores and returns it.

use rusqlite::{OptionalExtension as _, params};

use crate::db::DatabaseDriver;

/// One persisted source guard. `u64` facts the platform reports (device and inode numbers) are
/// stored bit-for-bit as SQLite's signed 64-bit integers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UploadGuardRow {
    /// The asset whose original this guards.
    pub asset_id: String,
    /// The STREAM nonce prefix the tags were sealed under.
    pub nonce_prefix: Vec<u8>,
    /// The plaintext size the signed manifest committed to.
    pub plaintext_size: i64,
    /// The plaintext file's length on disk when the guard was recorded.
    pub source_len: i64,
    /// Its modification time, in nanoseconds since the Unix epoch, when the platform gave one.
    pub modified_ns: Option<i64>,
    /// Its device number, on Unix.
    pub file_dev: Option<i64>,
    /// Its inode number, on Unix.
    pub file_ino: Option<i64>,
    /// Its status-change time's seconds, on Unix.
    pub ctime_secs: Option<i64>,
    /// Its status-change time's nanoseconds, on Unix.
    pub ctime_nanos: Option<i64>,
    /// Every STREAM chunk's 16-byte authentication tag, concatenated in chunk order.
    pub chunk_tags: Vec<u8>,
    /// When the guard was recorded, in Unix seconds.
    pub recorded_at: i64,
}

impl DatabaseDriver {
    /// Record `row` as the guard of its asset, replacing any guard recorded before.
    pub(crate) fn upsert_upload_guard(&self, row: &UploadGuardRow) -> Result<(), rusqlite::Error> {
        self.conn.execute(
            "INSERT OR REPLACE INTO upload_guards
             (asset_id, nonce_prefix, plaintext_size, source_len, modified_ns, file_dev, file_ino,
              ctime_secs, ctime_nanos, chunk_tags, recorded_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                row.asset_id,
                row.nonce_prefix,
                row.plaintext_size,
                row.source_len,
                row.modified_ns,
                row.file_dev,
                row.file_ino,
                row.ctime_secs,
                row.ctime_nanos,
                row.chunk_tags,
                row.recorded_at,
            ],
        )?;
        Ok(())
    }

    /// The guard recorded for `asset_id`, if one is.
    pub(crate) fn upload_guard(
        &self,
        asset_id: &str,
    ) -> Result<Option<UploadGuardRow>, rusqlite::Error> {
        self.conn
            .query_row(
                "SELECT asset_id, nonce_prefix, plaintext_size, source_len, modified_ns, file_dev,
                        file_ino, ctime_secs, ctime_nanos, chunk_tags, recorded_at
                 FROM upload_guards WHERE asset_id = ?1",
                params![asset_id],
                |r| {
                    Ok(UploadGuardRow {
                        asset_id: r.get(0)?,
                        nonce_prefix: r.get(1)?,
                        plaintext_size: r.get(2)?,
                        source_len: r.get(3)?,
                        modified_ns: r.get(4)?,
                        file_dev: r.get(5)?,
                        file_ino: r.get(6)?,
                        ctime_secs: r.get(7)?,
                        ctime_nanos: r.get(8)?,
                        chunk_tags: r.get(9)?,
                        recorded_at: r.get(10)?,
                    })
                },
            )
            .optional()
    }

    /// Forget the guard of `asset_id`. Returns whether one was recorded.
    pub(crate) fn remove_upload_guard(&self, asset_id: &str) -> Result<bool, rusqlite::Error> {
        let removed = self.conn.execute(
            "DELETE FROM upload_guards WHERE asset_id = ?1",
            params![asset_id],
        )?;
        Ok(removed > 0)
    }
}
