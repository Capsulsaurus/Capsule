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
        .map_or_else(
            || panic!("no entry for {rel} under {}", root.display()),
            |e| &e.metadata,
        )
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

// ── Every file is kept ───────────────────────────────────────────────────

/// Build an export holding kinds outside Capsule's extension lists, each beside its sidecar
/// where one exists, plus Takeout's own `archive_browser.html`.
fn build_every_kind_fixture(root: &Path) {
    let takeout = root.join("Takeout");
    let y = takeout.join("Google Photos/Photos from 2019");
    write(&takeout.join("archive_browser.html"), b"<html></html>");
    write(&y.join("loop.gif"), b"GIF89a-bytes");
    write(
        &y.join("loop.gif.json"),
        br#"{"title":"loop.gif","description":"A gif","photoTakenTime":{"timestamp":"1546300800"}}"#,
    );
    write(&y.join("sticker.webp"), b"RIFF-webp-bytes");
    write(
        &y.join("sticker.webp.supplemental-metadata.json"),
        br#"{"title":"sticker.webp","description":"A webp","favorited":true}"#,
    );
    write(&y.join("old-phone.3gp"), b"3gp-bytes");
    write(
        &y.join("old-phone.3gp.json"),
        br#"{"title":"old-phone.3gp","geoData":{"latitude":10.0,"longitude":20.0}}"#,
    );
    // An unknown suffix with no sidecar, and a file with no extension at all.
    write(&y.join("mystery.zzq"), b"mystery-bytes");
    write(&y.join("NOEXTENSION"), b"no-extension-bytes");
    // Operating-system folder metadata left by unpacking the archive on macOS or Windows:
    // none of it is an original, so none of it may become an asset.
    write(&y.join(".DS_Store"), b"finder-bytes");
    write(&y.join("._loop.gif"), b"appledouble-bytes");
    write(&y.join("._loop.gif.json"), b"appledouble-bytes");
    write(&y.join("Thumbs.db"), b"explorer-bytes");
    write(&y.join("desktop.ini"), b"explorer-bytes");
}

#[test]
fn every_non_json_file_is_imported_whatever_its_extension() {
    let tmp = TempDir::new().unwrap();
    build_every_kind_fixture(tmp.path());
    let ex = TakeoutAdapter::new()
        .extract(&[tmp.path().to_path_buf()])
        .unwrap();

    let mut names: Vec<String> = ex
        .entries
        .iter()
        .map(|e| {
            e.primary_path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "NOEXTENSION",
            "loop.gif",
            "mystery.zzq",
            "old-phone.3gp",
            "sticker.webp"
        ],
        "every non-JSON file is an asset; no JSON, Takeout artefact or OS folder metadata is"
    );

    // Each pairs with its own sidecar where one exists.
    let gif = meta(&ex, tmp.path(), "loop.gif");
    assert_eq!(gif.description.as_deref(), Some("A gif"));
    assert_eq!(gif.taken_time.unwrap().as_second(), 1_546_300_800);
    let webp = meta(&ex, tmp.path(), "sticker.webp");
    assert_eq!(webp.description.as_deref(), Some("A webp"));
    assert!(webp.favorite);
    let three_gp = meta(&ex, tmp.path(), "old-phone.3gp");
    assert_eq!(
        three_gp.gps.unwrap(),
        GeoPoint {
            lat: 10.0,
            lon: 20.0
        }
    );
    let mystery = meta(&ex, tmp.path(), "mystery.zzq");
    assert_eq!(mystery.taken_time_source, FoldSource::Absent);
    assert!(mystery.description.is_none());
}

#[test]
fn every_kind_imports_through_the_planner_and_executor() {
    use capsule_core::crypto::primitives::Argon2Params;

    use crate::import::execute;
    use crate::import::executor_cancellation::CancellationToken;
    use crate::import::planner::{ImportConfig, plan};
    use crate::lifecycle::Workspace;

    let src = TempDir::new().unwrap();
    build_every_kind_fixture(src.path());
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

    let config = ImportConfig::default();
    let ex = TakeoutAdapter::new()
        .extract(&[src.path().to_path_buf()])
        .unwrap();
    let planned = plan(&ex.to_scan_result(), ws.db(), &config).unwrap();
    assert_eq!(
        planned.counts.to_import, 5,
        "each kind is planned as an asset"
    );
    let summary = execute(
        &planned,
        &mut ws,
        &config,
        |_| {},
        &CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(summary.imported_count(), 5, "each kind is imported");
}

#[test]
fn only_named_takeout_artefacts_are_left_out() {
    assert!(is_takeout_artefact("archive_browser.html"));
    assert!(!is_takeout_artefact("index.html"));
    assert!(!is_takeout_artefact("loop.gif"));
    assert!(!is_takeout_artefact("noextension"));
}

// ── Resume: the adapter feeds the unchanged planner + executor ───────────

#[test]
fn fixture_import_is_resumable_and_skips_completed_work() {
    use capsule_core::crypto::primitives::Argon2Params;

    use crate::import::execute;
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
