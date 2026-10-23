use std::fs;

use capsule_core::import::scan::ScanResult;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::db::DatabaseDriver;
use crate::import::scanner::scan;

fn make_db() -> DatabaseDriver {
    DatabaseDriver::open_in_memory().unwrap()
}

fn make_scan(dir: &Path, names: &[&str]) -> ScanResult {
    for name in names {
        fs::write(dir.join(name), name.as_bytes()).unwrap();
    }
    scan(&[dir.to_path_buf()]).unwrap()
}

#[test]
fn test_non_duplicate_gives_import() {
    let tmp = TempDir::new().unwrap();
    let db = make_db();
    let scan = make_scan(tmp.path(), &["photo.jpg"]);
    let config = ImportConfig::default();
    let plan = plan(&scan, &db, &config).unwrap();
    assert_eq!(plan.counts.to_import, 1);
    assert!(matches!(plan.actions[0].1, ImportDecision::Import));
}

#[test]
fn test_duplicate_hash_skipped() {
    let tmp = TempDir::new().unwrap();
    let db = make_db();

    // Write a file and pre-insert its hash
    let content = b"unique_photo_content";
    fs::write(tmp.path().join("photo.jpg"), content).unwrap();
    let hash = capsule_core::crypto::hash::hash_bytes(content).to_hex();

    let row = crate::db::rows::AssetRow {
        uuid: "existing-uuid".to_string(),
        asset_type: "photo".to_string(),
        capture_timestamp: 1,
        capture_utc: None,
        capture_tz_source: None,
        import_timestamp: 1,
        hash_sha256: hash,
        width: None,
        height: None,
        duration_ms: None,
        stack_id: None,
        is_stack_hidden: false,
        chromahash: None,
        dominant_color: None,
        album_id: None,
        rating: 0,
        is_deleted: false,
        deleted_at: None,
        is_hidden: false,
    };
    db.insert_asset(&row).unwrap();

    let scan = scan(&[tmp.path().to_path_buf()]).unwrap();
    let config = ImportConfig::default();
    let plan = plan(&scan, &db, &config).unwrap();

    assert_eq!(plan.counts.duplicates, 1);
    assert!(matches!(
        plan.actions[0].1,
        ImportDecision::SkipDuplicate { .. }
    ));
}

#[test]
fn total_size_sums_only_imported_candidates() {
    let tmp = TempDir::new().unwrap();
    let db = make_db();

    // Two importable files (10 + 25 bytes) and one duplicate (skipped, not counted).
    fs::write(tmp.path().join("a.jpg"), vec![0u8; 10]).unwrap();
    fs::write(tmp.path().join("b.jpg"), vec![0u8; 25]).unwrap();
    let dup_content = vec![7u8; 99];
    fs::write(tmp.path().join("dup.jpg"), &dup_content).unwrap();
    let dup_hash = capsule_core::crypto::hash::hash_bytes(&dup_content).to_hex();
    let row = crate::db::rows::AssetRow {
        uuid: "dup-uuid".to_string(),
        asset_type: "photo".to_string(),
        capture_timestamp: 1,
        capture_utc: None,
        capture_tz_source: None,
        import_timestamp: 1,
        hash_sha256: dup_hash,
        width: None,
        height: None,
        duration_ms: None,
        stack_id: None,
        is_stack_hidden: false,
        chromahash: None,
        dominant_color: None,
        album_id: None,
        rating: 0,
        is_deleted: false,
        deleted_at: None,
        is_hidden: false,
    };
    db.insert_asset(&row).unwrap();

    let scan = scan(&[tmp.path().to_path_buf()]).unwrap();
    let plan = plan(&scan, &db, &ImportConfig::default()).unwrap();

    assert_eq!(plan.counts.to_import, 2);
    assert_eq!(plan.counts.duplicates, 1);
    // Only the two imported files (10 + 25) are charged; the duplicate is not.
    assert_eq!(plan.counts.total_size, 35);
    // Largest single import is the 25-byte file (the min-headroom input).
    assert_eq!(plan.largest_import_size(), 25);
    assert_eq!(plan.import_candidate_sizes(), vec![25, 10]);
}

