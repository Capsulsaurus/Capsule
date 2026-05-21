use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use crate::library::error::LibraryError;
use crate::sidecar::LibraryConfigCbor;

const SCRUB_INTERVAL_SECS: i64 = 7 * 86400;
const TMP_AGE_SECS: u64 = 5 * 60;

/// Run a startup scrub if more than 7 days have passed since the last one.
/// Removes `.tmp` files older than 5 minutes from `media/` and updates
/// `config.last_scrubbed_at`.
pub fn startup_scrub(root: &Path, config: &mut LibraryConfigCbor) -> Result<(), LibraryError> {
    let now = now_secs();
    let needs_scrub = match config.last_scrubbed_at {
        None => true,
        Some(last) => (now - last) > SCRUB_INTERVAL_SECS,
    };

    if !needs_scrub {
        return Ok(());
    }

    let media_dir = root.join("media");
    if media_dir.exists() {
        let stale_cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(TMP_AGE_SECS))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        scrub_tmp_files_before(&media_dir, stale_cutoff)?;
    }

    config.last_scrubbed_at = Some(now);
    Ok(())
}

/// Removes `.tmp` files whose mtime is before `stale_cutoff`.
pub(crate) fn scrub_tmp_files_before(
    dir: &Path,
    stale_cutoff: SystemTime,
) -> Result<(), LibraryError> {
    for entry in walkdir::WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        if !path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .ends_with(".tmp")
        {
            continue;
        }
        match path.metadata().and_then(|m| m.modified()) {
            Ok(mtime) if mtime < stale_cutoff => {
                let _ = fs::remove_file(path);
            }
            _ => {}
        }
    }
    Ok(())
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

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
        let mut config = crate::sidecar::LibraryConfigCbor {
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
        let mut config = crate::sidecar::LibraryConfigCbor {
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
        let mut config = crate::sidecar::LibraryConfigCbor {
            schema_version: 1,
            library_name: "test".to_string(),
            last_opened_at: now,
            last_scrubbed_at: Some(now - 8 * 86400), // 8 days ago → overdue
        };

        startup_scrub(tmp.path(), &mut config).unwrap();
        assert!(config.last_scrubbed_at.unwrap() >= now);
    }
}
