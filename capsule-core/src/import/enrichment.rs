//! Folding a third-party adapter's [`ExtractedMetadata`] into the **signed sidecar** at import
//! (slice `S-B10`).
//!
//! A [source adapter](crate::import::importers) already resolves the [precedence rule] at
//! extraction — embedded EXIF wins over the exporter's records, **except** for constructs the
//! exporter is authoritative for (album membership, favorites/rating, user-typed descriptions).
//! This module is the other half: it maps that folded record onto the fields the signed
//! [`SidecarV1`](crate::sidecar::SidecarV1) actually has, and indexes it by source
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
//! decision this slice took, so a later slice can revisit them deliberately:
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
//!   So the exporter's album titles are preserved verbatim as `tags_user` entries — the only
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

use crate::domain::GpsDatum;
use crate::import::importers::{ExtractedImport, ExtractedMetadata};
use crate::lifecycle::SidecarEnrichment;
use crate::sidecar::sidecar_v1::{Gps, GpsSource};

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
mod tests {
    use std::path::PathBuf;

    use jiff::Timestamp;

    use super::*;
    use crate::domain::{MemberRole, StackType};
    use crate::import::importers::{FoldSource, GeoPoint, SourceEntry};
    use crate::import::scan::ImportCandidate;
    use crate::metadata::AssetType;

    fn folded() -> ExtractedMetadata {
        ExtractedMetadata::default()
    }

    #[test]
    fn no_exporter_record_yields_no_enrichment() {
        assert_eq!(sidecar_enrichment(&folded()), None);
    }

    #[test]
    fn taken_time_and_gps_carry_across_as_fallbacks() {
        let m = ExtractedMetadata {
            taken_time: Timestamp::from_second(1_622_505_600).ok(),
            taken_time_source: FoldSource::Exporter,
            gps: Some(GeoPoint {
                lat: 21.3,
                lon: -157.8,
            }),
            gps_source: FoldSource::Exporter,
            ..folded()
        };
        let e = sidecar_enrichment(&m).expect("a record with values enriches");
        assert_eq!(
            e.capture_time.map(Timestamp::as_second),
            Some(1_622_505_600)
        );
        let gps = e.gps.expect("gps carried");
        assert_eq!((gps.lat, gps.lon), (21.3, -157.8));
        assert_eq!(gps.source, GpsSource::Manual);
        assert_eq!(gps.datum, GpsDatum::Wgs84);
    }

    #[test]
    fn a_favorite_maps_to_the_max_star_rating() {
        let m = ExtractedMetadata {
            favorite: true,
            ..folded()
        };
        assert_eq!(
            sidecar_enrichment(&m).and_then(|e| e.rating),
            Some(FAVORITE_RATING)
        );
    }

    #[test]
    fn a_non_favorite_leaves_the_rating_unwritten() {
        let m = ExtractedMetadata {
            description: Some("has a caption but is not a favorite".into()),
            ..folded()
        };
        let e = sidecar_enrichment(&m).expect("the description enriches");
        assert_eq!(e.rating, None, "an unstarred photo writes no rating at all");
    }

    #[test]
    fn a_description_maps_to_the_caption() {
        let m = ExtractedMetadata {
            description: Some("On the beach".into()),
            ..folded()
        };
        assert_eq!(
            sidecar_enrichment(&m).and_then(|e| e.caption),
            Some("On the beach".to_string())
        );
    }

    #[test]
    fn an_oversize_description_is_bounded_on_a_char_boundary() {
        // Multi-byte characters straddling the bound: the cut must stay valid UTF-8.
        let m = ExtractedMetadata {
            description: Some("é".repeat(CAPTION_MAX_BYTES)),
            ..folded()
        };
        let caption = sidecar_enrichment(&m)
            .and_then(|e| e.caption)
            .expect("the description enriches");
        assert!(caption.len() <= CAPTION_MAX_BYTES);
        assert!(caption.len() > CAPTION_MAX_BYTES - 4, "cut at the bound");
        assert!(caption.chars().all(|c| c == 'é'), "no split character");
    }

