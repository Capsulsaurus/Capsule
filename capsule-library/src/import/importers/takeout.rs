//! Google Takeout source adapter.
//!
//! Walks a Google Photos [Takeout] export — one directory tree, or the several extracted parts of
//! a split archive — pairs each media file with its JSON sidecar (photo-taken time, GPS,
//! description, favorites) and its album folder, and folds the out-of-band exporter metadata into
//! [`ExtractedMetadata`] under the [precedence rule](super): embedded EXIF wins over the exporter's
//! records, **except** for album membership, favorites, and user-typed descriptions, which the
//! exporter is authoritative for.
//!
//! **Every file is kept.** JSON is the exporter's sidecar and album-manifest channel; every other
//! file of an export is a media file whatever its extension (a GIF, a WebP, a 3GP video, a suffix
//! Capsule has never seen). Only Takeout's own named non-media artefacts (`archive_browser.html`)
//! and the operating system's folder metadata (`.DS_Store`, `._*`, `Thumbs.db`, `desktop.ini`, the
//! same rule as the plain scanner) are left out, each with a log line.
//!
//! Takeout's structural quirks are handled here so the pure planner never special-cases them:
//!
//! - **Truncated filenames.** Takeout truncates long sidecar names; the media file and its JSON can
//!   share only a prefix. The JSON's `title` carries the true original name, and a prefix fallback
//!   pairs the two.
//! - **`(1)` duplicates.** A same-named second file becomes `photo(1).jpg`, but its JSON keeps the
//!   counter *after* the extension: `photo.jpg(1).json`. Both are normalized to a `(base, ext, dup)`
//!   key so they pair without cross-matching the un-suffixed original.
//! - **Edited / original pairs.** `photo.jpg` (original) and `photo-edited.jpg` (Google's edit)
//!   collapse into one stacked candidate — the edited rendition never becomes a separate asset.
//! - **Split archives.** A media file and its JSON sidecar can land in different export parts; all
//!   parts are walked into one pool before pairing, so the pair is reunited.
//!
//! [Takeout]: https://docs/design/import/pipeline/#third-party-importers

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use capsule_core::domain::{DetectionMethod, MemberRole, StackType};
use capsule_core::import::group;
use capsule_core::import::scan::ImportCandidate;
use capsule_core::metadata::AssetType;
use jiff::Timestamp;
use serde::Deserialize;
use walkdir::WalkDir;

use crate::exif::{ExifExtract, extract_exif, resolve_timezone};
use crate::import::importers::{
    AdapterError, ExtractedImport, ExtractedMetadata, FoldSource, GeoPoint, ImportProvider,
    SourceAdapter, SourceEntry,
};
use crate::import::scanner::is_os_folder_metadata;

/// Minimum shared-prefix length before the truncation fallback will pair a media file with a
/// truncated JSON sidecar — long enough that unrelated files in the same folder never collide.
const TRUNCATION_MIN_PREFIX: usize = 16;

/// Google localizes the "-edited" suffix it appends to an edited rendition; a representative set of
/// the common locales. Matched case-insensitively against the file stem.
const EDITED_MARKERS: &[&str] = &[
    "-edited",
    "-bearbeitet",
    "-modifié",
    "-modificato",
    "-bewerkt",
    "-editado",
    "-redigerad",
    "-muokattu",
    "-edytowane",
    "-ha editado",
];

/// The Google Takeout import [`SourceAdapter`].
#[derive(Debug, Default, Clone, Copy)]
pub struct TakeoutAdapter;

impl TakeoutAdapter {
    /// Construct the adapter.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl SourceAdapter for TakeoutAdapter {
    fn provider(&self) -> ImportProvider {
        ImportProvider::GoogleTakeout
    }

    fn extract(&self, parts: &[PathBuf]) -> Result<ExtractedImport, AdapterError> {
        if parts.is_empty() {
            return Err(AdapterError::NoParts);
        }
        let mut walk = ExportWalk::default();
        for root in parts {
            walk.absorb_root(root)?;
        }
        Ok(walk.into_extracted())
    }
}

// ── Exporter JSON shapes ─────────────────────────────────────────────────────

