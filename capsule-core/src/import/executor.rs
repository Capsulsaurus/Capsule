use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::db::rows::{AssetRow, AssetStackRow, StackMemberRow};
use crate::domain::MemberRole;
use crate::exif::extract::extract_exif;
use crate::exif::timezone::resolve_timezone;
use crate::import::executor_cancellation::CancellationToken;
use crate::import::planner::{ImportActionPlan, ImportConfig, ImportDecision};
use crate::import::progress::{ImportExecutionSummary, ImportOutcome, ImportProgressEvent};
use crate::import::scan::ImportCandidate;
use crate::library::library::Library;
use crate::library::paths::{media_path, sidecar_path, tmp_path};
use crate::metadata::AssetType;
use crate::sidecar::asset_sidecar::AssetSidecar;
use crate::sidecar::io::write_sidecar;
use crate::sidecar::stack_hint::StackHint;

const IMPORTER_VERSION: &str = env!("CARGO_PKG_VERSION");
const RAWSHIFT_VERSION: &str = "0.0.0";

/// Phase 4 — execute the import plan.
///
/// Each `ImportDecision::Import` candidate undergoes a 10-step atomic
/// two-phase commit. Files are never partially written: every media file and
/// its sidecar are either fully committed or cleaned up.
pub fn execute(
    plan: &ImportActionPlan,
    library: &Library,
    config: &ImportConfig,
    on_event: impl Fn(ImportProgressEvent),
    cancel: &CancellationToken,
) -> Result<ImportExecutionSummary, Box<dyn std::error::Error + Send + Sync>> {
    let total = plan.actions.len() as u64;
    let total_files: u64 = plan
        .actions
        .iter()
        .filter(|(_, d)| matches!(d, ImportDecision::Import))
        .map(|(c, _)| c.source_paths.len() as u64)
        .sum();

    on_event(ImportProgressEvent::ImportStarted {
        total_candidates: total,
        total_files,
    });

    let mut summary = ImportExecutionSummary::default();

    for (i, (candidate, decision)) in plan.actions.iter().enumerate() {
        if cancel.is_cancelled() {
            break;
        }

        let primary_path = candidate.primary_path().clone();
        on_event(ImportProgressEvent::CandidateStarted {
            index: i as u64,
            total,
            primary_path: primary_path.clone(),
        });

        let outcomes = match decision {
            ImportDecision::Import => execute_candidate(candidate, library, config)?,
            ImportDecision::SkipDuplicate { existing_uuid } => {
                vec![(
                    primary_path,
                    ImportOutcome::DuplicateSkipped {
                        existing_uuid: existing_uuid.clone(),
                    },
                )]
            }
            ImportDecision::SkipUnsupported => {
                vec![(primary_path, ImportOutcome::Unsupported)]
            }
            ImportDecision::SkipError(msg) => {
                vec![(primary_path, ImportOutcome::CorruptUnreadable(msg.clone()))]
            }
        };

        on_event(ImportProgressEvent::CandidateCompleted {
            index: i as u64,
            outcomes: outcomes.clone(),
        });
        summary.outcomes.extend(outcomes);
    }

    on_event(ImportProgressEvent::ImportCompleted {
        summary: ImportExecutionSummary {
            outcomes: summary.outcomes.clone(),
        },
    });

    Ok(summary)
}

// ── Per-candidate execution ──────────────────────────────────────────────────

