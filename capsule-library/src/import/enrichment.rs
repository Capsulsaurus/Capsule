//! Folding a third-party adapter's [`ExtractedMetadata`] into the **signed sidecar** at import.
//!
//! A [source adapter](crate::import::importers) already resolves the [precedence rule] at
//! extraction — embedded EXIF wins over the exporter's records, **except** for constructs the
//! exporter is authoritative for (album membership, favorites/rating, user-typed descriptions).
//! This module is the other half: it maps that folded record onto the fields the signed
//! [`SidecarV1`](capsule_core::sidecar::SidecarV1) actually has, and indexes it by source
//! path so the [executor](crate::import::executor) can attach it to the member it is importing.
//!
//! The mapping, one row per [Takeout mapping-table] rule:
//!
//! | Folded field | Sidecar destination | Rule |
//! | --- | --- | --- |
//! | `taken_time` | `capture_timestamp` | fallback — the file's own EXIF wins at the write site |
//! | `gps` | `gps` (as [`GpsSource::Manual`]) | fallback — the file's own EXIF GPS wins |
//! | `description` | `caption` LWW register | exporter-authoritative |
//! | `favorite` | `rating` LWW register = [`FAVORITE_RATING`] | exporter-authoritative |
//! | `albums` | `tags_user` OR-set | exporter-authoritative |
//!
//! Two of those mappings are **not** stated by the design docs and are recorded here as the
//! decision this change took, so a later change can revisit them deliberately:
//!
//! - **Favorites.** Capsule models no "favorite" flag. The two candidate registers are the
//!   numeric star `rating` and the trinary `cull` flag, and [Organization — Culling] keeps them
//!   deliberately orthogonal: `cull` is a *review-pass* state (`pick` means "kept in a cull"),
//!   while `rating` is the user's expressed regard for a photo. A Google Photos favorite is the
//!   latter, so it lands as the maximum star value; `cull` is left untouched (a favorite is not
//!   a culling decision, and writing one would fabricate a review the user never made).
//! - **Album membership.** An asset belongs to exactly one [container album], which the plan
//!   resolves once for the whole import; the sidecar has no per-asset album field, and
//!   [Organization — The Default Album] forbids an automated import from inventing destinations.
//!   So the exporter's album titles are preserved as `tags_user` entries, normalized by the
//!   label rule at the write site (a title the rule refuses is dropped) — the only
//!   multi-valued user-content set in the sidecar, and a [smart-album] queryable field, so a
//!   Google Photos album can be reconstructed later as a view over `tags_user contains "…"`
//!   without re-encrypting or moving anything.
//!
//! [precedence rule]: https://docs/design/import/pipeline/#third-party-importers
//! [Takeout mapping-table]: https://docs/design/import/pipeline/#validation
//! [Organization — Culling]: https://docs/design/organization/#culling
//! [container album]: https://docs/design/organization/#container-albums
//! [Organization — The Default Album]: https://docs/design/organization/#the-default-album
//! [smart-album]: https://docs/design/organization/#smart-album-definition-schema
//!
//! ## Test contract
//!
//! - `no_exporter_record_yields_no_enrichment` — a default [`ExtractedMetadata`] maps to
//!   [`None`], so the executor attaches nothing and the write path is untouched (the
//!   byte-stability floor).
//! - `taken_time_and_gps_carry_across_as_fallbacks` — folded values reach the enrichment, GPS
//!   tagged [`GpsSource::Manual`] in the WGS-84 datum Takeout publishes.
//! - `a_favorite_maps_to_the_max_star_rating` / `a_non_favorite_leaves_the_rating_unwritten`.
//! - `a_description_maps_to_the_caption` and `an_oversize_description_is_bounded_on_a_char_boundary`.
//! - `album_membership_maps_to_user_tags`.
//! - `the_index_resolves_every_member_of_a_stacked_entry` / `the_index_misses_an_unknown_path`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use capsule_core::domain::GpsDatum;
use capsule_core::sidecar::sidecar_v1::{Gps, GpsSource};

use crate::import::importers::{ExtractedImport, ExtractedMetadata};
use crate::lifecycle::SidecarEnrichment;

/// The star rating an exporter-side "favorite" maps to — the top of the 0–5 scale.
pub const FAVORITE_RATING: u8 = 5;