/// A Google Photos per-media metadata sidecar (`photo.jpg.json`, `…supplemental-metadata.json`).
#[derive(Debug, Clone, Default, Deserialize)]
struct TakeoutSidecar {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "photoTakenTime")]
    photo_taken_time: Option<TakeoutTime>,
    #[serde(default, rename = "creationTime")]
    creation_time: Option<TakeoutTime>,
    #[serde(default, rename = "geoData")]
    geo_data: Option<GeoData>,
    #[serde(default, rename = "geoDataExif")]
    geo_data_exif: Option<GeoData>,
    #[serde(default)]
    favorited: bool,
}

/// A Takeout timestamp record — Unix seconds carried as a string.
#[derive(Debug, Clone, Default, Deserialize)]
struct TakeoutTime {
    #[serde(default)]
    timestamp: Option<String>,
}

/// A Takeout `geoData` / `geoDataExif` record. `(0, 0)` is Takeout's "no location" sentinel.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct GeoData {
    #[serde(default)]
    latitude: f64,
    #[serde(default)]
    longitude: f64,
}

/// A Takeout album-folder manifest (`metadata.json`), distinguished from a per-file sidecar by its
/// fixed filename.
#[derive(Debug, Clone, Default, Deserialize)]
struct AlbumMetadata {
    #[serde(default)]
    title: Option<String>,
}

impl TakeoutSidecar {
    /// The exporter's taken-time (`photoTakenTime`, falling back to `creationTime`).
    fn taken_time(&self) -> Option<Timestamp> {
        self.photo_taken_time
            .as_ref()
            .or(self.creation_time.as_ref())
            .and_then(|t| t.timestamp.as_ref())
            .and_then(|s| s.parse::<i64>().ok())
            .and_then(|secs| Timestamp::from_second(secs).ok())
    }

    /// The exporter's GPS fix: the (possibly user-edited) `geoData`, else the `geoDataExif` copy.
    /// The `(0, 0)` sentinel is treated as absent.
    fn gps(&self) -> Option<GeoPoint> {
        let pick = |g: &GeoData| {
            (g.latitude != 0.0 || g.longitude != 0.0).then_some(GeoPoint {
                lat: g.latitude,
                lon: g.longitude,
            })
        };
        self.geo_data
            .as_ref()
            .and_then(pick)
            .or_else(|| self.geo_data_exif.as_ref().and_then(pick))
    }

    /// The user-typed description, trimmed and empty-normalized to `None`.
    fn description(&self) -> Option<String> {
        self.description
            .as_ref()
            .map(|d| d.trim().to_string())
            .filter(|d| !d.is_empty())
    }
}

// ── The precedence fold ──────────────────────────────────────────────────────

/// Fold a media file's embedded EXIF and its exporter sidecar into [`ExtractedMetadata`], applying
/// the precedence rule: embedded EXIF wins for capture time and GPS; the exporter is authoritative
/// for description, favorites, and album membership.
fn fold(
    exif: &ExifExtract,
    sidecar: Option<&TakeoutSidecar>,
    mut albums: Vec<String>,
) -> ExtractedMetadata {
    // Capture time: EXIF `DateTimeOriginal` (resolved to UTC) wins; else the exporter's taken-time.
    let exif_time = exif
        .date_time_original
        .and_then(|_| resolve_timezone(exif).capture_timestamp)
        .and_then(|secs| Timestamp::from_second(secs).ok());
    let (taken_time, taken_time_source) = if let Some(t) = exif_time {
        (Some(t), FoldSource::Embedded)
    } else if let Some(t) = sidecar.and_then(TakeoutSidecar::taken_time) {
        (Some(t), FoldSource::Exporter)
    } else {
        (None, FoldSource::Absent)
    };

    // GPS: embedded EXIF fix wins; else the exporter's coordinates.
    let (gps, gps_source) = match (exif.gps_lat, exif.gps_lon) {
        (Some(lat), Some(lon)) => (Some(GeoPoint { lat, lon }), FoldSource::Embedded),
        _ => match sidecar.and_then(TakeoutSidecar::gps) {
            Some(p) => (Some(p), FoldSource::Exporter),
            None => (None, FoldSource::Absent),
        },
    };

    albums.sort();
    albums.dedup();

    ExtractedMetadata {
        taken_time,
        taken_time_source,
        gps,
        gps_source,
        // Exporter-authoritative constructs (never carried in the file bytes).
        description: sidecar.and_then(TakeoutSidecar::description),
        favorite: sidecar.is_some_and(|s| s.favorited),
        albums,
    }
}

