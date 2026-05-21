use std::path::Path;

use crate::db::DatabaseDriver;
use crate::library::error::LibraryError;
use crate::library::library::Library;
use crate::library::lock;
use crate::library::scrub::startup_scrub;
use crate::sidecar::io::{read_library_config, read_library_version, write_library_config};
use crate::sidecar::library_version::CURRENT_LIBRARY_VERSION;

/// Open an existing Capsule library at `root`.
///
/// Validates the version file, acquires the lock, runs a startup scrub if
/// needed, and updates `last_opened_at`.
pub fn open_library(root: &Path) -> Result<Library, LibraryError> {
    // 1. Read and validate version.
    let version_path = root.join(".library/version.cbor");
    let version = read_library_version(&version_path)
        .map_err(|e| LibraryError::CorruptVersion(e.to_string()))?;

    if version.version != CURRENT_LIBRARY_VERSION {
        return Err(LibraryError::VersionMismatch {
            found: version.version,
            expected: CURRENT_LIBRARY_VERSION,
        });
    }

    // 2. Acquire lock.
    lock::try_acquire(root)?;

    // 3. Open DB (release lock on failure).
    let db_path = root.join("index/library.sqlite");
    let db = DatabaseDriver::open(&db_path).map_err(|e| {
        let _ = lock::release(root);
        LibraryError::Db(e)
    })?;

    // 4. Read config (release lock on failure).
    let config_path = root.join(".library/config.cbor");
    let mut config = read_library_config(&config_path).map_err(|e| {
        let _ = lock::release(root);
        LibraryError::Cbor(e.to_string())
    })?;

    // 5. Startup scrub (release lock on failure).
    startup_scrub(root, &mut config).inspect_err(|_e| {
        let _ = lock::release(root);
    })?;

    // 6. Update last_opened_at.
    config.last_opened_at = now_secs();
    write_library_config(&config_path, &config).map_err(|e| {
        let _ = lock::release(root);
        LibraryError::Cbor(e.to_string())
    })?;

    Ok(Library::new(root.to_path_buf(), db, config))
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::init::init_library;
    use tempfile::TempDir;

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
}
