use std::fs;

use tempfile::TempDir;

use super::*;

#[test]
fn test_scrub_removes_old_tmp() {
    let tmp = TempDir::new().unwrap();
    let media = tmp.path().join("media/2024/2024-07");
    fs::create_dir_all(&media).unwrap();
    fs::write(media.join("abc.jpg.tmp"), b"stale").unwrap();
    fs::write(media.join("abc.cbor.tmp"), b"stale").unwrap();
    // Write a non-.tmp file that must be kept
    fs::write(media.join("abc.jpg"), b"real").unwrap();

    // Use SystemTime::now() + 1s as cutoff so that all files (just created) look "old"
    let future = SystemTime::now() + Duration::from_secs(2);
    scrub_tmp_files_before(tmp.path().join("media").as_path(), future).unwrap();

    assert!(
        !media.join("abc.jpg.tmp").exists(),
        "old .tmp should be removed"
    );
    assert!(
        !media.join("abc.cbor.tmp").exists(),
        "old .cbor.tmp should be removed"
    );
    assert!(
        media.join("abc.jpg").exists(),
        "non-.tmp file should be kept"
    );
}

#[test]
fn test_scrub_keeps_files_newer_than_cutoff() {
    let tmp = TempDir::new().unwrap();
    let media = tmp.path().join("media");
    fs::create_dir_all(&media).unwrap();
    fs::write(media.join("fresh.jpg.tmp"), b"fresh").unwrap();

    // Use UNIX_EPOCH as cutoff → nothing is older than epoch → nothing deleted
    scrub_tmp_files_before(&media, SystemTime::UNIX_EPOCH).unwrap();

    assert!(
        media.join("fresh.jpg.tmp").exists(),
        "file newer than cutoff should be kept"
    );
}

#[test]
fn test_startup_scrub_skipped_if_recent() {
    let tmp = TempDir::new().unwrap();
    let media = tmp.path().join("media");
    fs::create_dir_all(&media).unwrap();
    fs::write(media.join("stale.jpg.tmp"), b"x").unwrap();

    let now = now_secs();
    let mut config = capsule_core::sidecar::LibraryConfigCbor {
        schema_version: 1,
        library_name: "test".to_string(),
        last_opened_at: now,
        last_scrubbed_at: Some(now - 60), // 60 s ago — well under the 7-day threshold
    };

    startup_scrub(tmp.path(), &mut config).unwrap();
    // Scrub was skipped, so the file should still be there
    assert!(media.join("stale.jpg.tmp").exists());
}

#[test]
fn test_startup_scrub_runs_if_never_scrubbed() {
    let tmp = TempDir::new().unwrap();
    let media = tmp.path().join("media");
    fs::create_dir_all(&media).unwrap();
    // Write a file with a very old mtime — we can't easily set mtime, so instead we
    // verify that last_scrubbed_at is updated (the scrub runs). Actual file cleanup
    // is tested by test_scrub_removes_old_tmp.
    let now = now_secs();
    let mut config = capsule_core::sidecar::LibraryConfigCbor {
        schema_version: 1,
        library_name: "test".to_string(),
        last_opened_at: now,
        last_scrubbed_at: None,
    };

    startup_scrub(tmp.path(), &mut config).unwrap();
    assert!(
        config.last_scrubbed_at.is_some(),
        "last_scrubbed_at should be set after scrub"
    );
    assert!(config.last_scrubbed_at.unwrap() >= now);
}

#[test]
fn test_startup_scrub_updates_timestamp_if_overdue() {
    let tmp = TempDir::new().unwrap();
    let media = tmp.path().join("media");
    fs::create_dir_all(&media).unwrap();

    let now = now_secs();
    let mut config = capsule_core::sidecar::LibraryConfigCbor {
        schema_version: 1,
        library_name: "test".to_string(),
        last_opened_at: now,
        last_scrubbed_at: Some(now - 8 * 86400), // 8 days ago → overdue
    };

    startup_scrub(tmp.path(), &mut config).unwrap();
    assert!(config.last_scrubbed_at.unwrap() >= now);
}
