use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use capsule_core::sidecar::LibraryConfigCbor;

use crate::library::error::LibraryError;

const SCRUB_INTERVAL_SECS: i64 = 7 * 86400;
const TMP_AGE_SECS: u64 = 5 * 60;

/// Run a startup scrub if more than 7 days have passed since the last one.
/// Removes `.tmp` files older than 5 minutes from `media/` and updates
/// `config.last_scrubbed_at`.
pub(crate) fn startup_scrub(
    root: &Path,
    config: &mut LibraryConfigCbor,
) -> Result<(), LibraryError> {
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
        .filter_map(Result::ok)
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
mod tests;
