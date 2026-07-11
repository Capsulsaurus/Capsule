use std::path::Path;

use crate::db::DatabaseDriver;
use crate::domain::ImportMode;
use crate::import::scan::{ImportCandidate, ScanResult};
use crate::import::upload::{StagedStreamingConflict, UploadPolicy, ensure_streaming_compatible};
use crate::library::{LibraryError, available_bytes};

/// Configuration for an import run.
#[derive(Debug, Clone)]
pub struct ImportConfig {
    pub import_mode: ImportMode,
    pub target_album_id: Option<String>,
    /// If true, import even if a file with the same SHA-256 hash already exists.
    pub force_reimport_duplicates: bool,
    /// The per-device upload policy (staged uploads, slice `S-B4`). `Full` (the
    /// default) opens every bundle session eagerly; `Staged` opens them in tier
    /// order gated by the connection class, and is **mutually exclusive** with a
    /// streaming import — the planner rejects the combination at confirmation
    /// ([`ImportActionPlan::confirm_upload_policy`]) and the streaming executor
    /// refuses it by construction.
    pub upload_policy: UploadPolicy,
}

impl Default for ImportConfig {
    fn default() -> Self {
        Self {
            import_mode: ImportMode::Copy,
            target_album_id: None,
            force_reimport_duplicates: false,
            upload_policy: UploadPolicy::Full,
        }
    }
}

/// Decision for a single candidate.
#[derive(Debug, Clone)]
pub enum ImportDecision {
    Import,
    SkipDuplicate { existing_uuid: String },
    SkipUnsupported,
    SkipError(String),
}

#[derive(Debug, Default, Clone)]
pub struct PlanCounts {
    pub to_import: usize,
    pub duplicates: usize,
    pub unsupported: usize,
    pub errors: usize,
    /// Total bytes the plan will bring into the library: the summed on-disk size of every
    /// source file of every `Import` candidate (skips contribute nothing). This is the
    /// `total_size` the confirmation UI shows and the free-space probe compares against to set
    /// [`ImportActionPlan::streaming_recommended`]. Missing/unreadable sizes count as zero — the
    /// same file already reports as a `SkipError`, so it is not double-charged as import bytes.
    pub total_size: u64,
}

/// Output of Phase 2 (plan).
#[derive(Debug)]
pub struct ImportActionPlan {
    pub actions: Vec<(ImportCandidate, ImportDecision)>,
    pub counts: PlanCounts,
    /// The free-space probe's streaming recommendation, attached **at confirmation** by
    /// [`attach_streaming_recommendation`](Self::attach_streaming_recommendation) — never by the
    /// pure planner (the probe is I/O). `None` until attached; `Some(true)` means the library
    /// volume is near/over full for this plan's `total_size` and the user should confirm a
    /// [streaming import](crate::import::streaming). Recording it on the plan (like the resolved
    /// destination `album_id`) keeps the planner deterministic.
    pub streaming_recommended: Option<bool>,
}

impl ImportActionPlan {
    /// Per-`Import`-candidate source-byte sizes, largest first — the input to the streaming
    /// minimum-headroom check (the largest single asset must fully materialize locally). Skip
    /// decisions contribute nothing.
    pub fn import_candidate_sizes(&self) -> Vec<u64> {
        let mut sizes: Vec<u64> = self
            .actions
            .iter()
            .filter(|(_, d)| matches!(d, ImportDecision::Import))
            .map(|(c, _)| candidate_size(c))
            .collect();
        sizes.sort_unstable_by(|a, b| b.cmp(a));
        sizes
    }

    /// The largest single `Import` candidate's size, or `0` when nothing will be imported.
    pub fn largest_import_size(&self) -> u64 {
        self.import_candidate_sizes().into_iter().max().unwrap_or(0)
    }

    /// Attach the free-space probe's streaming recommendation **at confirmation**: probe
    /// `library_root`'s available bytes (I/O — never inside the pure planner) and set
    /// [`streaming_recommended`](Self::streaming_recommended) from the pure
    /// [`streaming_recommended`](crate::library::streaming_recommended) predicate over this
    /// plan's `total_size`. Returns the attached verdict.
    #[tracing::instrument(skip_all, fields(total_size = self.counts.total_size))]
    pub fn attach_streaming_recommendation(
        &mut self,
        library_root: &Path,
        headroom_margin: u64,
    ) -> Result<bool, LibraryError> {
        let available = available_bytes(library_root)?;
        Ok(self.set_streaming_recommendation(available, headroom_margin))
    }

    /// The pure half of [`attach_streaming_recommendation`](Self::attach_streaming_recommendation),
    /// taking an already-probed `available` so the auto-detect verdict is unit-testable without
    /// touching a real volume. Sets and returns [`streaming_recommended`](Self::streaming_recommended).
    pub fn set_streaming_recommendation(&mut self, available: u64, headroom_margin: u64) -> bool {
        let recommended = crate::library::streaming_recommended(
            self.counts.total_size,
            available,
            headroom_margin,
        );
        self.streaming_recommended = Some(recommended);
        recommended
    }