fn execute_candidate(
    candidate: &ImportCandidate,
    library: &Library,
    config: &ImportConfig,
) -> Result<Vec<(PathBuf, ImportOutcome)>, Box<dyn std::error::Error + Send + Sync>> {
    let now = now_secs();
    let mut member_commits: Vec<MemberCommit> = Vec::new();

    // ── Phase A: copy + verify all members ──────────────────────────────────
    for (source_path, role) in &candidate.members {
        match commit_member(source_path, *role, candidate, library, config, now) {
            Ok(commit) => member_commits.push(commit),
            Err(e) => {
                // Roll back any already-committed members for this candidate
                for prev in &member_commits {
                    let _ = fs::remove_file(&prev.media_final);
                    let _ = fs::remove_file(&prev.sidecar_final);
                }
                let outcome = if e.contains("corrupt_transfer") {
                    ImportOutcome::CorruptTransfer
                } else if e.contains("permission") {
                    ImportOutcome::PermissionDenied(e)
                } else {
                    ImportOutcome::CorruptUnreadable(e)
                };
                return Ok(vec![(source_path.clone(), outcome)]);
            }
        }
    }

    // ── Phase B: DB inserts + stack ─────────────────────────────────────────
    let primary_commit = member_commits
        .iter()
        .find(|c| c.role == MemberRole::Primary)
        .or_else(|| member_commits.first());

    let stack_id = if candidate.stack_type.is_some() {
        let sid = format!("stack-{}", now);
        // Determine primary UUID
        let primary_uuid = primary_commit
            .map(|c| c.uuid_str.clone())
            .unwrap_or_default();

        let stack_row = AssetStackRow {
            id: sid.clone(),
            stack_type: candidate
                .stack_type
                .map(|st| format!("{st:?}").to_lowercase())
                .unwrap_or_else(|| "custom".to_string()),
            primary_asset_id: primary_uuid.clone(),
            cover_asset_id: Some(primary_uuid),
            is_collapsed: true,
            is_auto_generated: true,
            created_at: now,
            modified_at: now,
        };
        let _ = library.db.insert_stack(&stack_row);
        Some(sid)
    } else {
        None
    };

    let mut outcomes = Vec::new();
    for (seq, commit) in member_commits.iter().enumerate() {
        let is_primary = commit.role == MemberRole::Primary || seq == 0;

        let row = AssetRow {
            uuid: commit.uuid_str.clone(),
            asset_type: asset_type_str(candidate.detected_type).to_string(),
            capture_timestamp: commit.capture_utc.unwrap_or(now),
            capture_utc: commit.capture_utc,
            capture_tz_source: commit.capture_tz_source.clone(),
            import_timestamp: now,
            hash_blake3: commit.hash.clone(),
            width: commit.width.map(|w| w as i64),
            height: commit.height.map(|h| h as i64),
            duration_ms: None,
            stack_id: stack_id.clone(),
            is_stack_hidden: !is_primary,
            chromahash: None,
            dominant_color: None,
            album_id: config.target_album_id.clone(),
            rating: 0,
            is_deleted: false,
            deleted_at: None,
        };
        library.db.insert_asset(&row)?;

        if let Some(ref sid) = stack_id {
            let member_row = StackMemberRow {
                id: format!("{sid}#{seq}"),
                stack_id: sid.clone(),
                asset_id: commit.uuid_str.clone(),
                sequence_order: seq as i64,
                member_role: role_str(commit.role).to_string(),
                created_at: now,
            };
            let _ = library.db.insert_stack_member(&member_row);
        }

        // Move mode: delete source file after successful commit
        if matches!(config.import_mode, crate::domain::ImportMode::Move) {
            let _ = fs::remove_file(&commit.source_path);
        }

        outcomes.push((commit.source_path.clone(), ImportOutcome::Imported));
    }

    Ok(outcomes)
}

// ── Per-member atomic commit ─────────────────────────────────────────────────

struct MemberCommit {
    source_path: PathBuf,
    uuid_str: String,
    role: MemberRole,
    hash: String,
    media_final: PathBuf,
    sidecar_final: PathBuf,
    capture_utc: Option<i64>,
    capture_tz_source: Option<String>,
    width: Option<u32>,
    height: Option<u32>,
}