    #[test]
    fn album_membership_maps_to_user_tags() {
        let m = ExtractedMetadata {
            albums: vec!["Alps".into(), "Vacation 2021".into()],
            ..folded()
        };
        assert_eq!(
            sidecar_enrichment(&m).map(|e| e.tags),
            Some(vec!["Alps".to_string(), "Vacation 2021".to_string()])
        );
    }

    fn stacked_entry(primary: &str, alternate: &str, metadata: ExtractedMetadata) -> SourceEntry {
        SourceEntry {
            candidate: ImportCandidate {
                source_paths: vec![PathBuf::from(primary), PathBuf::from(alternate)],
                detected_type: AssetType::Photo,
                stack_type: Some(StackType::Custom),
                detection_method: None,
                detection_key: None,
                members: vec![
                    (PathBuf::from(primary), MemberRole::Primary),
                    (PathBuf::from(alternate), MemberRole::Alternate),
                ],
            },
            metadata,
        }
    }

    #[test]
    fn the_index_resolves_every_member_of_a_stacked_entry() {
        let m = ExtractedMetadata {
            description: Some("Edited photo".into()),
            ..folded()
        };
        let extracted = ExtractedImport {
            entries: vec![stacked_entry(
                "/x/edit.jpg",
                "/x/edit-edited.jpg",
                m.clone(),
            )],
        };
        let index = SourceMetadataIndex::from_extracted(&extracted);
        assert_eq!(index.len(), 2);
        // Google's edited rendition is its own signed asset, and the exporter's record
        // describes the photograph — so both members resolve to it.
        assert_eq!(index.get(Path::new("/x/edit.jpg")), Some(&m));
        assert_eq!(index.get(Path::new("/x/edit-edited.jpg")), Some(&m));
    }

    #[test]
    fn the_index_misses_an_unknown_path() {
        let index = SourceMetadataIndex::from_extracted(&ExtractedImport::default());
        assert!(index.is_empty());
        assert_eq!(index.get(Path::new("/x/never-seen.jpg")), None);
    }
}

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
mod archive_tests {
    use std::fs;

    use jiff::Timestamp;
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    use crate::crypto::primitives::Argon2Params;
    use crate::import::execute_with_source_metadata;
    use crate::import::executor_cancellation::CancellationToken;
    use crate::import::importers::SourceAdapter;
    use crate::import::importers::takeout::TakeoutAdapter;
    use crate::import::planner::{ImportConfig, plan};
    use crate::lifecycle::{SidecarEnrichment, SignedImportOptions, Workspace};

    // ── Fixtures ────────────────────────────────────────────────────────────

    /// One 12-byte IFD entry.
    fn entry(out: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: [u8; 4]) {
        out.extend_from_slice(&tag.to_be_bytes());
        out.extend_from_slice(&kind.to_be_bytes());
        out.extend_from_slice(&count.to_be_bytes());
        out.extend_from_slice(&value);
    }

