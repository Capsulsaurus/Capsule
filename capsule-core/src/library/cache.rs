//! Adaptive cache eviction — the client-side "Space Recovery" sweep.
//!
//! Keeps the reclaimable cache (the `cache/` tree plus fetched-but-unpinned originals) within a
//! byte budget by evicting least-recently-accessed representations first, with tier order
//! (original → preview → thumbnail) breaking recency ties. Pinned representations, device-owned
//! originals, the canonical `media/` files, and the `library.sqlite` index are never touched.
//! The budget is a plain parameter so `capsule-sdk` connection-class detection can drive it.
//!
//! SSoT: design/filesystem/client § Space Recovery.

use std::fs;
use std::path::Path;

use crate::db::DatabaseDriver;
use crate::db::rows::CachedRepresentationRow;

/// What an eviction sweep reclaimed.
#[derive(Debug, Default, PartialEq)]
pub struct EvictionReport {
    /// The representations evicted, worst-first (least-recently-accessed).
    pub evicted: Vec<CachedRepresentationRow>,
    /// Total bytes reclaimed.
    pub bytes_reclaimed: i64,
}

/// Reclaim cached representations until the reclaimable set fits within `budget_bytes`. Deletes
/// each evicted representation's cache file from disk and removes its index row. A cache file that
/// is already gone is treated as reclaimed (not an error) and its stale row is still dropped.
/// Pinned and device-owned originals are never evicted by this automatic path; canonical `media/`
/// files are never candidates (they are not tracked in `cached_representations`).
pub fn cache_sweep(
    db: &DatabaseDriver,
    budget_bytes: i64,
) -> Result<EvictionReport, rusqlite::Error> {
    let candidates = db.eviction_candidates(budget_bytes)?;
    let mut report = EvictionReport::default();
    for row in candidates {
        let _ = fs::remove_file(Path::new(&row.path));
        db.remove_representation(&row.uuid, &row.tier)?;
        report.bytes_reclaimed += row.bytes;
        report.evicted.push(row);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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
}