/// The caption bound the [metadata schema] fixes (`caption_lww.value` ≤ 4096 bytes). A longer
/// exporter description is truncated on a character boundary rather than dropped: the sidecar
/// is a copy, and the exporter's own JSON remains the untruncated record.
///
/// [metadata schema]: https://docs/design/metadata/#sidecar-schema-v1
pub const CAPTION_MAX_BYTES: usize = 4096;

/// The folded exporter metadata of a whole export, keyed by **every** source path it covers.
///
/// An [`ExtractedImport`] is keyed by each entry's *primary* path, but the executor imports one
/// member at a time (an edited rendition is its own signed asset), so the index maps every
/// member of an entry to that entry's record: the exporter describes the photograph, and each
/// member is a rendition of it.
#[derive(Debug, Default, Clone)]
pub struct SourceMetadataIndex {
    by_path: BTreeMap<PathBuf, ExtractedMetadata>,
}

impl SourceMetadataIndex {
    /// An index covering nothing — what a plain filesystem import attaches.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Index an adapter's extraction by every member path of every entry.
    #[must_use]
    pub fn from_extracted(extracted: &ExtractedImport) -> Self {
        let mut by_path = BTreeMap::new();
        for entry in &extracted.entries {
            for (path, _role) in &entry.candidate.members {
                by_path.insert(path.clone(), entry.metadata.clone());
            }
        }
        Self { by_path }
    }

    /// The folded record for a source file, if the export carried one.
    #[must_use]
    pub fn get(&self, path: &Path) -> Option<&ExtractedMetadata> {
        self.by_path.get(path)
    }

    /// How many source files the index covers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.by_path.len()
    }

    /// Whether the index covers nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_path.is_empty()
    }
}

/// Map one folded exporter record onto the sidecar fields it enriches, or [`None`] when the
/// record carries nothing to write.
///
/// The [`None`] case is load-bearing: an entry whose export supplied no metadata must reach the
/// signed write path with no enrichment at all, so its sidecar encodes byte-identically to a
/// plain filesystem import's.
#[must_use]
pub fn sidecar_enrichment(metadata: &ExtractedMetadata) -> Option<SidecarEnrichment> {
    let enrichment = SidecarEnrichment {
        capture_time: metadata.taken_time,
        // Takeout publishes WGS-84 coordinates, so the datum stays the wire-absent default. The
        // fix is *not* tagged `Exif`: it was read out of the exporter's record, not out of these
        // file bytes, and the sidecar is signed — `Manual` is the honest provenance for a
        // service-held, user-editable location.
        gps: metadata.gps.map(|p| Gps {
            lat: p.lat,
            lon: p.lon,
            source: GpsSource::Manual,
            datum: GpsDatum::Wgs84,
        }),
        caption: metadata.description.as_deref().map(bounded_caption),
        rating: metadata.favorite.then_some(FAVORITE_RATING),
        tags: metadata.albums.clone(),
    };
    (enrichment != SidecarEnrichment::default()).then_some(enrichment)
}

/// Truncate a description to [`CAPTION_MAX_BYTES`] on a character boundary.
fn bounded_caption(text: &str) -> String {
    if text.len() <= CAPTION_MAX_BYTES {
        return text.to_string();
    }
    let mut end = CAPTION_MAX_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    tracing::warn!(
        source_bytes = text.len(),
        kept_bytes = end,
        "import: exporter description exceeds the sidecar caption bound; truncated"
    );
    text[..end].to_string()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;

// ── Archive-level tests: the mapping table, at the signed sidecar ────────────
//
// The unit tests above pin the mapping; these pin the *deliverable* — that a Takeout import
// actually lands the exporter's metadata inside the signed sidecar, under the precedence rule,
// and that an import carrying no exporter record is byte-identical to a plain one.
//
// Test contract:
//
// - `embedded_exif_beats_the_exporter_and_the_exporter_authoritative_fields_land` — one archive
//   entry whose EXIF and JSON sidecar disagree on **both** capture time and GPS: EXIF wins both,
//   while the description, favorite, and album membership are written from the exporter.
// - `the_exporter_fills_capture_and_gps_when_the_bytes_carry_none` — the other arm of the rule.
// - `re_running_the_enriched_import_skips_completed_work` — determinism/resume is unchanged by
//   the enrichment path.
// - `an_import_with_no_exporter_metadata_is_byte_stable` — a default enrichment reaching the
//   write path encodes to the same canonical bytes as no enrichment at all.
#[cfg(test)]
mod archive_tests;
