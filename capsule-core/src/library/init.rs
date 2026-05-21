use std::fs;
use std::path::Path;

use crate::db::DatabaseDriver;
use crate::library::error::LibraryError;
use crate::library::library::Library;
use crate::library::lock;
use crate::sidecar::library_version::{CURRENT_LIBRARY_VERSION, LibraryVersionCbor};
use crate::sidecar::{
    LibraryConfigCbor,
    io::{write_library_config, write_library_version},
};

/// Skeleton directory paths (relative to library root).
const SKELETON_DIRS: &[&str] = &[
    "media",
    "index",
    "index/thumbnails",
    "index/thumbnails/xs",
    "index/thumbnails/s",
    "index/thumbnails/m",
    "index/thumbnails/l",
    "index/thumbnails/xl",
    "index/thumbnails/o",
    "index/meta",
    "index/transcodes",
    "index/transcodes/h264",
    "index/transcodes/live",
    ".library",
    ".library/migrations",
    ".library/trash",
];

/// Create a new Capsule library at `root` with the given name.
///
/// Fails if `root` is a non-empty directory. If `root` does not exist it is
/// created. On any failure after the first directory is created, all created
/// directories are removed before returning the error.
pub fn init_library(root: &Path, name: &str) -> Result<Library, LibraryError> {
    let root_existed = root.exists();
    if root_existed {
        let has_entries = fs::read_dir(root)
            .map_err(LibraryError::Io)?
            .next()
            .is_some();
        if has_entries {
            return Err(LibraryError::DirectoryNotEmpty);
        }
    }

    let result = init_inner(root, name);
    if let Err(ref _e) = result
        && !root_existed
    {
        let _ = fs::remove_dir_all(root);
    }
    result
}

fn init_inner(root: &Path, name: &str) -> Result<Library, LibraryError> {
    for dir in SKELETON_DIRS {
        fs::create_dir_all(root.join(dir)).map_err(LibraryError::Io)?;
    }

    let now = now_secs();

    // Initialise SQLite.
    let db_path = root.join("index/library.sqlite");
    let db = DatabaseDriver::open(&db_path).map_err(LibraryError::Db)?;

    // Write .library/version.cbor
    let version_path = root.join(".library/version.cbor");
    write_library_version(
        &version_path,
        &LibraryVersionCbor {
            version: CURRENT_LIBRARY_VERSION,
        },
    )
    .map_err(|e| LibraryError::Cbor(e.to_string()))?;

    // Write .library/config.cbor
    let config = LibraryConfigCbor {
        schema_version: 1,
        library_name: name.to_string(),
        last_opened_at: now,
        last_scrubbed_at: None,
    };
    let config_path = root.join(".library/config.cbor");
    write_library_config(&config_path, &config).map_err(|e| LibraryError::Cbor(e.to_string()))?;

    // Acquire lock last — Library::drop will release it.
    lock::try_acquire(root)?;

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
    use crate::sidecar::io::read_library_version;
    use tempfile::TempDir;

    #[test]
    fn test_init_creates_skeleton_dirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("mylib");

        let lib = init_library(&root, "My Library").expect("init should succeed");

        for dir in SKELETON_DIRS {
            assert!(root.join(dir).is_dir(), "missing skeleton dir: {dir}");
        }

        // version.cbor is readable
        let ver = read_library_version(&root.join(".library/version.cbor")).unwrap();
        assert_eq!(ver.version, CURRENT_LIBRARY_VERSION);

        // SQLite has the correct schema version
        assert_eq!(lib.db.schema_version().unwrap(), 1);

        // Config has the correct library name
        assert_eq!(lib.config().library_name, "My Library");

        // Lock file exists
        assert!(root.join(".library/lock").exists());
    }

    #[test]
    fn test_init_fails_on_nonempty_dir() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("existing_file.txt"), b"hi").unwrap();

        let result = init_library(tmp.path(), "test");
        assert!(
            matches!(result, Err(LibraryError::DirectoryNotEmpty)),
            "expected DirectoryNotEmpty"
        );
    }

    #[test]
    fn test_init_on_nonexistent_dir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("a/b/c");

        let lib = init_library(&root, "Deep").expect("should create nested dirs");
        assert!(root.join("media").is_dir());
        drop(lib); // releases lock
    }

    #[test]
    fn test_init_lock_released_on_drop() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path().join("lib");
        {
            let _lib = init_library(&root, "test").unwrap();
            assert!(root.join(".library/lock").exists());
        }
        assert!(
            !root.join(".library/lock").exists(),
            "lock should be released on drop"
        );
    }
}