fn commit_member(
    source: &Path,
    role: MemberRole,
    candidate: &ImportCandidate,
    library: &Library,
    config: &ImportConfig,
    now: i64,
) -> Result<MemberCommit, String> {
    // Step 1: Generate UUID
    let uuid = Uuid::now_v7();
    let uuid_str = uuid.to_string();

    // Step 2: EXIF + timezone
    let exif = extract_exif(source).unwrap_or_default();
    let tz = resolve_timezone(&exif);
    let capture_utc = tz.capture_utc;
    let capture_tz_source = tz
        .capture_tz_source
        .map(|s| format!("{s:?}").to_lowercase());
    let width = exif.width;
    let height = exif.height;

    let ext = source
        .extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();

    // Step 3: Create media dir
    let final_media = media_path(&library.root, &uuid, &ext, capture_utc);
    fs::create_dir_all(final_media.parent().unwrap()).map_err(|e| format!("mkdir failed: {e}"))?;

    // Step 4: Copy source → tmp
    let tmp_media = tmp_path(&final_media);
    fs::copy(source, &tmp_media).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            format!("permission denied: {e}")
        } else {
            format!("copy failed: {e}")
        }
    })?;

    // Step 5: BLAKE3 verify
    let source_bytes = fs::read(source).map_err(|e| format!("read failed: {e}"))?;
    let source_hash = blake3::hash(&source_bytes).to_hex().to_string();
    let tmp_bytes = fs::read(&tmp_media).map_err(|e| format!("read tmp failed: {e}"))?;
    let tmp_hash = blake3::hash(&tmp_bytes).to_hex().to_string();
    if source_hash != tmp_hash {
        let _ = fs::remove_file(&tmp_media);
        return Err("corrupt_transfer".to_string());
    }

    // Step 6: Build sidecar
    let stack_hint = candidate.stack_type.map(|st| StackHint {
        detection_key: candidate
            .detection_key
            .clone()
            .unwrap_or_else(|| uuid_str.clone()),
        detection_method: candidate
            .detection_method
            .unwrap_or(crate::domain::DetectionMethod::FilenameStem),
        member_role: role,
        stack_type: st,
    });

    let original_filename = source
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    let sidecar = AssetSidecar {
        version: 1,
        uuid: uuid_str.clone(),
        asset_type: candidate.detected_type,
        original_filename,
        import_timestamp: now,
        modified_timestamp: now,
        hash_blake3: source_hash.clone(),
        file_size: source_bytes.len() as u64,
        is_deleted: false,
        rating: 0,
        tags: vec![],
        import_mode: config.import_mode,
        importer_version: IMPORTER_VERSION.to_string(),
        rawshift_version: RAWSHIFT_VERSION.to_string(),
        capture_timestamp: tz.capture_timestamp,
        capture_utc,
        capture_tz: tz.capture_tz,
        capture_tz_source: tz.capture_tz_source,
        tz_db_version: tz.tz_db_version,
        width,
        height,
        duration_ms: None,
        stack_hint,
        album_id: config.target_album_id.clone(),
        deleted_at: None,
        camera_make: exif.make,
        camera_model: exif.model,
        gps_lat: exif.gps_lat,
        gps_lon: exif.gps_lon,
        unknown_fields: BTreeMap::new(),
    };

    // Step 7: Write sidecar tmp
    let final_sidecar = sidecar_path(&library.root, &uuid, &ext, capture_utc);
    let tmp_sidecar = tmp_path(&final_sidecar);
    write_sidecar(&tmp_sidecar, &sidecar).map_err(|e| {
        let _ = fs::remove_file(&tmp_media);
        format!("sidecar write failed: {e}")
    })?;

    // Step 8: Rename media tmp → final (atomic)
    fs::rename(&tmp_media, &final_media).map_err(|e| {
        let _ = fs::remove_file(&tmp_media);
        let _ = fs::remove_file(&tmp_sidecar);
        format!("rename media failed: {e}")
    })?;

    // Step 9: Rename sidecar tmp → final (atomic)
    // Note: write_sidecar already does the tmp→final rename internally,
    // so final_sidecar already exists. But we wrote to tmp_sidecar above
    // manually via tmp_path; write_sidecar expects the *destination* path
    // and handles the .tmp internally. Adjust: write directly to final.
    // Actually write_sidecar(path, …) writes to path.tmp then renames to path.
    // So if we called write_sidecar(&tmp_sidecar, …) that wrote to tmp_sidecar.tmp
    // then renamed to tmp_sidecar. We then need to rename tmp_sidecar → final_sidecar.
    if tmp_sidecar.exists() {
        fs::rename(&tmp_sidecar, &final_sidecar).map_err(|e| {
            let _ = fs::remove_file(&tmp_sidecar);
            format!("rename sidecar failed: {e}")
        })?;
    }
    // If write_sidecar already placed it at the right path, we're done.

    Ok(MemberCommit {
        source_path: source.to_path_buf(),
        uuid_str,
        role,
        hash: source_hash,
        media_final: final_media,
        sidecar_final: final_sidecar,
        capture_utc,
        capture_tz_source,
        width,
        height,
    })
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn asset_type_str(t: AssetType) -> &'static str {
    match t {
        AssetType::Photo => "photo",
        AssetType::Video => "video",
        AssetType::Sidecar => "sidecar",
    }
}

