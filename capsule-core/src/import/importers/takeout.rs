//! Google Takeout source adapter (slice `S-B6`).
//!
//! Walks a Google Photos [Takeout] export — one directory tree, or the several extracted parts of
//! a split archive — pairs each media file with its JSON sidecar (photo-taken time, GPS,
//! description, favorites) and its album folder, and folds the out-of-band exporter metadata into
//! [`ExtractedMetadata`] under the [precedence rule](super): embedded EXIF wins over the exporter's
//! records, **except** for album membership, favorites, and user-typed descriptions, which the
//! exporter is authoritative for.
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

use jiff::Timestamp;
use serde::Deserialize;
use walkdir::WalkDir;

use crate::domain::{DetectionMethod, MemberRole, StackType};
use crate::exif::{ExifExtract, extract_exif, resolve_timezone};
use crate::import::group;
use crate::import::importers::{
    AdapterError, ExtractedImport, ExtractedMetadata, FoldSource, GeoPoint, ImportProvider,
    SourceAdapter, SourceEntry,
};
use crate::import::scan::ImportCandidate;
use crate::metadata::AssetType;

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

fn is_media_ext(ext: &str) -> bool {
    group::is_primary(ext) || group::is_raw(ext) || group::is_video(ext)
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
            if is_media_ext(&ext) {
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
mod tests {
    use std::fs;

    use jiff::civil;
    use tempfile::TempDir;

    use super::*;

    // ── Fold precedence: one assertion per metadata-mapping rule ─────────────

    fn exif_floating(dt: &str) -> ExifExtract {
        ExifExtract {
            date_time_original: civil::DateTime::strptime("%Y:%m:%d %H:%M:%S", dt).ok(),
            ..Default::default()
        }
    }

    fn sidecar_json(json: &str) -> TakeoutSidecar {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn taken_time_prefers_embedded_exif_over_exporter() {
        // EXIF 2021-06-01 00:00:00 UTC == 1622505600; exporter says something else — EXIF wins.
        let exif = exif_floating("2021:06:01 00:00:00");
        let sc = sidecar_json(r#"{"photoTakenTime":{"timestamp":"1000000000"}}"#);
        let m = fold(&exif, Some(&sc), vec![]);
        assert_eq!(m.taken_time_source, FoldSource::Embedded);
        assert_eq!(m.taken_time.unwrap().as_second(), 1_622_505_600);
    }

    #[test]
    fn taken_time_falls_back_to_exporter_when_no_exif() {
        let exif = ExifExtract::default();
        let sc = sidecar_json(r#"{"photoTakenTime":{"timestamp":"1609502400"}}"#);
        let m = fold(&exif, Some(&sc), vec![]);
        assert_eq!(m.taken_time_source, FoldSource::Exporter);
        assert_eq!(m.taken_time.unwrap().as_second(), 1_609_502_400);
    }

    #[test]
    fn taken_time_absent_when_neither_side_has_it() {
        let m = fold(
            &ExifExtract::default(),
            Some(&TakeoutSidecar::default()),
            vec![],
        );
        assert_eq!(m.taken_time_source, FoldSource::Absent);
        assert!(m.taken_time.is_none());
    }

    #[test]
    fn gps_prefers_embedded_exif_over_exporter() {
        let exif = ExifExtract {
            gps_lat: Some(48.8584),
            gps_lon: Some(2.2945),
            ..Default::default()
        };
        let sc = sidecar_json(r#"{"geoData":{"latitude":40.0,"longitude":-70.0}}"#);
        let m = fold(&exif, Some(&sc), vec![]);
        assert_eq!(m.gps_source, FoldSource::Embedded);
        assert_eq!(
            m.gps.unwrap(),
            GeoPoint {
                lat: 48.8584,
                lon: 2.2945
            }
        );
    }

    #[test]
    fn gps_folds_from_exporter_when_no_exif() {
        let sc = sidecar_json(r#"{"geoData":{"latitude":37.7749,"longitude":-122.4194}}"#);
        let m = fold(&ExifExtract::default(), Some(&sc), vec![]);
        assert_eq!(m.gps_source, FoldSource::Exporter);
        assert_eq!(
            m.gps.unwrap(),
            GeoPoint {
                lat: 37.7749,
                lon: -122.4194
            }
        );
    }

    #[test]
    fn gps_zero_sentinel_is_absent() {
        let sc = sidecar_json(
            r#"{"geoData":{"latitude":0.0,"longitude":0.0},"geoDataExif":{"latitude":0.0,"longitude":0.0}}"#,
        );
        let m = fold(&ExifExtract::default(), Some(&sc), vec![]);
        assert_eq!(m.gps_source, FoldSource::Absent);
        assert!(m.gps.is_none());
    }

    #[test]
    fn description_is_exporter_authoritative() {
        let sc = sidecar_json(r#"{"description":"  Sunset over the bay  "}"#);
        let m = fold(&ExifExtract::default(), Some(&sc), vec![]);
        assert_eq!(m.description.as_deref(), Some("Sunset over the bay"));
    }

    #[test]
    fn empty_description_normalizes_to_none() {
        let sc = sidecar_json(r#"{"description":"   "}"#);
        let m = fold(&ExifExtract::default(), Some(&sc), vec![]);
        assert!(m.description.is_none());
    }

    #[test]
    fn favorite_flag_folds_from_exporter() {
        let fav = sidecar_json(r#"{"favorited":true}"#);
        let not = sidecar_json(r#"{"favorited":false}"#);
        assert!(fold(&ExifExtract::default(), Some(&fav), vec![]).favorite);
        assert!(!fold(&ExifExtract::default(), Some(&not), vec![]).favorite);
    }

    #[test]
    fn albums_are_sorted_and_deduped() {
        let m = fold(
            &ExifExtract::default(),
            None,
            vec!["Trip".into(), "Alps".into(), "Trip".into()],
        );
        assert_eq!(m.albums, vec!["Alps".to_string(), "Trip".to_string()]);
    }

    // ── Filename normalization ───────────────────────────────────────────────

    #[test]
    fn dup_counter_normalizes_media_and_sidecar_to_one_key() {
        let media = media_key_from_name("photo(1).jpg");
        let (json, _) = parse_sidecar_target("photo.jpg(1).json").unwrap();
        assert_eq!(media, json);
        assert_eq!(media.dup, 1);
        // The un-suffixed original must NOT collide with the duplicate's key.
        assert_ne!(media, media_key_from_name("photo.jpg"));
    }

    #[test]
    fn supplemental_metadata_suffix_targets_the_media() {
        let (key, _) = parse_sidecar_target("photo.jpg.supplemental-metadata.json").unwrap();
        assert_eq!(key, media_key_from_name("photo.jpg"));
        // Truncated supplemental tail resolves the same.
        let (trunc, _) = parse_sidecar_target("photo.jpg.supplemental-met.json").unwrap();
        assert_eq!(trunc, media_key_from_name("photo.jpg"));
    }

    #[test]
    fn edited_marker_stripped_to_base() {
        assert_eq!(
            strip_edited_marker("img_1234-edited").as_deref(),
            Some("img_1234")
        );
        assert_eq!(
            strip_edited_marker("img_1234-bearbeitet").as_deref(),
            Some("img_1234")
        );
        assert!(strip_edited_marker("img_1234").is_none());
    }

    // ── Archive-level fixtures ───────────────────────────────────────────────

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    /// Build one Takeout tree exercising every mapping-table rule and quirk.
    fn build_takeout_fixture(root: &Path) {
        let gp = root.join("Takeout/Google Photos");
        // Year bucket: a plain photo + its supplemental sidecar (no album, no GPS).
        let y = gp.join("Photos from 2021");
        write(&y.join("plain.jpg"), b"plain-bytes");
        write(
            &y.join("plain.jpg.supplemental-metadata.json"),
            br#"{"title":"plain.jpg","photoTakenTime":{"timestamp":"1609502400"},"favorited":false}"#,
        );

        // Album "Vacation": member with taken-time, GPS, description, favorite + album membership.
        let album = gp.join("Vacation");
        write(
            &album.join("metadata.json"),
            br#"{"title":"Vacation 2021"}"#,
        );
        write(&album.join("beach.jpg"), b"beach-bytes");
        write(
            &album.join("beach.jpg.json"),
            br#"{"title":"beach.jpg","description":"On the beach","photoTakenTime":{"timestamp":"1622505600"},"geoData":{"latitude":21.3,"longitude":-157.8},"favorited":true}"#,
        );
        // `(1)` duplicate: distinct asset, JSON counter after the extension.
        write(&album.join("beach(1).jpg"), b"beach-1-bytes");
        write(
            &album.join("beach.jpg(1).json"),
            br#"{"title":"beach.jpg","description":"Second beach","photoTakenTime":{"timestamp":"1622509200"}}"#,
        );

        // Edited/original pair: original + Google's edited rendition share one asset.
        write(&album.join("edit.jpg"), b"edit-original-bytes");
        write(&album.join("edit-edited.jpg"), b"edit-edited-bytes");
        write(
            &album.join("edit.jpg.json"),
            br#"{"title":"edit.jpg","description":"Edited photo","photoTakenTime":{"timestamp":"1622600000"}}"#,
        );

        // Truncated filename: sidecar name truncated, no `title` — only the prefix fallback pairs it.
        write(
            &y.join("this_is_a_very_long_photo_name_1234567.jpg"),
            b"long-bytes",
        );
        write(
            &y.join("this_is_a_very_long_photo_name_12345.json"),
            br#"{"description":"Truncated sidecar","photoTakenTime":{"timestamp":"1622700000"}}"#,
        );
    }

    fn meta<'a>(ex: &'a ExtractedImport, root: &Path, rel: &str) -> &'a ExtractedMetadata {
        // The fixture files are keyed by absolute primary path; find by suffix.
        ex.entries
            .iter()
            .find(|e| e.primary_path().ends_with(rel))
            .map(|e| &e.metadata)
            .unwrap_or_else(|| panic!("no entry for {rel} under {}", root.display()))
    }

    #[test]
    fn takeout_mapping_table() {
        let tmp = TempDir::new().unwrap();
        build_takeout_fixture(tmp.path());
        let ex = TakeoutAdapter::new()
            .extract(&[tmp.path().to_path_buf()])
            .unwrap();

        // Row: taken-time from exporter (no EXIF in the fixture bytes).
        let beach = meta(&ex, tmp.path(), "Vacation/beach.jpg");
        assert_eq!(beach.taken_time_source, FoldSource::Exporter);
        assert_eq!(beach.taken_time.unwrap().as_second(), 1_622_505_600);
        // Row: GPS fold.
        assert_eq!(
            beach.gps.unwrap(),
            GeoPoint {
                lat: 21.3,
                lon: -157.8
            }
        );
        assert_eq!(beach.gps_source, FoldSource::Exporter);
        // Row: description.
        assert_eq!(beach.description.as_deref(), Some("On the beach"));
        // Row: favorites.
        assert!(beach.favorite);
        // Row: album membership.
        assert_eq!(beach.albums, vec!["Vacation 2021".to_string()]);

        // Row: `(1)` duplicate pairs with its own sidecar (counter after the extension).
        let dup = meta(&ex, tmp.path(), "beach(1).jpg");
        assert_eq!(dup.description.as_deref(), Some("Second beach"));
        assert_eq!(dup.taken_time.unwrap().as_second(), 1_622_509_200);

        // Row: truncated-filename pairing (no `title`, prefix fallback only).
        let long = meta(
            &ex,
            tmp.path(),
            "this_is_a_very_long_photo_name_1234567.jpg",
        );
        assert_eq!(long.description.as_deref(), Some("Truncated sidecar"));
        assert_eq!(long.taken_time.unwrap().as_second(), 1_622_700_000);

        // Row: edited/original collapse into ONE stacked candidate.
        let edit_entry = ex
            .entries
            .iter()
            .find(|e| e.primary_path().ends_with("edit.jpg"))
            .unwrap();
        assert_eq!(edit_entry.candidate.stack_type, Some(StackType::Custom));
        assert_eq!(edit_entry.candidate.members.len(), 2);
        assert!(
            edit_entry
                .candidate
                .members
                .iter()
                .any(|(p, r)| p.ends_with("edit-edited.jpg") && *r == MemberRole::Alternate)
        );
        // The edited rendition is NOT a standalone entry.
        assert!(
            !ex.entries
                .iter()
                .any(|e| e.primary_path().ends_with("edit-edited.jpg"))
        );

        // Plain year-bucket photo has no album membership.
        let plain = meta(&ex, tmp.path(), "plain.jpg");
        assert!(plain.albums.is_empty());
        assert_eq!(plain.taken_time_source, FoldSource::Exporter);
    }

    #[test]
    fn split_archive_reunites_media_and_sidecar() {
        let part_a = TempDir::new().unwrap();
        let part_b = TempDir::new().unwrap();
        // Media lands in part A, its JSON sidecar in part B (same album folder name).
        write(
            &part_a.path().join("Takeout/Google Photos/Trip/img.jpg"),
            b"img-bytes",
        );
        write(
            &part_b
                .path()
                .join("Takeout/Google Photos/Trip/metadata.json"),
            br#"{"title":"Trip"}"#,
        );
        write(
            &part_b
                .path()
                .join("Takeout/Google Photos/Trip/img.jpg.json"),
            br#"{"description":"Split across parts","geoData":{"latitude":1.0,"longitude":2.0}}"#,
        );

        let ex = TakeoutAdapter::new()
            .extract(&[part_a.path().to_path_buf(), part_b.path().to_path_buf()])
            .unwrap();
        assert_eq!(ex.entries.len(), 1);
        let m = &ex.entries[0].metadata;
        assert_eq!(m.description.as_deref(), Some("Split across parts"));
        assert_eq!(m.gps.unwrap(), GeoPoint { lat: 1.0, lon: 2.0 });
        assert_eq!(m.albums, vec!["Trip".to_string()]);
    }

    #[test]
    fn extraction_is_deterministic_across_runs() {
        let tmp = TempDir::new().unwrap();
        build_takeout_fixture(tmp.path());
        let adapter = TakeoutAdapter::new();
        let a = adapter.extract(&[tmp.path().to_path_buf()]).unwrap();
        let b = adapter.extract(&[tmp.path().to_path_buf()]).unwrap();

        assert_eq!(a.entries.len(), b.entries.len());
        for (ea, eb) in a.entries.iter().zip(&b.entries) {
            assert_eq!(ea.primary_path(), eb.primary_path());
            assert_eq!(ea.metadata, eb.metadata);
            assert_eq!(ea.candidate.members.len(), eb.candidate.members.len());
        }
    }

    // ── Resume: the adapter feeds the unchanged planner + executor ───────────

    #[test]
    fn fixture_import_is_resumable_and_skips_completed_work() {
        use crate::crypto::primitives::Argon2Params;
        use crate::import::executor::execute;
        use crate::import::executor_cancellation::CancellationToken;
        use crate::import::planner::{ImportConfig, plan};
        use crate::lifecycle::Workspace;

        let src = TempDir::new().unwrap();
        build_takeout_fixture(src.path());
        let lib = TempDir::new().unwrap();

        let mut ws = Workspace::create_with_params(
            lib.path(),
            b"passphrase",
            Argon2Params {
                mem_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
        )
        .unwrap();
        let default = ws.default_album_id();
        ws.create_album_with_id(default, "Imports").unwrap();

        let adapter = TakeoutAdapter::new();
        let config = ImportConfig::default();
        let token = CancellationToken::new();

        // First run: extract → to_scan_result → plan → execute. Everything imports.
        let ex1 = adapter.extract(&[src.path().to_path_buf()]).unwrap();
        let scan1 = ex1.to_scan_result();
        let plan1 = plan(&scan1, ws.db(), &config).unwrap();
        let imported = plan1.counts.to_import;
        assert!(imported > 0, "first run imports the fixture");
        let summary1 = execute(&plan1, &mut ws, &config, |_| {}, &token).unwrap();
        assert!(summary1.imported_count() >= imported);

        // Re-run over the same fixture: the deterministic planner now skips completed work — the
        // adapter re-derives the same candidates and every primary hashes to an existing asset.
        let ex2 = adapter.extract(&[src.path().to_path_buf()]).unwrap();
        let plan2 = plan(&ex2.to_scan_result(), ws.db(), &config).unwrap();
        assert_eq!(plan2.counts.to_import, 0, "re-run imports nothing new");
        assert_eq!(
            plan2.counts.duplicates, imported,
            "all candidates skip as duplicates"
        );
    }
}
