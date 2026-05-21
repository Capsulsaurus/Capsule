use std::collections::HashMap;
use std::path::PathBuf;

type StemKey = (Option<PathBuf>, String);
type StemFiles = Vec<(PathBuf, String)>;

use crate::domain::{DetectionMethod, MemberRole, StackType};
use crate::import::scan::ImportCandidate;
use crate::metadata::AssetType;

// ── Extension sets ──────────────────────────────────────────────────────────

pub const RAW_EXTS: &[&str] = &[
    "arw", "cr2", "cr3", "nef", "nrw", "rw2", "orf", "pef", "raf", "srw", "3fr", "dcr", "dng",
    "erf", "mef", "mos", "mrw", "ptx", "rwl", "x3f",
];

pub const PRIMARY_EXTS: &[&str] = &["jpg", "jpeg", "heic", "heif", "avif", "png", "tiff", "tif"];

pub const VIDEO_EXTS: &[&str] = &["mp4", "mov", "m4v", "avi", "mkv", "mts", "m2ts"];

const XMP_EXT: &str = "xmp";

pub fn is_raw(ext: &str) -> bool {
    RAW_EXTS.contains(&ext.to_lowercase().as_str())
}
pub fn is_primary(ext: &str) -> bool {
    PRIMARY_EXTS.contains(&ext.to_lowercase().as_str())
}
pub fn is_video(ext: &str) -> bool {
    VIDEO_EXTS.contains(&ext.to_lowercase().as_str())
}
pub fn is_xmp(ext: &str) -> bool {
    ext.to_lowercase() == XMP_EXT
}

pub fn is_supported_extension(ext: &str) -> bool {
    is_raw(ext) || is_primary(ext) || is_video(ext) || is_xmp(ext)
}

// ── Grouping ─────────────────────────────────────────────────────────────────

