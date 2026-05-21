use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::domain::{DetectionMethod, MemberRole, StackType};
use crate::import::group::{group_by_stem, is_supported_extension, is_video};
use crate::import::scan::{ImportCandidate, ScanResult};
use crate::import::special::SpecialDirectoryStatus;
use crate::metadata::AssetType;

/// Phase 1 — scan source directories and build a list of `ImportCandidate`s.
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
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
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
        })
        .filter_map(|e| e.ok())
    {
        let path = entry.path().to_path_buf();
        if !path.is_file() {
            continue;
        }

        let ext = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();

        if !is_supported_extension(&ext) {
            continue;
        }

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
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_file(dir: &Path, name: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, b"placeholder").unwrap();
        p
    }

    #[test]
    fn test_single_jpeg_standalone() {
        let tmp = TempDir::new().unwrap();
        create_file(tmp.path(), "photo.jpg");
        let result = scan(&[tmp.path().to_path_buf()]).unwrap();
        assert_eq!(result.candidates.len(), 1);
        let c = &result.candidates[0];
        assert!(c.stack_type.is_none());
        assert_eq!(c.detected_type, AssetType::Photo);
    }

    #[test]
    fn test_raw_jpeg_pair() {
        let tmp = TempDir::new().unwrap();
        create_file(tmp.path(), "img_0001.jpg");
        create_file(tmp.path(), "img_0001.ARW");
        let result = scan(&[tmp.path().to_path_buf()]).unwrap();
        assert_eq!(result.candidates.len(), 1);
        assert_eq!(result.candidates[0].stack_type, Some(StackType::RawJpeg));
    }

    #[test]
    fn test_raw_only() {
        let tmp = TempDir::new().unwrap();
        create_file(tmp.path(), "img_0002.CR2");
        let result = scan(&[tmp.path().to_path_buf()]).unwrap();
        assert_eq!(result.candidates.len(), 1);
        assert!(result.candidates[0].stack_type.is_none());
    }

    #[test]
    fn test_git_dir_skipped() {
        let tmp = TempDir::new().unwrap();
        let git = tmp.path().join(".git");
        fs::create_dir_all(&git).unwrap();
        create_file(&git, "config");
        create_file(tmp.path(), "photo.jpg");
        let result = scan(&[tmp.path().to_path_buf()]).unwrap();
        assert_eq!(
            result.candidates.len(),
            1,
            "files inside .git should be skipped"
        );
    }

    #[test]
    fn test_xmp_pairs_with_raw() {
        let tmp = TempDir::new().unwrap();
        create_file(tmp.path(), "img.NEF");
        create_file(tmp.path(), "img.xmp");
        let result = scan(&[tmp.path().to_path_buf()]).unwrap();
        assert_eq!(result.candidates.len(), 1);
        let c = &result.candidates[0];
        assert!(c.members.iter().any(|(_, r)| *r == MemberRole::Sidecar));
    }

    #[test]
    fn test_empty_directory() {
        let tmp = TempDir::new().unwrap();
        let result = scan(&[tmp.path().to_path_buf()]).unwrap();
        assert!(result.candidates.is_empty());
    }

    #[test]
    fn test_unsupported_extension_skipped() {
        let tmp = TempDir::new().unwrap();
        create_file(tmp.path(), "document.pdf");
        let result = scan(&[tmp.path().to_path_buf()]).unwrap();
        // PDF is not in the supported extension list
        assert!(result.candidates.is_empty());
    }
}
