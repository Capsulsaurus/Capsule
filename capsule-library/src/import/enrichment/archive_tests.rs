use std::fs;

use capsule_core::crypto::primitives::Argon2Params;
use jiff::Timestamp;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
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

    // Row: taken-time vs EXIF precedence — both sides present and disagreeing. This
    // case only arises because `extract_exif` reads real EXIF; were the EXIF side always
    // absent, the exporter would win by default.
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
        capsule_core::crypto::verify_asset::VerifyOutcome::Accept
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

    // Before this change these bytes were stamped with the import clock and had no location.
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

/// A user's capture-time and location corrections sit beside the values the import read
/// from the file: re-importing the same archive neither re-records nor discards either
/// side, and both survive a reopen from disk and an index rebuild.
#[test]
fn an_edit_then_a_re_import_keeps_both_the_imported_and_the_user_values() {
    let src = TempDir::new().expect("src");
    let lib = TempDir::new().expect("lib");
    let (contested, _) = build_archive(src.path());
    let mut ws = signed_workspace(lib.path());
    import_archive(src.path(), &mut ws);
    let id = asset_for(&ws, &contested);

    let corrected = Timestamp::from_second(1_000_000_000).expect("in range");
    ws.set_capture_timestamp(&id, corrected).expect("correct");
    ws.set_location(&id, 48.85837, 2.294_481, GpsDatum::Wgs84)
        .expect("locate");

    let check = |ws: &Workspace| {
        let sidecar = &ws.asset(&id).expect("asset").sidecar;
        // As imported: the file's own EXIF, exactly as the first import recorded it.
        assert_eq!(capture_secs(ws, &id), 1_622_505_600 + 12 * 3600);
        let imported = sidecar.gps.as_ref().expect("the EXIF fix is kept");
        assert_eq!((imported.lat, imported.lon), (10.0, 20.0));
        assert_eq!(imported.source, GpsSource::Exif);
        // The user's values are the effective ones.
        assert_eq!(sidecar.effective_capture_timestamp(), corrected.to_string());
        let manual = sidecar.effective_gps().expect("the user's fix");
        assert_eq!((manual.lat, manual.lon), (48.85837, 2.294_481));
        assert_eq!(manual.source, GpsSource::Manual);
        let row = ws
            .db()
            .find_by_uuid(&id.to_string())
            .expect("index")
            .expect("indexed");
        assert_eq!(
            row.capture_timestamp, 1_000_000_000,
            "dated by the correction"
        );
    };
    check(&ws);
    let records = ws.asset(&id).expect("asset").chain.records().len();

    assert_eq!(
        import_archive(src.path(), &mut ws),
        0,
        "a duplicate is skipped"
    );
    check(&ws);
    assert_eq!(
        ws.asset(&id).expect("asset").chain.records().len(),
        records,
        "the re-import wrote nothing to the edited asset"
    );

    drop(ws);
    let reopened = Workspace::open(
        lib.path(),
        b"passphrase",
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .expect("reopen");
    check(&reopened);
    drop(reopened);

    std::fs::remove_file(lib.path().join("index/library.sqlite")).expect("drop index");
    let library = crate::library::open_library(lib.path()).expect("library");
    crate::library::rebuild_index(&library).expect("rebuild");
    let rebuilt = library
        .db
        .find_by_uuid(&id.to_string())
        .expect("index")
        .expect("back in the rebuilt index");
    assert_eq!(rebuilt.capture_timestamp, 1_000_000_000);
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
