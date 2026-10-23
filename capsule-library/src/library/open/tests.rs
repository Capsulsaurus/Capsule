use tempfile::TempDir;

use super::*;
use crate::library::init::init_library;

#[test]
fn test_open_initialized_library() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");

    // Init, then close to release the lock.
    let lib = init_library(&root, "Test").unwrap();
    let name = lib.config().library_name.clone();
    lib.close().unwrap();

    // Open
    let lib2 = open_library(&root).expect("open should succeed");
    assert_eq!(lib2.config().library_name, name);
}

#[test]
fn test_open_missing_version_returns_corrupt() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");

    // Create minimal dir structure but no version.cbor
    std::fs::create_dir_all(root.join(".library")).unwrap();

    let result = open_library(&root);
    assert!(
        matches!(result, Err(LibraryError::CorruptVersion(_))),
        "expected CorruptVersion"
    );
}

#[test]
fn test_open_updates_last_opened_at() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");

    let lib = init_library(&root, "T").unwrap();
    let t0 = lib.config().last_opened_at;
    lib.close().unwrap();

    // Small sleep to ensure time advances.
    std::thread::sleep(std::time::Duration::from_millis(10));

    let lib2 = open_library(&root).unwrap();
    assert!(
        lib2.config().last_opened_at >= t0,
        "last_opened_at should be updated"
    );
}

#[test]
fn test_open_lock_released_on_drop() {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");

    let lib = init_library(&root, "T").unwrap();
    lib.close().unwrap();

    {
        let _lib2 = open_library(&root).unwrap();
    }
    assert!(!root.join(".library/lock").exists());
}

/// **Newer-catalog refusal.** A catalog stamped by a newer build is refused with a typed
/// error naming both versions — never downgraded, never opened, and never flattened into
/// the generic `Db` arm a client can only print. The file is left byte-for-byte untouched
/// and the lock is released, so the user's *current* build can still open it after an
/// update.
#[test]
fn test_open_refuses_a_catalog_newer_than_this_build_untouched() {
    use crate::db::schema::SCHEMA_VERSION;

    let tmp = TempDir::new().unwrap();
    let root = tmp.path().join("lib");
    init_library(&root, "T").unwrap().close().unwrap();

    let db_path = root.join("index/library.sqlite");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1))
            .unwrap();
    }
    let before = std::fs::read(&db_path).unwrap();

    match open_library(&root) {
        Err(LibraryError::CatalogTooNew { found, supported }) => {
            assert_eq!(found, SCHEMA_VERSION + 1);
            assert_eq!(supported, SCHEMA_VERSION);
        }
        Err(other) => panic!("expected CatalogTooNew, got {other:?}"),
        Ok(_) => panic!("a too-new catalog must not open"),
    }
    assert_eq!(
        std::fs::read(&db_path).unwrap(),
        before,
        "a refused catalog must not be written to"
    );
    assert!(
        !root.join(".library/lock").exists(),
        "the lock is released on a refused open"
    );
    // And the same library opens fine once the stamp is back within range.
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(&format!("PRAGMA user_version = {SCHEMA_VERSION};"))
            .unwrap();
    }
    assert!(open_library(&root).is_ok());
}