// ── Filename normalization ───────────────────────────────────────────────────

/// A normalized media identity: lowercase stem (dup counter removed), lowercase extension, and the
/// `(n)` duplicate index. Both a media file and the JSON that targets it reduce to this key, so
/// `photo(1).jpg` and `photo.jpg(1).json` pair while never matching the un-suffixed original.
#[derive(Debug, Clone, PartialEq, Eq)]
struct MediaKey {
    base: String,
    ext: String,
    dup: u32,
}

/// Split a trailing `(n)` duplicate counter from a stem: `photo(1)` → (`photo`, 1).
fn split_trailing_dup(stem: &str) -> (String, u32) {
    if let Some(open) = stem.rfind('(')
        && stem.ends_with(')')
    {
        let inner = &stem[open + 1..stem.len() - 1];
        if !inner.is_empty()
            && inner.bytes().all(|b| b.is_ascii_digit())
            && let Ok(n) = inner.parse::<u32>()
        {
            return (stem[..open].to_string(), n);
        }
    }
    (stem.to_string(), 0)
}

/// Normalize a media filename (e.g. `Photo(1).JPG`) into its [`MediaKey`].
fn media_key_from_name(name: &str) -> MediaKey {
    let lower = name.to_lowercase();
    let (stem, ext) = match lower.rfind('.') {
        Some(i) => (lower[..i].to_string(), lower[i + 1..].to_string()),
        None => (lower.clone(), String::new()),
    };
    let (base, dup) = split_trailing_dup(&stem);
    MediaKey { base, ext, dup }
}

/// Strip a (possibly truncated) `.supplemental-metadata` suffix, matched on its stable stem.
fn strip_supplemental(s: &str) -> &str {
    match s.find(".supplemental") {
        Some(idx) => &s[..idx],
        None => s,
    }
}

/// Reduce a JSON sidecar filename to the media it targets: the normalized [`MediaKey`] plus the raw
/// target string (for the truncation-prefix fallback). Returns `None` for a non-`.json` name.
fn parse_sidecar_target(json_name: &str) -> Option<(MediaKey, String)> {
    let lower = json_name.to_lowercase();
    let body = lower.strip_suffix(".json")?;
    // The dup counter, when present, sits after the media extension: `photo.jpg(1)`.
    let (body, dup) = split_trailing_dup(body);
    let target_raw = strip_supplemental(&body).to_string();
    let mut key = media_key_from_name(&target_raw);
    key.dup = dup;
    Some((key, target_raw))
}

/// Strip Google's localized `-edited` suffix from a stem, returning the original base if present.
fn strip_edited_marker(stem_lower: &str) -> Option<String> {
    EDITED_MARKERS.iter().find_map(|m| {
        stem_lower
            .strip_suffix(m)
            .filter(|base| !base.is_empty())
            .map(str::to_string)
    })
}

/// Takeout's own non-media artefacts, by lowercase file name: the files the exporter writes beside
/// a user's originals that are not originals themselves. These are the only non-JSON files of an
/// export the adapter leaves out (**every file is kept** — thumbnails § Media Is Rawshift's); an
/// extension Capsule does not recognise is never a reason to leave a file out.
const TAKEOUT_ARTEFACTS: &[&str] = &["archive_browser.html"];

/// Whether `name_lower` (a lowercase file name) is one of [`TAKEOUT_ARTEFACTS`].
fn is_takeout_artefact(name_lower: &str) -> bool {
    TAKEOUT_ARTEFACTS.contains(&name_lower)
}

fn file_name_lower(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn parent_dir_name_lower(path: &Path) -> Option<String> {
    path.parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().to_lowercase())
}

// ── The walk ─────────────────────────────────────────────────────────────────

struct MediaFile {
    path: PathBuf,
    parent: PathBuf,
    file_name: String,
    ext: String,
    key: MediaKey,
}

struct SidecarFile {
    path: PathBuf,
    parent: PathBuf,
    target_key: MediaKey,
    target_raw: String,
    title: Option<String>,
    sidecar: TakeoutSidecar,
}

/// Accumulated media, per-file sidecars, and album memberships across all export parts.
#[derive(Default)]
struct ExportWalk {
    media: Vec<MediaFile>,
    sidecars: Vec<SidecarFile>,
    /// Album folder name (lowercase) → display title.
    album_titles: BTreeMap<String, String>,
}