/// Streaming auto-detect (unit): with `available_bytes()` mocked below and above
/// `total_size + headroom`, `streaming_recommended` is set in the constrained case and clear
/// otherwise. Exercised through the pure `set_streaming_recommendation` so no real volume is
/// needed. (SSoT: pipeline doc — Validation, "Streaming auto-detect".)
#[test]
fn streaming_auto_detect_sets_recommendation_from_probe() {
    let tmp = TempDir::new().unwrap();
    let db = make_db();
    fs::write(tmp.path().join("big.jpg"), vec![0u8; 1_000]).unwrap();
    let mut plan = plan(
        &scan(&[tmp.path().to_path_buf()]).unwrap(),
        &db,
        &ImportConfig::default(),
    )
    .unwrap();
    assert_eq!(plan.counts.total_size, 1_000);
    assert_eq!(
        plan.streaming_recommended, None,
        "pure planner never sets it"
    );

    // Roomy volume: total_size + headroom (1_000 + 100) is well under available (10_000).
    assert!(!plan.set_streaming_recommendation(10_000, 100));
    assert_eq!(plan.streaming_recommended, Some(false));

    // Constrained volume: total_size + headroom (1_000 + 100) meets/exceeds available (1_050).
    assert!(plan.set_streaming_recommendation(1_050, 100));
    assert_eq!(plan.streaming_recommended, Some(true));
}

/// **Planner staged × streaming exclusion (unit).** A plan confirmed with both a
/// staged upload policy and a streaming import is rejected at confirmation; every
/// other combination is accepted. (SSoT: transfer upload doc — Upload Tiering.)
#[test]
fn staged_and_streaming_is_rejected_at_confirmation() {
    let tmp = TempDir::new().unwrap();
    let db = make_db();
    fs::write(tmp.path().join("a.jpg"), vec![0u8; 100]).unwrap();
    let plan = plan(
        &scan(&[tmp.path().to_path_buf()]).unwrap(),
        &db,
        &ImportConfig::default(),
    )
    .unwrap();

    // The one rejected combination.
    assert_eq!(
        plan.confirm_upload_policy(UploadPolicy::Staged, true),
        Err(StagedStreamingConflict)
    );
    // Every compatible combination confirms.
    assert!(
        plan.confirm_upload_policy(UploadPolicy::Staged, false)
            .is_ok()
    );
    assert!(plan.confirm_upload_policy(UploadPolicy::Full, true).is_ok());
    assert!(
        plan.confirm_upload_policy(UploadPolicy::Full, false)
            .is_ok()
    );
}

/// **Regression guard.** Codec coverage must never gate *admission*.
///
/// Capsule is a backup tool: declining to back up every HEIC because we cannot thumbnail it
/// turns a cosmetic gap into data loss. Formats with no decoder in this build —
/// HEIC/AVIF/TIFF and every RAW flavour — plan as `Import` exactly like JPEG, and so do
/// video, unrecognised extensions, extensionless and zero-byte files (issue #695). There is
/// no "unsupported" decision to land in.
///
/// If someone reintroduces an admission-time codec or extension gate, this test fails.
#[test]
fn undecodable_stills_are_never_skipped_at_plan_time() {
    let tmp = TempDir::new().unwrap();
    let db = make_db();
    fs::write(tmp.path().join("empty.dat"), b"").unwrap();
    // Distinct stems: same-stem files are grouped into a single candidate.
    let scan = make_scan(
        tmp.path(),
        &[
            "a.jpg", "b.png", "c.heic", "d.avif", "e.tiff", "f.ARW", "g.dng", "h.CR2", "i.mp4",
            "j.mov", "k.pdf", "l.xyz", "README",
        ],
    );

    let plan = plan(&scan, &db, &ImportConfig::default()).unwrap();

    assert_eq!(plan.counts.errors, 0);
    assert_eq!(
        plan.counts.to_import, 14,
        "every file is admitted regardless of codec coverage or extension"
    );
    assert!(
        plan.actions
            .iter()
            .all(|(_, d)| matches!(d, ImportDecision::Import)),
        "every candidate must plan as Import"
    );
    // Undecodable originals are charged against the import byte total like any other file —
    // they really are being copied into the library.
    assert!(plan.counts.total_size > 0);
}

