use capsule_core::sidecar::{LibraryConfigCbor, LibraryVersionCbor};
use tempfile::TempDir;

use super::*;

#[test]
fn test_write_read_library_version() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("version.cbor");
    let v = LibraryVersionCbor { version: 1 };
    write_library_version(&path, &v).unwrap();
    assert!(path.exists());
    let read_back = read_library_version(&path).unwrap();
    assert_eq!(v, read_back);
}

#[test]
fn test_write_read_library_config() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config.cbor");
    let cfg = LibraryConfigCbor {
        schema_version: 1,
        library_name: "Test".to_string(),
        last_opened_at: 1_720_000_000,
        last_scrubbed_at: None,
    };
    write_library_config(&path, &cfg).unwrap();
    let read_back = read_library_config(&path).unwrap();
    assert_eq!(cfg, read_back);
}

#[test]
fn test_write_read_library_config_with_scrubbed() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("config2.cbor");
    let cfg = LibraryConfigCbor {
        schema_version: 1,
        library_name: "My Library".to_string(),
        last_opened_at: 1_720_000_000,
        last_scrubbed_at: Some(1_719_990_000),
    };
    write_library_config(&path, &cfg).unwrap();
    let read_back = read_library_config(&path).unwrap();
    assert_eq!(cfg, read_back);
}
