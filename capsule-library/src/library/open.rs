use std::path::Path;

use capsule_core::sidecar::library_version::CURRENT_LIBRARY_VERSION;

use crate::db::{DatabaseDriver, MigrationError};
use crate::library::error::LibraryError;
use crate::library::library::Library;
use crate::library::lock;
use crate::library::scrub::startup_scrub;
use crate::sidecar_io::{read_library_config, read_library_version, write_library_config};

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

    // 3. Open DB (release lock on failure). A catalog stamped newer than this build is the
    //    one open failure with its own recovery (update the app), so it keeps its type.
    let db_path = root.join("index/library.sqlite");
    let db = DatabaseDriver::open_typed(&db_path).map_err(|e| {
        let _ = lock::release(root);
        match e {
            MigrationError::CatalogTooNew { found, supported } => {
                LibraryError::CatalogTooNew { found, supported }
            }
            other => LibraryError::Db(other.into()),
        }
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
mod tests;