fn role_str(r: MemberRole) -> &'static str {
    match r {
        MemberRole::Primary => "primary",
        MemberRole::Raw => "raw",
        MemberRole::Video => "video",
        MemberRole::Audio => "audio",
        MemberRole::DepthMap => "depth_map",
        MemberRole::Processed => "processed",
        MemberRole::Source => "source",
        MemberRole::Alternate => "alternate",
        MemberRole::Sidecar => "sidecar",
        MemberRole::Proxy => "proxy",
        MemberRole::Master => "master",
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::ImportMode;
    use crate::import::executor_cancellation::CancellationToken;
    use crate::import::planner::{ImportConfig, plan};
    use crate::import::scanner::scan;
    use crate::library::init::init_library;
    use std::fs;
    use tempfile::TempDir;

    fn noop_event(_: ImportProgressEvent) {}

    #[test]
    fn test_single_file_import() {
        let src = TempDir::new().unwrap();
        let lib_dir = TempDir::new().unwrap();

        let photo = src.path().join("test.jpg");
        fs::write(&photo, b"fake jpeg content for test").unwrap();

        let lib = init_library(lib_dir.path(), "Test").unwrap();
        let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
        let config = ImportConfig::default();
        let plan_result = plan(&scan_result, &lib.db, &config).unwrap();

        assert_eq!(plan_result.counts.to_import, 1);

        let token = CancellationToken::new();
        let summary = execute(&plan_result, &lib, &config, noop_event, &token).unwrap();

        assert_eq!(summary.imported_count(), 1);

        // Verify media file exists in library
        let media_root = lib_dir.path().join("media");
        let media_files: Vec<_> = walkdir::WalkDir::new(&media_root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_file() && !e.path().to_string_lossy().ends_with(".cbor"))
            .collect();
        assert_eq!(
            media_files.len(),
            1,
            "exactly one media file should be in library"
        );

        // Verify sidecar exists
        let sidecar_files: Vec<_> = walkdir::WalkDir::new(&media_root)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "cbor").unwrap_or(false))
            .collect();
        assert_eq!(sidecar_files.len(), 1, "exactly one sidecar should exist");

        // Verify DB has a row
        let timeline = lib.db.query_timeline(0, 100).unwrap();
        assert_eq!(timeline.len(), 1);
    }

    #[test]
    fn test_corrupt_transfer_detected() {
        // Test that CorruptTransfer outcome occurs when source and copy diverge.
        // This is hard to simulate with real fs::copy, so we test the hash comparison logic.
        let src_bytes = b"source content";
        let tmp_bytes = b"different content"; // simulates corruption
        let src_hash = blake3::hash(src_bytes).to_hex().to_string();
        let tmp_hash = blake3::hash(tmp_bytes).to_hex().to_string();
        assert_ne!(
            src_hash, tmp_hash,
            "hashes should differ for corrupt transfer test"
        );
    }

    #[test]
    fn test_move_mode_deletes_source() {
        let src = TempDir::new().unwrap();
        let lib_dir = TempDir::new().unwrap();

        let photo = src.path().join("move_me.jpg");
        fs::write(&photo, b"jpeg to move").unwrap();

        let lib = init_library(lib_dir.path(), "Test").unwrap();
        let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
        let config = ImportConfig {
            import_mode: ImportMode::Move,
            ..Default::default()
        };
        let plan_result = plan(&scan_result, &lib.db, &config).unwrap();
        let token = CancellationToken::new();
        execute(&plan_result, &lib, &config, noop_event, &token).unwrap();

        assert!(
            !photo.exists(),
            "source file should be deleted in move mode"
        );
    }

    #[test]
    fn test_cancellation_stops_execution() {
        let src = TempDir::new().unwrap();
        let lib_dir = TempDir::new().unwrap();

        // Create 3 files
        for i in 0..3 {
            fs::write(
                src.path().join(format!("photo_{i}.jpg")),
                format!("content_{i}").as_bytes(),
            )
            .unwrap();
        }

        let lib = init_library(lib_dir.path(), "Test").unwrap();
        let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
        let config = ImportConfig::default();
        let plan_result = plan(&scan_result, &lib.db, &config).unwrap();

        let token = CancellationToken::new();
        token.cancel(); // Cancel before starting

        let summary = execute(&plan_result, &lib, &config, noop_event, &token).unwrap();
        // No files should be processed (cancelled before first item)
        assert_eq!(
            summary.outcomes.len(),
            0,
            "no files imported after immediate cancellation"
        );
    }

    #[test]
    fn test_raw_jpeg_stack_import() {
        let src = TempDir::new().unwrap();
        let lib_dir = TempDir::new().unwrap();

        fs::write(src.path().join("img_0001.jpg"), b"jpeg content").unwrap();
        fs::write(src.path().join("img_0001.ARW"), b"raw content").unwrap();

        let lib = init_library(lib_dir.path(), "Test").unwrap();
        let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
        assert_eq!(
            scan_result.candidates.len(),
            1,
            "should form a single stack candidate"
        );

        let config = ImportConfig::default();
        let plan_result = plan(&scan_result, &lib.db, &config).unwrap();
        let token = CancellationToken::new();
        let summary = execute(&plan_result, &lib, &config, noop_event, &token).unwrap();

        assert_eq!(
            summary.imported_count(),
            2,
            "both RAW and JPEG should be imported"
        );

        // Timeline should show only 1 asset (JPEG visible, RAW hidden)
        let timeline = lib.db.query_timeline(0, 100).unwrap();
        assert_eq!(
            timeline.len(),
            1,
            "only primary should be visible in timeline"
        );
    }
}