impl ExportWalk {
    /// Walk one export part. JSON files are the sidecar and album-manifest channel; every other
    /// file is a media file whatever its extension, except a named [`TAKEOUT_ARTEFACTS`] entry.
    /// Operating-system folder metadata ([`is_os_folder_metadata`]) is left out before either
    /// channel, as the plain scanner does. Every file left out is logged.
    fn absorb_root(&mut self, root: &Path) -> Result<(), AdapterError> {
        for entry in WalkDir::new(root) {
            let entry = entry.map_err(|e| AdapterError::Io {
                root: root.display().to_string(),
                reason: e.to_string(),
            })?;
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.into_path();
            if is_os_folder_metadata(&path) {
                tracing::debug!(
                    path = %path.display(),
                    reason = "os_folder_metadata",
                    "takeout import: left out OS folder metadata that is not an original"
                );
                continue;
            }
            let name = file_name_lower(&path);
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();

            if name == "metadata.json" {
                self.absorb_album_manifest(&path);
                continue;
            }
            if ext == "json" {
                self.absorb_sidecar(path, &name);
                continue;
            }
            if is_takeout_artefact(&name) {
                tracing::debug!(
                    path = %path.display(),
                    reason = "takeout_artefact",
                    "takeout import: left out a Takeout artefact that is not an original"
                );
                continue;
            }
            tracing::trace!(path = %path.display(), ext = %ext, "takeout import: media file");
            let key = media_key_from_name(&name);
            let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
            self.media.push(MediaFile {
                path,
                parent,
                file_name: name,
                ext,
                key,
            });
        }
        Ok(())
    }

    fn absorb_album_manifest(&mut self, path: &Path) {
        let Some(folder) = parent_dir_name_lower(path) else {
            return;
        };
        // Year buckets ("Photos from 2021") are not user albums.
        if folder.starts_with("photos from ") {
            return;
        }
        let title = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<AlbumMetadata>(&b).ok())
            .and_then(|m| m.title)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| {
                path.parent()
                    .and_then(Path::file_name)
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
        if title.to_lowercase().starts_with("photos from ") {
            return;
        }
        self.album_titles.insert(folder, title);
    }

