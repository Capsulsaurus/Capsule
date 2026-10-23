use tempfile::TempDir;

use super::*;

#[test]
fn sweep_deletes_cache_files_and_rows_but_not_canonical_media() {
    let dir = TempDir::new().unwrap();
    let db = DatabaseDriver::open_in_memory().unwrap();

    // A reclaimable thumbnail file and a canonical media file that must survive.
    let thumb = dir.path().join("thumb.jpg");
    let canonical = dir.path().join("original.jpg");
    fs::write(&thumb, vec![0u8; 500]).unwrap();
    fs::write(&canonical, b"canonical bytes").unwrap();

    db.upsert_representation(&CachedRepresentationRow {
        uuid: "a".to_string(),
        tier: "thumbnail".to_string(),
        format: Some("jpg".to_string()),
        bytes: 500,
        path: thumb.to_string_lossy().into_owned(),
        last_accessed_at: 1,
        pinned: false,
        is_owned_original: false,
    })
    .unwrap();

    // Budget 0 → reclaim everything reclaimable.
    let report = cache_sweep(&db, 0).unwrap();
    assert_eq!(report.evicted.len(), 1);
    assert_eq!(report.bytes_reclaimed, 500);
    assert!(!thumb.exists(), "evicted cache file is deleted");
    assert!(canonical.exists(), "canonical media is untouched");
    assert!(
        db.eviction_candidates(0).unwrap().is_empty(),
        "the index row is removed"
    );
}

#[test]
fn sweep_missing_file_is_not_an_error() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    db.upsert_representation(&CachedRepresentationRow {
        uuid: "gone".to_string(),
        tier: "preview".to_string(),
        format: None,
        bytes: 10,
        path: "/nonexistent/path/preview.bin".to_string(),
        last_accessed_at: 1,
        pinned: false,
        is_owned_original: false,
    })
    .unwrap();
    let report = cache_sweep(&db, 0).unwrap();
    assert_eq!(report.evicted.len(), 1);
    assert!(db.eviction_candidates(0).unwrap().is_empty());
}
