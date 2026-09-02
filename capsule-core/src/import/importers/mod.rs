//! Third-party **source adapters** (slice `S-B6`; the shared seam `S-B7`/`S-B8`/`S-B9` implement).
//!
//! Migration from another photo service is an import, not a new pipeline: a [`SourceAdapter`]
//! yields `(bytes-source, extracted-metadata)` entries into the same [Scan & Extract] stage the
//! filesystem [`scanner`](crate::import::scanner) feeds, and the pure
//! [`planner`](crate::import::planner) and [`executor`](crate::import::executor) are **unchanged**.
//!
//! What an adapter owns is *structure awareness*: metadata the exporting service carries
//! out-of-band (an accompanying JSON/CSV, an album manifest) is folded into
//! [`ExtractedMetadata`] **before** planning, under a fixed precedence — embedded EXIF wins over
//! exporter-side records, **except** for constructs the exporter is authoritative for (album
//! membership, favorites/rating, and user-typed descriptions the file bytes never carried). The
//! fold happens at extraction, so the planner's determinism contract holds: a given export yields
//! the same [`ScanResult`] on every run.
//!
//! The folded record is not planner input only. The
//! [executor](crate::import::execute_with_source_metadata) writes it into the **signed
//! sidecar** at import through [`enrichment`](crate::import::enrichment) (`S-B10`) — capture time
//! and GPS as fallbacks behind the file's own EXIF, and the exporter-authoritative constructs
//! (description, favorites, album membership) unconditionally — so a migrated library keeps what
//! the exporting service carried instead of discarding it once the plan is built.
//!
//! The committed adapter is [`takeout::TakeoutAdapter`] (Google Takeout, `S-B6`); the iCloud
//! (`S-B7`), Immich (`S-B8`), and tethered-camera (`S-B9`) adapters are post-v1 and land on this
//! same seam.
//!
//! [Scan & Extract]: https://docs/design/import/pipeline/#scan--extract

pub(crate) mod takeout;

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use thiserror::Error;

use crate::import::scan::{ImportCandidate, ScanResult};

/// The exporting service an adapter migrates from (for logs / telemetry / user-facing labels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportProvider {
    /// Google Photos via Google Takeout (`S-B6`).
    GoogleTakeout,
    /// iCloud Photos export (`S-B7`, post-v1).
    ICloud,
    /// Immich export/API (`S-B8`, post-v1).
    Immich,
    /// Tethered camera over PTP/IP (`S-B9`, post-v1).
    Camera,
}

/// Errors surfaced while walking an export. Malformed *per-file* exporter metadata is tolerated
/// (the entry degrades to no exporter record); these errors are reserved for failures that abort
/// the whole extraction (no parts supplied, an unreadable root).
#[derive(Debug, Error)]
pub enum AdapterError {
    /// No export parts were supplied.
    #[error("no import parts supplied")]
    NoParts,
    /// A supplied export root could not be walked.
    #[error("io reading export root {root}: {reason}")]
    Io {
        /// The offending root path.
        root: String,
        /// The underlying error text.
        reason: String,
    },
}

/// Which side a folded field was ultimately taken from, per the precedence rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FoldSource {
    /// From the file's own embedded metadata (EXIF) — the authoritative source for capture time
    /// and GPS when present.
    Embedded,
    /// From the exporting service's out-of-band record (JSON sidecar, CSV, album manifest).
    Exporter,
    /// Neither side carried the field.
    Absent,
}

/// A WGS-84 point folded from either embedded EXIF or the exporter's record.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeoPoint {
    /// Latitude (WGS-84).
    pub lat: f64,
    /// Longitude (WGS-84).
    pub lon: f64,
}

/// The out-of-band metadata an adapter folds for one media entry, with the precedence already
/// resolved. This is the artifact the [Takeout mapping table] validates, one field per rule, and
/// what [`sidecar_enrichment`](crate::import::sidecar_enrichment) maps onto the
/// signed sidecar's fields at import.
///
/// [Takeout mapping table]: https://docs/design/import/pipeline/#validation
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedMetadata {
    /// Capture time: embedded EXIF `DateTimeOriginal` if present, else the exporter's taken-time.
    pub taken_time: Option<Timestamp>,
    /// Which side [`taken_time`](Self::taken_time) came from.
    pub taken_time_source: FoldSource,
    /// GPS fix: embedded EXIF GPS if present, else the exporter's coordinates.
    pub gps: Option<GeoPoint>,
    /// Which side [`gps`](Self::gps) came from.
    pub gps_source: FoldSource,
    /// User-typed description — **exporter-authoritative** (the file bytes never carried it).
    pub description: Option<String>,
    /// Favorite/starred flag — **exporter-authoritative**.
    pub favorite: bool,
    /// Album memberships this entry belongs to — **exporter-authoritative**. Deterministically
    /// ordered (sorted, de-duplicated).
    pub albums: Vec<String>,
}

impl Default for ExtractedMetadata {
    fn default() -> Self {
        Self {
            taken_time: None,
            taken_time_source: FoldSource::Absent,
            gps: None,
            gps_source: FoldSource::Absent,
            description: None,
            favorite: false,
            albums: Vec::new(),
        }
    }
}

/// One normalized import entry: the [`ImportCandidate`] the pure planner consumes plus the folded
/// exporter [`ExtractedMetadata`] for its primary media file.
#[derive(Debug, Clone)]
pub struct SourceEntry {
    /// The candidate fed verbatim to [`plan`](crate::import::plan).
    pub candidate: ImportCandidate,
    /// The folded out-of-band metadata for this entry's primary media file.
    pub metadata: ExtractedMetadata,
}

impl SourceEntry {
    /// The primary media path of this entry (the key callers look metadata up by).
    pub fn primary_path(&self) -> &PathBuf {
        self.candidate.primary_path()
    }
}

/// The result of walking an export: normalized entries in a deterministic order.
#[derive(Debug, Default, Clone)]
pub struct ExtractedImport {
    /// Entries, ordered deterministically (sorted by primary path) so a given export yields the
    /// same [`ScanResult`] on every run.
    pub entries: Vec<SourceEntry>,
}

impl ExtractedImport {
    /// Build the [`ScanResult`] the pure [planner](crate::import::plan) consumes. This is the
    /// only handoff into the pipeline — the adapter **feeds** the planner and never modifies it.
    pub fn to_scan_result(&self) -> ScanResult {
        ScanResult {
            candidates: self.entries.iter().map(|e| e.candidate.clone()).collect(),
        }
    }

    /// The folded exporter metadata for a media file, looked up by its primary path.
    pub fn metadata_for(&self, primary: &Path) -> Option<&ExtractedMetadata> {
        self.entries
            .iter()
            .find(|e| e.primary_path().as_path() == primary)
            .map(|e| &e.metadata)
    }
}

/// A third-party import source adapter — the seam every provider importer implements.
///
/// An implementation normalizes an exporting service's on-disk layout into `(bytes-source,
/// extracted-metadata)` [entries](SourceEntry), folding out-of-band metadata under the precedence
/// rule **before** the planner sees it. Implementations MUST be deterministic: the same `parts`
/// (in any order) yield the same [`ExtractedImport`] every run.
pub trait SourceAdapter {
    /// The service this adapter migrates from.
    fn provider(&self) -> ImportProvider;

    /// Walk the export `parts` (one or more roots — a single export directory, or the several
    /// extracted parts of a split archive) and produce the normalized entries. Split-archive
    /// parts are merged: a media file in one part pairs with its exporter sidecar in another.
    fn extract(&self, parts: &[PathBuf]) -> Result<ExtractedImport, AdapterError>;
}
