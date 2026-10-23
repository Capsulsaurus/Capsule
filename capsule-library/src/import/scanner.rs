use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use capsule_core::domain::{DetectionMethod, MemberRole, StackType};
use capsule_core::import::group::{group_by_stem, is_video};
use capsule_core::import::scan::{ImportCandidate, ScanResult};
use capsule_core::import::special::SpecialDirectoryStatus;
use capsule_core::metadata::AssetType;
use walkdir::WalkDir;

/// Phase 1 — scan source directories and build a list of `ImportCandidate`s.
///
/// **Every regular file is a candidate, whatever its extension** (SSoT: thumbnails § Media Is
/// Rawshift's — "every file is kept"). Codec coverage and extension recognition decide grouping
/// and derivatives, never admission: a video, a HEIC, a zero-byte file or a file with an
/// unrecognised or missing extension is backed up byte for byte like a JPEG. The only files
/// left out are the operating system's own folder metadata (`is_os_folder_metadata`) and the
/// contents of special directories (`.git`, DaVinci Resolve), none of which is a user original.
///
/// An entry the walk cannot read (a permission-denied directory, a vanished file, a missing
/// source root) is never dropped silently: it becomes a standalone candidate the planner then
/// reports as a `SkipError` naming the path, so a run that could not see part of its source
/// says so.
///
/// Responsibilities:
/// - Recursive walkdir traversal, skipping special directories (`.git`, DaVinci).
/// - Live Photo pairing: HEIC with `content_identifier` XMP field + matching `.mov`.
/// - Filename-stem grouping for RAW/JPEG pairs (delegated to `group_by_stem`).
/// - Standalone files for everything else.
pub fn scan(
    source_paths: &[PathBuf],
) -> Result<ScanResult, Box<dyn std::error::Error + Send + Sync>> {
    let mut all_files: Vec<PathBuf> = Vec::new();
    // content_identifier → heic_path (for Live Photo detection)
    let mut content_id_map: HashMap<String, PathBuf> = HashMap::new();
    // content_identifier → mov_path
    let mut mov_ci_map: HashMap<String, PathBuf> = HashMap::new();

    for source in source_paths {
        collect_files(source, &mut all_files, &mut content_id_map, &mut mov_ci_map);
    }

    // Match Live Photo pairs: HEIC (with CI) + MOV (with same CI)
    let mut live_photo_heics: Vec<PathBuf> = Vec::new();
    let mut live_photo_movs: Vec<PathBuf> = Vec::new();
    let mut live_photo_pairs: Vec<ImportCandidate> = Vec::new();

    for (ci, heic) in &content_id_map {
        if let Some(mov) = mov_ci_map.get(ci) {
            live_photo_heics.push(heic.clone());
            live_photo_movs.push(mov.clone());
            live_photo_pairs.push(ImportCandidate {
                source_paths: vec![heic.clone(), mov.clone()],
                detected_type: AssetType::Photo,
                stack_type: Some(StackType::LivePhoto),
                detection_method: Some(DetectionMethod::ContentIdentifier),
                detection_key: Some(ci.clone()),
                members: vec![
                    (heic.clone(), MemberRole::Primary),
                    (mov.clone(), MemberRole::Video),
                ],
            });
        }
    }

    // Remove paired files from the flat list before stem grouping
    let paired: Vec<PathBuf> = live_photo_heics
        .iter()
        .chain(live_photo_movs.iter())
        .cloned()
        .collect();
    let remaining: Vec<PathBuf> = all_files
        .into_iter()
        .filter(|p| !paired.contains(p))
        .collect();

    // Group remaining files by stem (RAW+JPEG detection, etc.)
    let mut stem_candidates = group_by_stem(&remaining);

    // Combine all candidates
    let mut candidates = live_photo_pairs;
    candidates.append(&mut stem_candidates);

    Ok(ScanResult { candidates })
}

fn collect_files(
    root: &Path,
    files: &mut Vec<PathBuf>,
    content_id_map: &mut HashMap<String, PathBuf>,
    mov_ci_map: &mut HashMap<String, PathBuf>,
) {
    for entry in WalkDir::new(root).into_iter().filter_entry(|e| {
        // Skip special directories
        if e.file_type().is_dir()
            && let Some(status) = SpecialDirectoryStatus::from_path(e.path())
        {
            match status {
                SpecialDirectoryStatus::Git | SpecialDirectoryStatus::DavinciResolve => {
                    return false;
                }
            }
        }
        true
    }) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                // Kept as a candidate so the planner reports it: a subtree the walk could not
                // read is part of the source the user asked for, not something to skip.
                tracing::warn!(
                    path = ?error.path(),
                    %error,
                    "import scan: an entry could not be read; reporting it as an error"
                );
                if let Some(path) = error.path() {
                    files.push(path.to_path_buf());
                }
                continue;
            }
        };
        let path = entry.path().to_path_buf();
        if !path.is_file() {
            continue;
        }
        if is_os_folder_metadata(&path) {
            tracing::trace!(path = %path.display(), "import scan: OS folder metadata left out");
            continue;
        }

        let ext = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();

        // For HEIC files: try to extract Apple content_identifier for Live Photo pairing
        if (ext == "heic" || ext == "heif")
            && let Some(ci) = extract_content_identifier(&path)
        {
            content_id_map.insert(ci, path.clone());
            // Don't add to regular files yet — handle in Live Photo logic
            files.push(path);
            continue;
        }

        // For MOV files: try to extract content_identifier for Live Photo pairing
        if is_video(&ext)
            && (ext == "mov" || ext == "mp4")
            && let Some(ci) = extract_content_identifier(&path)
        {
            mov_ci_map.insert(ci, path.clone());
            files.push(path);
            continue;
        }

        files.push(path);
    }
}

/// Whether `path` is folder metadata an operating system writes beside a user's files, rather
/// than a file the user put there: macOS Finder's `.DS_Store` and its AppleDouble `._*`
/// companions, and Windows Explorer's `Thumbs.db` and `desktop.ini`. They are regenerated by
/// the OS on demand, so leaving them out of a backup loses nothing.
pub(crate) fn is_os_folder_metadata(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name == ".DS_Store"
        || name.starts_with("._")
        || name.eq_ignore_ascii_case("thumbs.db")
        || name.eq_ignore_ascii_case("desktop.ini")
}

/// Naive byte-scan for the Apple QuickTime content identifier.
///
/// Searches for the ASCII string `com.apple.quicktime.content.identifier`
/// followed by a null-terminated or length-prefixed ASCII value in the file.
/// Returns `None` if not found or unreadable.
fn extract_content_identifier(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let marker = b"com.apple.quicktime.content.identifier";

    let pos = bytes.windows(marker.len()).position(|w| w == marker)?;

    // Skip past the marker and a few bytes of framing (null + length byte etc.)
    let after = &bytes[pos + marker.len()..];
    // Skip up to 32 bytes of framing, looking for a printable ASCII identifier
    for start in 0..32.min(after.len()) {
        let ch = after[start];
        // Identifiers are typically UUID strings (hex + hyphens, 36 bytes)
        if ch.is_ascii_alphanumeric() || ch == b'-' {
            let end = after[start..]
                .iter()
                .position(|&b| !b.is_ascii_alphanumeric() && b != b'-')
                .unwrap_or(after.len() - start)
                + start;
            let candidate = std::str::from_utf8(&after[start..end]).ok()?;
            if candidate.len() >= 8 {
                return Some(candidate.to_string());
            }
        }
    }

    None
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