    /// A JPEG carrying nothing but a valid EXIF APP1 segment: `DateTimeOriginal` and, when
    /// `gps` is given, a fix at whole degrees north/east. Synthesized rather than committed —
    /// the archive under test is built by the test, so there is no binary fixture to keep in
    /// step with it.
    fn jpeg_with_exif(date: &[u8; 20], gps: Option<(u32, u32)>) -> Vec<u8> {
        // Every offset below is relative to the start of the TIFF header.
        let ifd0_entries: u32 = if gps.is_some() { 2 } else { 1 };
        let ifd0 = 8u32;
        let sub_ifd = ifd0 + 2 + 12 * ifd0_entries + 4;
        let gps_ifd = sub_ifd + 2 + 12 + 4; // the SubIFD holds one entry
        let values = if gps.is_some() {
            gps_ifd + 2 + 12 * 4 + 4
        } else {
            gps_ifd
        };
        let (ascii, lat_at, lon_at) = (values, values + 20, values + 44);

        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM"); // big-endian
        tiff.extend_from_slice(&0x002Au16.to_be_bytes());
        tiff.extend_from_slice(&ifd0.to_be_bytes());

        // IFD0: the Exif-SubIFD pointer, plus the GPS-IFD pointer when there is a fix.
        tiff.extend_from_slice(
            &u16::try_from(ifd0_entries)
                .expect("two entries")
                .to_be_bytes(),
        );
        entry(&mut tiff, 0x8769, 4, 1, sub_ifd.to_be_bytes()); // ExifIFDPointer (LONG)
        if gps.is_some() {
            entry(&mut tiff, 0x8825, 4, 1, gps_ifd.to_be_bytes()); // GPSInfoIFDPointer
        }
        tiff.extend_from_slice(&0u32.to_be_bytes()); // no next IFD

        // Exif SubIFD: DateTimeOriginal only.
        tiff.extend_from_slice(&1u16.to_be_bytes());
        entry(&mut tiff, 0x9003, 2, 20, ascii.to_be_bytes()); // ASCII, out of line
        tiff.extend_from_slice(&0u32.to_be_bytes());

        // GPS IFD: refs inline (they fit in the 4-byte value field), coordinates out of line.
        if gps.is_some() {
            tiff.extend_from_slice(&4u16.to_be_bytes());
            entry(&mut tiff, 0x0001, 2, 2, *b"N\0\0\0"); // GPSLatitudeRef
            entry(&mut tiff, 0x0002, 5, 3, lat_at.to_be_bytes()); // GPSLatitude (RATIONAL×3)
            entry(&mut tiff, 0x0003, 2, 2, *b"E\0\0\0"); // GPSLongitudeRef
            entry(&mut tiff, 0x0004, 5, 3, lon_at.to_be_bytes()); // GPSLongitude
            tiff.extend_from_slice(&0u32.to_be_bytes());
        }

        // Value area, in the order the offsets above name.
        tiff.extend_from_slice(date);
        if let Some((lat_deg, lon_deg)) = gps {
            for deg in [lat_deg, lon_deg] {
                for numerator in [deg, 0, 0] {
                    tiff.extend_from_slice(&numerator.to_be_bytes());
                    tiff.extend_from_slice(&1u32.to_be_bytes());
                }
            }
        }

        let mut app1 = Vec::from(*b"Exif\0\0");
        app1.extend_from_slice(&tiff);
        let mut jpeg = vec![0xFF, 0xD8]; // SOI
        jpeg.extend_from_slice(&[0xFF, 0xE1]);
        let len = u16::try_from(app1.len() + 2).expect("fixture segment fits in a u16");
        jpeg.extend_from_slice(&len.to_be_bytes());
        jpeg.extend_from_slice(&app1);
        jpeg.extend_from_slice(&[0xFF, 0xD9]); // EOI
        jpeg
    }

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture directory");
        }
        fs::write(path, bytes).expect("fixture file");
    }

    /// A workspace with fast Argon2 params and its default album created.
    fn signed_workspace(dir: &Path) -> Workspace {
        let mut ws = Workspace::create_with_params(
            dir,
            b"passphrase",
            Argon2Params {
                mem_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
        )
        .expect("workspace");
        let default = ws.default_album_id();
        ws.create_album_with_id(default, "Imports").expect("album");
        ws
    }

    /// The EXIF-bearing member: EXIF says 2021-06-01T12:00:00Z at 10°N 20°E, its JSON sidecar
    /// says 2001-09-09 at 40°N 70°W — and carries the three exporter-authoritative constructs.
    fn contested_jpeg() -> Vec<u8> {
        jpeg_with_exif(b"2021:06:01 12:00:00\0", Some((10, 20)))
    }

    const CONTESTED_JSON: &[u8] = br#"{"title":"beach.jpg","description":"On the beach","photoTakenTime":{"timestamp":"1000000000"},"geoData":{"latitude":40.0,"longitude":-70.0},"favorited":true}"#;
    const PLAIN_JSON: &[u8] = br#"{"title":"plain.jpg","description":"Snowy morning","photoTakenTime":{"timestamp":"1609502400"},"geoData":{"latitude":21.3,"longitude":-157.8},"favorited":false}"#;

    fn build_archive(root: &Path) -> (Vec<u8>, Vec<u8>) {
        let gp = root.join("Takeout/Google Photos");
        let album = gp.join("Vacation");
        let contested = contested_jpeg();
        write(
            &album.join("metadata.json"),
            br#"{"title":"Vacation 2021"}"#,
        );
        write(&album.join("beach.jpg"), &contested);
        write(&album.join("beach.jpg.json"), CONTESTED_JSON);

        // A year bucket (never an album) holding bytes with no EXIF at all.
        let plain = b"plain-bytes-with-no-exif".to_vec();
        let year = gp.join("Photos from 2021");
        write(&year.join("plain.jpg"), &plain);
        write(
            &year.join("plain.jpg.supplemental-metadata.json"),
            PLAIN_JSON,
        );
        (contested, plain)
    }

    /// Run adapter → index → plan → enriched executor over an archive.
    fn import_archive(src: &Path, ws: &mut Workspace) -> usize {
        let extracted = TakeoutAdapter::new()
            .extract(&[src.to_path_buf()])
            .expect("extraction");
        let index = SourceMetadataIndex::from_extracted(&extracted);
        let config = ImportConfig::default();
        let action_plan = plan(&extracted.to_scan_result(), ws.db(), &config).expect("plan");
        let to_import = action_plan.counts.to_import;
        execute_with_source_metadata(
            &action_plan,
            ws,
            &config,
            &index,
            |_| {},
            &CancellationToken::new(),
        )
        .expect("execution");
        to_import
    }

    fn asset_for(ws: &Workspace, bytes: &[u8]) -> Uuid {
        ws.asset_ids()
            .into_iter()
            .find(|id| ws.read_plaintext(id).is_ok_and(|p| p == bytes))
            .expect("an imported asset holding these bytes")
    }

    fn capture_secs(ws: &Workspace, id: &Uuid) -> i64 {
        ws.asset(id)
            .expect("asset")
            .sidecar
            .capture_timestamp
            .parse::<Timestamp>()
            .expect("rfc3339 capture timestamp")
            .as_second()
    }

    // ── The mapping table, at the sidecar ───────────────────────────────────

    #[test]
    fn embedded_exif_beats_the_exporter_and_the_exporter_authoritative_fields_land() {
        let src = TempDir::new().expect("src");
        let lib = TempDir::new().expect("lib");
        let (contested, _) = build_archive(src.path());
        let mut ws = signed_workspace(lib.path());
        import_archive(src.path(), &mut ws);

        let id = asset_for(&ws, &contested);
        let sidecar = &ws.asset(&id).expect("asset").sidecar;

        // Row: taken-time vs EXIF precedence — both sides present and disagreeing. Until
        // `S-B16` fixed `extract_exif` this case could not arise: the EXIF side was always
        // absent, so the exporter won by default.
        assert_eq!(
            capture_secs(&ws, &id),
            1_622_505_600 + 12 * 3600,
            "the file's own EXIF capture time must win over the exporter's"
        );

        // Row: GPS fold — the embedded fix wins, and is recorded as EXIF-sourced.
        let gps = sidecar.gps.as_ref().expect("a fix reached the sidecar");
        assert_eq!((gps.lat, gps.lon), (10.0, 20.0));
        assert_eq!(gps.source, GpsSource::Exif);

        // Row: description — exporter-authoritative, so it lands despite EXIF winning above.
        assert_eq!(
            sidecar.caption.get().map(String::as_str),
            Some("On the beach")
        );
        // Row: favorites.
        assert_eq!(sidecar.rating.get(), Some(&FAVORITE_RATING));
        // Row: album membership.
        assert!(
            sidecar.tags_user.value().contains("Vacation 2021"),
            "the exporter's album title is preserved as a user tag"
        );

        // The enriched asset is still a signed asset that verifies through the chokepoint.
        assert_eq!(
            ws.verify(&id).expect("verify"),
            crate::crypto::verify_asset::VerifyOutcome::Accept
        );

        // The signed registers are the source of truth, and the queryable index projects them
        // with no extra write: the album title is searchable and the star rating filterable, so
        // the migrated album can be reconstructed as a view over what the sidecar already says.
        assert_eq!(
            ws.db().tags_for(&id.to_string()).expect("indexed tags"),
            vec!["Vacation 2021".to_string()]
        );
        let row = ws
            .db()
            .query_timeline(0, 100)
            .expect("timeline")
            .into_iter()
            .find(|r| r.uuid == id.to_string())
            .expect("the enriched asset is in the timeline");
        assert_eq!(row.rating, i64::from(FAVORITE_RATING));
        assert_eq!(row.capture_timestamp, 1_622_505_600 + 12 * 3600);
    }

    #[test]
    fn the_exporter_fills_capture_and_gps_when_the_bytes_carry_none() {
        let src = TempDir::new().expect("src");
        let lib = TempDir::new().expect("lib");
        let (_, plain) = build_archive(src.path());
        let mut ws = signed_workspace(lib.path());
        import_archive(src.path(), &mut ws);

        let id = asset_for(&ws, &plain);
        let sidecar = &ws.asset(&id).expect("asset").sidecar;

        // Before this slice these bytes were stamped with the import clock and had no location.
        assert_eq!(capture_secs(&ws, &id), 1_609_502_400);
        let gps = sidecar
            .gps
            .as_ref()
            .expect("the exporter's fix reached the sidecar");
        assert_eq!((gps.lat, gps.lon), (21.3, -157.8));
        assert_eq!(
            gps.source,
            GpsSource::Manual,
            "a fix read from the exporter's record must not claim to be this file's EXIF"
        );
        assert_eq!(gps.datum, GpsDatum::Wgs84);

        assert_eq!(
            sidecar.caption.get().map(String::as_str),
            Some("Snowy morning")
        );
        assert_eq!(
            sidecar.rating.get(),
            None,
            "an unstarred photo leaves the rating register unwritten"
        );
        assert!(
            sidecar.tags_user.value().is_empty(),
            "a year bucket is not an album"
        );
    }

    #[test]
    fn re_running_the_enriched_import_skips_completed_work() {
        let src = TempDir::new().expect("src");
        let lib = TempDir::new().expect("lib");
        build_archive(src.path());
        let mut ws = signed_workspace(lib.path());

        let first = import_archive(src.path(), &mut ws);
        assert!(first > 0, "the first run imports the archive");
        let second = import_archive(src.path(), &mut ws);
        assert_eq!(second, 0, "the re-run imports nothing new");
        assert_eq!(ws.asset_ids().len(), first, "and adds no assets");
    }

    // ── Byte stability ──────────────────────────────────────────────────────

    /// An import that carries no exporter metadata must encode exactly as it did before this
    /// slice. The enrichment can only *add* values, so with nothing folded every field it
    /// touches stays at the default it had — proven here by comparing the canonical signing
    /// bytes of two imports of the same file, one with no enrichment at all and one with a
    /// default enrichment reaching the write path, normalized for the values that are
    /// per-import by construction (asset id, session id, and the two clocks).
    #[test]
    fn an_import_with_no_exporter_metadata_is_byte_stable() {
        let src = TempDir::new().expect("src");
        let lib = TempDir::new().expect("lib");
        let photo = src.path().join("photo.jpg");
        write(&photo, b"bytes with no exif and no exporter record");

        let mut ws = signed_workspace(lib.path());
        let album = ws.default_album_id();
        let plain = ws
            .import_asset_with(album, &photo, &SignedImportOptions::default())
            .expect("plain import")
            .asset_id;
        let defaulted = ws
            .import_asset_with(
                album,
                &photo,
                &SignedImportOptions {
                    enrichment: Some(SidecarEnrichment::default()),
                    ..Default::default()
                },
            )
            .expect("import with an empty enrichment")
            .asset_id;

        let baseline = ws.asset(&plain).expect("asset").sidecar.clone();
        let mut candidate = ws.asset(&defaulted).expect("asset").sidecar.clone();
        candidate.uuid = baseline.uuid;
        candidate.session_id = baseline.session_id;
        candidate.import_timestamp = baseline.import_timestamp.clone();
        candidate.capture_timestamp = baseline.capture_timestamp.clone();

        assert_eq!(
            candidate.signing_bytes(),
            baseline.signing_bytes(),
            "an import with nothing folded must encode byte-identically"
        );

        // And the executor does not even reach that path: an entry with no exporter record maps
        // to no enrichment at all.
        assert_eq!(sidecar_enrichment(&ExtractedMetadata::default()), None);
    }
}
