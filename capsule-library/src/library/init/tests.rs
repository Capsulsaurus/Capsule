use tempfile::TempDir;

use super::*;
use crate::sidecar_io::read_library_version;

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
    assert_eq!(
        lib.db.schema_version().unwrap(),
        crate::db::schema::SCHEMA_VERSION
    );

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
