use std::path::Path;

use crate::db::DatabaseDriver;
use crate::domain::ImportMode;
use crate::import::scan::{ImportCandidate, ScanResult};

/// Configuration for an import run.
#[derive(Debug, Clone)]
pub struct ImportConfig {
    pub import_mode: ImportMode,
    pub target_album_id: Option<String>,
    /// If true, import even if a file with the same BLAKE3 hash already exists.
    pub force_reimport_duplicates: bool,
}

impl Default for ImportConfig {
    fn default() -> Self {
        Self {
            import_mode: ImportMode::Copy,
            target_album_id: None,
            force_reimport_duplicates: false,
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
}

/// Output of Phase 2 (plan).
#[derive(Debug)]
pub struct ImportActionPlan {
    pub actions: Vec<(ImportCandidate, ImportDecision)>,
    pub counts: PlanCounts,
}

/// Phase 2 — decide what to do with each candidate from the scan.
///
/// BLAKE3-hashes the primary member of each candidate and checks the DB for
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
            ImportDecision::Import => counts.to_import += 1,
            ImportDecision::SkipDuplicate { .. } => counts.duplicates += 1,
            ImportDecision::SkipUnsupported => counts.unsupported += 1,
            ImportDecision::SkipError(_) => counts.errors += 1,
        }
        actions.push((candidate.clone(), decision));
    }

    Ok(ImportActionPlan { actions, counts })
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
    let bytes = std::fs::read(path)?;
    let hash = blake3::hash(&bytes);
    Ok(hash.to_hex().to_string())
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::DatabaseDriver;
    use crate::import::scan::ScanResult;
    use crate::import::scanner::scan;
    use std::fs;
    use tempfile::TempDir;

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
        let hash = blake3::hash(content).to_hex().to_string();

        let row = crate::db::rows::AssetRow {
            uuid: "existing-uuid".to_string(),
            asset_type: "photo".to_string(),
            capture_timestamp: 1,
            capture_utc: None,
            capture_tz_source: None,
            import_timestamp: 1,
            hash_blake3: hash,
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
    fn test_force_reimport_duplicates() {
        let tmp = TempDir::new().unwrap();
        let db = make_db();

        let content = b"reimport_me";
        fs::write(tmp.path().join("photo.jpg"), content).unwrap();
        let hash = blake3::hash(content).to_hex().to_string();

        let row = crate::db::rows::AssetRow {
            uuid: "existing-uuid2".to_string(),
            asset_type: "photo".to_string(),
            capture_timestamp: 1,
            capture_utc: None,
            capture_tz_source: None,
            import_timestamp: 1,
            hash_blake3: hash,
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