/// Group a flat list of files by directory+stem, assigning member roles.
///
/// RAW+primary pairs → `RawJpeg` stack.
/// RAW-only (no primary) → standalone (no stack_hint).
/// Primary-only → standalone.
/// XMP → paired with same-stem RAW or primary; standalone if no match.
/// Ungrouped extensions → standalone Sidecar.
///
/// Files in different parent directories are never grouped together.
pub fn group_by_stem(files: &[PathBuf]) -> Vec<ImportCandidate> {
    // Map: (parent, lowercase_stem) → Vec<(path, lowercase_ext)>
    let mut by_stem: HashMap<StemKey, StemFiles> = HashMap::new();

    for path in files {
        let parent = path.parent().map(PathBuf::from);
        let stem = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let ext = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        by_stem
            .entry((parent, stem))
            .or_default()
            .push((path.clone(), ext));
    }

    let mut candidates = Vec::new();

    for ((_, stem), paths_exts) in by_stem {
        let raws: Vec<PathBuf> = paths_exts
            .iter()
            .filter(|(_, e)| is_raw(e))
            .map(|(p, _)| p.clone())
            .collect();
        let primaries: Vec<PathBuf> = paths_exts
            .iter()
            .filter(|(_, e)| is_primary(e))
            .map(|(p, _)| p.clone())
            .collect();
        let videos: Vec<PathBuf> = paths_exts
            .iter()
            .filter(|(_, e)| is_video(e))
            .map(|(p, _)| p.clone())
            .collect();
        let xmps: Vec<PathBuf> = paths_exts
            .iter()
            .filter(|(_, e)| is_xmp(e))
            .map(|(p, _)| p.clone())
            .collect();
        let others: Vec<PathBuf> = paths_exts
            .iter()
            .filter(|(_, e)| !is_raw(e) && !is_primary(e) && !is_video(e) && !is_xmp(e))
            .map(|(p, _)| p.clone())
            .collect();

        if !raws.is_empty() && !primaries.is_empty() {
            // RAW+primary pair → RawJpeg stack
            let mut members: Vec<(PathBuf, MemberRole)> = Vec::new();
            for p in &primaries {
                members.push((p.clone(), MemberRole::Primary));
            }
            for p in &raws {
                members.push((p.clone(), MemberRole::Raw));
            }
            for p in &xmps {
                members.push((p.clone(), MemberRole::Sidecar));
            }
            let source_paths: Vec<PathBuf> = members.iter().map(|(p, _)| p.clone()).collect();
            candidates.push(ImportCandidate {
                source_paths,
                detected_type: AssetType::Photo,
                stack_type: Some(StackType::RawJpeg),
                detection_method: Some(DetectionMethod::FilenameStem),
                detection_key: Some(stem.clone()),
                members,
            });
        } else if !raws.is_empty() {
            // RAW-only (with optional same-stem XMP)
            for raw in &raws {
                let mut members = vec![(raw.clone(), MemberRole::Primary)];
                for xmp in &xmps {
                    members.push((xmp.clone(), MemberRole::Sidecar));
                }
                let source_paths = members.iter().map(|(p, _)| p.clone()).collect();
                candidates.push(ImportCandidate {
                    source_paths,
                    detected_type: AssetType::Photo,
                    stack_type: None,
                    detection_method: None,
                    detection_key: None,
                    members,
                });
            }
        } else if !primaries.is_empty() {
            // Primary-only (with optional same-stem XMP)
            for primary in &primaries {
                let mut members = vec![(primary.clone(), MemberRole::Primary)];
                for xmp in &xmps {
                    members.push((xmp.clone(), MemberRole::Sidecar));
                }
                let source_paths = members.iter().map(|(p, _)| p.clone()).collect();
                candidates.push(ImportCandidate {
                    source_paths,
                    detected_type: AssetType::Photo,
                    stack_type: None,
                    detection_method: None,
                    detection_key: None,
                    members,
                });
            }
        } else if !videos.is_empty() {
            // Video-only (standalone)
            for video in &videos {
                candidates.push(ImportCandidate {
                    source_paths: vec![video.clone()],
                    detected_type: AssetType::Video,
                    stack_type: None,
                    detection_method: None,
                    detection_key: None,
                    members: vec![(video.clone(), MemberRole::Primary)],
                });
            }
        } else if !xmps.is_empty() {
            // XMP-only (no matching RAW or primary)
            for xmp in &xmps {
                candidates.push(ImportCandidate {
                    source_paths: vec![xmp.clone()],
                    detected_type: AssetType::Sidecar,
                    stack_type: None,
                    detection_method: None,
                    detection_key: None,
                    members: vec![(xmp.clone(), MemberRole::Sidecar)],
                });
            }
        }

        // Unsupported extensions → each becomes a standalone Sidecar
        for other in &others {
            candidates.push(ImportCandidate {
                source_paths: vec![other.clone()],
                detected_type: AssetType::Sidecar,
                stack_type: None,
                detection_method: None,
                detection_key: None,
                members: vec![(other.clone(), MemberRole::Sidecar)],
            });
        }
    }

    candidates
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{MemberRole, StackType};

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(|n| PathBuf::from(n)).collect()
    }

    fn find_candidate<'a>(
        candidates: &'a [ImportCandidate],
        primary: &str,
    ) -> Option<&'a ImportCandidate> {
        candidates.iter().find(|c| {
            c.members
                .iter()
                .any(|(p, r)| p.to_string_lossy() == primary && *r == MemberRole::Primary)
        })
    }

    #[test]
    fn test_raw_jpeg_pair() {
        let files = paths(&["/photos/img_0001.jpg", "/photos/img_0001.ARW"]);
        let candidates = group_by_stem(&files);
        assert_eq!(candidates.len(), 1);
        let c = &candidates[0];
        assert_eq!(c.stack_type, Some(StackType::RawJpeg));
        assert!(c.members.iter().any(|(_, r)| *r == MemberRole::Primary));
        assert!(c.members.iter().any(|(_, r)| *r == MemberRole::Raw));
    }

    #[test]
    fn test_raw_only() {
        let files = paths(&["/photos/img_0002.CR2"]);
        let candidates = group_by_stem(&files);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].stack_type.is_none());
        assert_eq!(candidates[0].detected_type, AssetType::Photo);
        assert!(
            candidates[0]
                .members
                .iter()
                .any(|(_, r)| *r == MemberRole::Primary)
        );
    }

    #[test]
    fn test_jpeg_only() {
        let files = paths(&["/photos/img_0003.jpg"]);
        let candidates = group_by_stem(&files);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].stack_type.is_none());
    }

    #[test]
    fn test_xmp_with_raw() {
        let files = paths(&["/photos/img.ARW", "/photos/img.xmp"]);
        let candidates = group_by_stem(&files);
        assert_eq!(candidates.len(), 1);
        // RAW-only (xmp pairs as sidecar)
        assert!(
            candidates[0]
                .members
                .iter()
                .any(|(_, r)| *r == MemberRole::Sidecar)
        );
        assert!(
            candidates[0]
                .members
                .iter()
                .any(|(_, r)| *r == MemberRole::Primary)
        );
    }

    #[test]
    fn test_xmp_with_raw_and_jpeg() {
        let files = paths(&["/photos/img.ARW", "/photos/img.jpg", "/photos/img.xmp"]);
        let candidates = group_by_stem(&files);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].stack_type, Some(StackType::RawJpeg));
        assert!(
            candidates[0]
                .members
                .iter()
                .any(|(_, r)| *r == MemberRole::Sidecar)
        );
    }

    #[test]
    fn test_xmp_standalone() {
        let files = paths(&["/photos/img.xmp"]);
        let candidates = group_by_stem(&files);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].detected_type, AssetType::Sidecar);
    }

    #[test]
    fn test_multiple_independent_files() {
        let files = paths(&["/photos/a.jpg", "/photos/b.jpg"]);
        let candidates = group_by_stem(&files);
        assert_eq!(candidates.len(), 2);
    }

    #[test]
    fn test_different_dirs_not_grouped() {
        let files = paths(&["/a/img.jpg", "/b/img.ARW"]);
        let candidates = group_by_stem(&files);
        assert_eq!(
            candidates.len(),
            2,
            "files in different dirs must not be grouped"
        );
    }

    #[test]
    fn test_video_standalone() {
        let files = paths(&["/photos/clip.MOV"]);
        let candidates = group_by_stem(&files);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].detected_type, AssetType::Video);
    }
}