    /// **Confirmation-time upload-policy gate** (staged uploads, slice `S-B4`).
    ///
    /// A [`UploadPolicy::Staged`] run and a streaming import are mutually exclusive
    /// per import (download-sync doc): streaming exists to release local bytes
    /// quickly, staged defers exactly the T2 upload release depends on. This is the
    /// planner's single rejection point — call it at confirmation with the run's
    /// `policy` and whether a streaming import was chosen (`use_streaming`); a
    /// conflicting combination returns [`StagedStreamingConflict`] instead of ever
    /// entering the executor. Delegates to the pure
    /// [`ensure_streaming_compatible`](crate::import::upload::ensure_streaming_compatible)
    /// invariant so the rule lives in one place.
    pub fn confirm_upload_policy(
        &self,
        policy: UploadPolicy,
        use_streaming: bool,
    ) -> Result<(), StagedStreamingConflict> {
        ensure_streaming_compatible(policy, use_streaming)
    }
}

/// Phase 2 — decide what to do with each candidate from the scan.
///
/// SHA-256-hashes the primary member of each candidate and checks the DB for
/// duplicates. Returns an `ImportActionPlan` with per-candidate decisions.
pub fn plan(
    scan: &ScanResult,
    db: &DatabaseDriver,
    config: &ImportConfig,
) -> Result<ImportActionPlan, Box<dyn std::error::Error + Send + Sync>> {
    // Validate target album if specified (fail fast before hashing anything).
    if let Some(ref _album_id) = config.target_album_id {
        // TODO: when album DB is implemented, validate here.
        // For now, pass through.
    }

    let mut actions = Vec::new();
    let mut counts = PlanCounts::default();

    for candidate in &scan.candidates {
        let decision = decide(candidate, db, config)?;
        match &decision {
            ImportDecision::Import => {
                counts.to_import += 1;
                // total_size accumulates only what will actually be imported.
                counts.total_size = counts.total_size.saturating_add(candidate_size(candidate));
            }
            ImportDecision::SkipDuplicate { .. } => counts.duplicates += 1,
            ImportDecision::SkipUnsupported => counts.unsupported += 1,
            ImportDecision::SkipError(_) => counts.errors += 1,
        }
        actions.push((candidate.clone(), decision));
    }

    Ok(ImportActionPlan {
        actions,
        counts,
        streaming_recommended: None,
    })
}

/// The summed on-disk size of every source file in `candidate`. A file whose metadata cannot be
/// read counts as zero — an unreadable file is already surfaced as a `SkipError`, so it never
/// inflates the import byte total. Shared with the [streaming executor](crate::import::streaming),
/// which names the largest candidate for the minimum-headroom error.
pub(crate) fn candidate_size(candidate: &ImportCandidate) -> u64 {
    candidate
        .source_paths
        .iter()
        .map(|p| std::fs::metadata(p).map_or(0, |m| m.len()))
        .sum()
}

fn decide(
    candidate: &ImportCandidate,
    db: &DatabaseDriver,
    config: &ImportConfig,
) -> Result<ImportDecision, Box<dyn std::error::Error + Send + Sync>> {
    // Hash the primary file (first member with Primary role, or source_paths[0])
    let primary_path = candidate.primary_path();

    let hash = match hash_file(primary_path) {
        Ok(h) => h,
        Err(e) => {
            return Ok(ImportDecision::SkipError(format!(
                "failed to hash {}: {e}",
                primary_path.display()
            )));
        }
    };

    if !config.force_reimport_duplicates
        && let Some(existing) = db.find_by_hash(&hash)?
    {
        return Ok(ImportDecision::SkipDuplicate {
            existing_uuid: existing.uuid,
        });
    }

    Ok(ImportDecision::Import)
}

fn hash_file(path: &Path) -> Result<String, std::io::Error> {
    crate::utils::hash::get_file_hash(path)
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;
    use crate::db::DatabaseDriver;
    use crate::import::scan::ScanResult;
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
        let hash = crate::utils::hash::hash_bytes(content);

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
        let dup_hash = crate::utils::hash::hash_bytes(&dup_content);
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
    /// other combination is accepted. (SSoT: download-sync doc — Validation.)
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

    #[test]
    fn test_force_reimport_duplicates() {
        let tmp = TempDir::new().unwrap();
        let db = make_db();

        let content = b"reimport_me";
        fs::write(tmp.path().join("photo.jpg"), content).unwrap();
        let hash = crate::utils::hash::hash_bytes(content);

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
        };
        db.insert_asset(&row).unwrap();

        let scan = scan(&[tmp.path().to_path_buf()]).unwrap();
        let mut config = ImportConfig::default();
        config.force_reimport_duplicates = true;
        let plan = plan(&scan, &db, &config).unwrap();

        assert_eq!(
            plan.counts.to_import, 1,
            "force_reimport should produce Import action"
        );
    }
}
