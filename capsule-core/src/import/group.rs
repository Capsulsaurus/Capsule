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

pub(crate) fn is_raw(ext: &str) -> bool {
    RAW_EXTS.contains(&ext.to_lowercase().as_str())
}
pub(crate) fn is_primary(ext: &str) -> bool {
    PRIMARY_EXTS.contains(&ext.to_lowercase().as_str())
}
pub fn is_video(ext: &str) -> bool {
    VIDEO_EXTS.contains(&ext.to_lowercase().as_str())
}
pub(crate) fn is_xmp(ext: &str) -> bool {
    ext.to_lowercase() == XMP_EXT
}

// ── Grouping ─────────────────────────────────────────────────────────────────

/// Group a flat list of files by directory+stem, assigning member roles.
///
/// RAW+primary pairs → `RawJpeg` stack.
/// RAW-only (no primary) → standalone (no stack_hint).
/// Primary-only → standalone.
/// Video → always standalone, even beside a same-stem still.
/// XMP → paired with the same-stem RAW+primary stack, or, among unpaired same-stem stills,
/// with the first in path order only; standalone if no still shares its stem.
/// Ungrouped extensions → standalone Sidecar.
///
/// Every input file lands in exactly one candidate.
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
        } else if !raws.is_empty() || !primaries.is_empty() {
            // RAW-only or primary-only: each still is its own standalone candidate. The
            // same-stem XMPs ride with the first still in path order only, so one XMP never
            // lands in two candidates whatever order the scan listed the files in (issue #788).
            let mut stills = if raws.is_empty() {
                primaries.clone()
            } else {
                raws.clone()
            };
            stills.sort();
            for (index, still) in stills.iter().enumerate() {
                let mut members = vec![(still.clone(), MemberRole::Primary)];
                if index == 0 {
                    for xmp in &xmps {
                        members.push((xmp.clone(), MemberRole::Sidecar));
                    }
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
        }

        // Every video is its own standalone candidate, whether or not a still shares its stem:
        // an unpaired `IMG_0001.JPG` + `IMG_0001.MOV` is two originals, and neither may be
        // dropped (issue #695). Live Photo pairs were matched by content identifier before
        // this grouping ran.
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

        // An XMP rides with a same-stem still above; with no still to ride with (alone, or
        // beside a video only) it is kept as its own candidate.
        if raws.is_empty() && primaries.is_empty() {
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

        // Unrecognised extensions → each is kept as its own standalone candidate. The
        // extension lists above decide grouping only, never whether a file is backed up.
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
mod tests;
