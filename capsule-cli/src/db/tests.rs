//! The [`LocalDb`] handle: it opens a file in WAL mode, migrated, shares one connection between
//! clones, and refuses a database the migrator refuses.

use std::path::PathBuf;

use super::*;

/// A database file removed (with its WAL companions) when dropped.
struct TempFile(PathBuf);

impl TempFile {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("capsule-cli-db-{}.sqlite", uuid::Uuid::new_v4())))
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let mut path = self.0.clone().into_os_string();
            path.push(suffix);
            let _ = std::fs::remove_file(path);
        }
    }
}

#[tokio::test]
async fn a_new_file_opens_in_wal_mode_at_the_current_version() {
    let file = TempFile::new();
    let db = LocalDb::open_async(file.0.clone()).await.expect("opens");
    let (mode, version) = db
        .call(|conn| {
            let mode: String = conn.query_row("PRAGMA journal_mode", [], |row| row.get(0))?;
            Ok((mode, migrate::read_user_version(conn)?))
        })
        .await
        .expect("reads");
    assert_eq!(mode, "wal");
    assert_eq!(version, SCHEMA_VERSION);
}

#[tokio::test]
async fn two_handles_on_one_file_see_each_others_writes() {
    let file = TempFile::new();
    let first = LocalDb::open_async(file.0.clone()).await.expect("opens");
    let second = LocalDb::open_async(file.0.clone())
        .await
        .expect("opens again");
    first
        .call(|conn| conn.execute("INSERT INTO sync_cursor VALUES (0, x'07')", []))
        .await
        .expect("writes");
    let cursor: Vec<u8> = second
        .clone()
        .call(|conn| conn.query_row("SELECT cursor FROM sync_cursor", [], |row| row.get(0)))
        .await
        .expect("reads");
    assert_eq!(cursor, [7]);
}

#[tokio::test]
async fn a_database_from_a_newer_build_is_refused_on_open() {
    let file = TempFile::new();
    {
        let conn = Connection::open(&file.0).expect("opens raw");
        conn.execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1))
            .expect("stamps");
    }
    let error = LocalDb::open_async(file.0.clone())
        .await
        .expect_err("refused");
    assert!(matches!(
        error,
        OpenError::Migration(MigrationError::TooNew { .. })
    ));
}

#[tokio::test]
async fn a_failed_unit_of_work_returns_its_sqlite_error() {
    let file = TempFile::new();
    let db = LocalDb::open_async(file.0.clone()).await.expect("opens");
    let error = db
        .call(|conn| {
            conn.execute_batch(
                "INSERT INTO peering_floor VALUES ('d', 1); DELETE FROM peering_floor;",
            )
        })
        .await
        .expect_err("the trigger refuses");
    assert!(matches!(error, DbError::Sqlite(_)));
}