    fn absorb_sidecar(&mut self, path: PathBuf, name: &str) {
        let Some((target_key, target_raw)) = parse_sidecar_target(name) else {
            return;
        };
        // A malformed sidecar degrades to an empty record rather than aborting the import.
        let sidecar = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice::<TakeoutSidecar>(&b).ok())
            .unwrap_or_default();
        let title = sidecar.title.as_ref().map(|t| t.to_lowercase());
        let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
        self.sidecars.push(SidecarFile {
            path,
            parent,
            target_key,
            target_raw,
            title,
            sidecar,
        });
    }

    fn into_extracted(mut self) -> ExtractedImport {
        // Deterministic processing order regardless of filesystem walk order.
        self.media.sort_by(|a, b| a.path.cmp(&b.path));

        let paired = self.pair_sidecars();
        let mut entries = self.build_entries(&paired);
        entries.sort_by(|a, b| a.primary_path().cmp(b.primary_path()));
        ExtractedImport { entries }
    }

    /// Pair each media file with at most one sidecar, in ordered passes: exact key, `title`, then a
    /// truncation-prefix fallback. Each sidecar is consumed once. Returns `paired[i]` = the sidecar
    /// index for `media[i]`.
    fn pair_sidecars(&self) -> Vec<Option<usize>> {
        let mut used = vec![false; self.sidecars.len()];
        let mut paired = vec![None; self.media.len()];
        for (mi, m) in self.media.iter().enumerate() {
            let chosen = self
                .pick(&used, m, |s| s.target_key == m.key)
                .or_else(|| {
                    self.pick(&used, m, |s| {
                        s.title.as_deref() == Some(m.file_name.as_str())
                    })
                })
                .or_else(|| self.pick(&used, m, |s| truncation_match(&s.target_raw, &m.file_name)));
            if let Some(si) = chosen {
                used[si] = true;
                paired[mi] = Some(si);
            }
        }
        paired
    }

    /// The best unconsumed sidecar matching `pred`: same-directory sidecars win, then lowest path.
    fn pick(
        &self,
        used: &[bool],
        m: &MediaFile,
        pred: impl Fn(&SidecarFile) -> bool,
    ) -> Option<usize> {
        let mut best: Option<usize> = None;
        for (i, s) in self.sidecars.iter().enumerate() {
            if used[i] || !pred(s) {
                continue;
            }
            let better = match best {
                None => true,
                Some(b) => {
                    let cur_same = s.parent == m.parent;
                    let best_same = self.sidecars[b].parent == m.parent;
                    match (cur_same, best_same) {
                        (true, false) => true,
                        (false, true) => false,
                        _ => s.path < self.sidecars[b].path,
                    }
                }
            };
            if better {
                best = Some(i);
            }
        }
        best
    }

    /// Group media into candidates (collapsing edited/original pairs) and fold each entry.
    fn build_entries(&self, paired: &[Option<usize>]) -> Vec<SourceEntry> {
        // Group by (parent, original-stem, ext, dup): an edited rendition shares its original's key.
        type GroupKey = (PathBuf, String, String, u32);
        let mut groups: BTreeMap<GroupKey, Vec<usize>> = BTreeMap::new();
        for (mi, m) in self.media.iter().enumerate() {
            let base = strip_edited_marker(&m.key.base).unwrap_or_else(|| m.key.base.clone());
            groups
                .entry((m.parent.clone(), base, m.key.ext.clone(), m.key.dup))
                .or_default()
                .push(mi);
        }

        let mut entries = Vec::new();
        for indices in groups.values() {
            let originals: Vec<usize> = indices
                .iter()
                .copied()
                .filter(|&i| strip_edited_marker(&self.media[i].key.base).is_none())
                .collect();
            let editeds: Vec<usize> = indices
                .iter()
                .copied()
                .filter(|&i| strip_edited_marker(&self.media[i].key.base).is_some())
                .collect();

            if originals.len() == 1 && !editeds.is_empty() {
                // One stacked candidate: original is primary, each edited rendition an alternate.
                entries.push(self.entry_for(originals[0], &editeds, paired));
            } else {
                for &i in indices {
                    entries.push(self.entry_for(i, &[], paired));
                }
            }
        }
        entries
    }

    /// Build a [`SourceEntry`] for the primary media `pi`, stacking `extras` as alternates and
    /// folding the primary's exporter sidecar + album membership.
    fn entry_for(&self, pi: usize, extras: &[usize], paired: &[Option<usize>]) -> SourceEntry {
        let primary = &self.media[pi];
        let sidecar = paired[pi].map(|si| &self.sidecars[si].sidecar);
        let exif = extract_exif(&primary.path).unwrap_or_default();
        let albums: Vec<String> = parent_dir_name_lower(&primary.path)
            .and_then(|folder| self.album_titles.get(&folder).cloned())
            .into_iter()
            .collect();
        let metadata = fold(&exif, sidecar, albums);

        let detected_type = if group::is_video(&primary.ext) {
            AssetType::Video
        } else {
            AssetType::Photo
        };

        if extras.is_empty() {
            let members = vec![(primary.path.clone(), MemberRole::Primary)];
            SourceEntry {
                candidate: ImportCandidate {
                    source_paths: vec![primary.path.clone()],
                    detected_type,
                    stack_type: None,
                    detection_method: None,
                    detection_key: None,
                    members,
                },
                metadata,
            }
        } else {
            let mut members = vec![(primary.path.clone(), MemberRole::Primary)];
            let mut source_paths = vec![primary.path.clone()];
            for &ei in extras {
                members.push((self.media[ei].path.clone(), MemberRole::Alternate));
                source_paths.push(self.media[ei].path.clone());
            }
            SourceEntry {
                candidate: ImportCandidate {
                    source_paths,
                    detected_type,
                    stack_type: Some(StackType::Custom),
                    detection_method: Some(DetectionMethod::FilenameStem),
                    detection_key: Some(
                        strip_edited_marker(&primary.key.base)
                            .unwrap_or_else(|| primary.key.base.clone()),
                    ),
                    members,
                },
                metadata,
            }
        }
    }
}

/// A media file and a truncated sidecar target share a long-enough prefix (either direction).
fn truncation_match(target_raw: &str, media_name: &str) -> bool {
    let shared = if media_name.starts_with(target_raw) {
        target_raw.len()
    } else if target_raw.starts_with(media_name) {
        media_name.len()
    } else {
        return false;
    };
    shared >= TRUNCATION_MIN_PREFIX
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