#[test]
fn test_force_reimport_duplicates() {
    let tmp = TempDir::new().unwrap();
    let db = make_db();

    let content = b"reimport_me";
    fs::write(tmp.path().join("photo.jpg"), content).unwrap();
    let hash = capsule_core::crypto::hash::hash_bytes(content).to_hex();

    let row = crate::db::rows::AssetRow {
        uuid: "existing-uuid2".to_string(),
        asset_type: "photo".to_string(),
        capture_timestamp: 1,
        capture_utc: None,
        capture_tz_source: None,
        import_timestamp: 1,
        hash_sha256: hash,
        width: None,
        height: None,
        duration_ms: None,
        stack_id: None,
        is_stack_hidden: false,
        chromahash: None,
        dominant_color: None,
        album_id: None,
        rating: 0,
        is_deleted: false,
        deleted_at: None,
        is_hidden: false,
    };
    db.insert_asset(&row).unwrap();

    let scan = scan(&[tmp.path().to_path_buf()]).unwrap();
    let config = ImportConfig {
        force_reimport_duplicates: true,
        ..ImportConfig::default()
    };
    let plan = plan(&scan, &db, &config).unwrap();

    assert_eq!(
        plan.counts.to_import, 1,
        "force_reimport should produce Import action"
    );
}

/// The planner consumes the resolution context instead of a raw album id, and
/// records **which rule fired** so a surprising destination is explainable from the plan
/// alone (SSoT: organization § The Default Album).
#[test]
fn plan_records_the_resolved_album_and_the_rule_that_chose_it() {
    use capsule_core::import::default_album::ResolutionRule;

    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("a.jpg"), b"aaa").unwrap();
    let db = DatabaseDriver::open_in_memory().unwrap();
    let scan = scan(&[tmp.path().to_path_buf()]).unwrap();

    let derived = Uuid::from_u128(0x33);
    let pick = Uuid::from_u128(0x11);

    // Bound with only the derived de facto album: the terminal rule fires.
    let mut config = ImportConfig {
        album: DefaultAlbumContext::derived(derived),
        ..ImportConfig::default()
    };
    let recorded = plan(&scan, &db, &config).unwrap().album.unwrap();
    assert_eq!(recorded.album_id, derived);
    assert_eq!(recorded.rule, ResolutionRule::DerivedDeFacto);

    // An explicit pick outranks it, and that is what the plan records.
    config.album.explicit_pick = Some(pick);
    let recorded = plan(&scan, &db, &config).unwrap().album.unwrap();
    assert_eq!(recorded.album_id, pick);
    assert_eq!(recorded.rule, ResolutionRule::ExplicitPick);

    // An unbound context records nothing — the executor binds the library's derived
    // album and runs the same resolution. Planning still succeeds.
    let plan_unbound = plan(&scan, &db, &ImportConfig::default()).unwrap();
    assert!(plan_unbound.album.is_none());
    assert_eq!(plan_unbound.counts.to_import, 1);
}

/// Planning is deterministic in the destination too: the same context yields the same
/// recorded resolution on every run.
#[test]
fn recorded_resolution_is_deterministic_across_runs() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("a.jpg"), b"aaa").unwrap();
    let db = DatabaseDriver::open_in_memory().unwrap();
    let scan = scan(&[tmp.path().to_path_buf()]).unwrap();
    let config = ImportConfig {
        album: DefaultAlbumContext {
            owner_default_album_id: Some(Uuid::from_u128(0x22)),
            ..DefaultAlbumContext::derived(Uuid::from_u128(0x33))
        },
        ..ImportConfig::default()
    };

    let first = plan(&scan, &db, &config).unwrap().album;
    for _ in 0..4 {
        assert_eq!(plan(&scan, &db, &config).unwrap().album, first);
    }
}
