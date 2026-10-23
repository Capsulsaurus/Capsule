use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::verify_asset::VerifyOutcome;

use super::*;

const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// The instant `exif_jpeg` carries: 2019-03-04 05:06:07 at +00:00.
const EXIF_SECS: i64 = 1_551_675_967;

/// A JPEG container holding one EXIF APP1 segment with `DateTimeOriginal` and, when
/// `with_offset`, `OffsetTimeOriginal` +00:00 — the pair the importer resolves to an
/// instant. Without the offset the time is floating, which the importer (and so this
/// pass) does not resolve.
fn exif_jpeg(with_offset: bool, salt: &[u8]) -> Vec<u8> {
    const DTO: &[u8] = b"2019:03:04 05:06:07\0";
    const OTO: &[u8] = b"+00:00\0";
    let entries: u32 = if with_offset { 2 } else { 1 };
    let ifd0_at: u32 = 8;
    let exif_ifd_at = ifd0_at + 2 + 12 + 4;
    let data_at = exif_ifd_at + 2 + entries * 12 + 4;
    let dto_at = data_at;
    let oto_at = dto_at + DTO.len() as u32;

    fn entry(tiff: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: [u8; 4]) {
        tiff.extend_from_slice(&tag.to_be_bytes());
        tiff.extend_from_slice(&kind.to_be_bytes());
        tiff.extend_from_slice(&count.to_be_bytes());
        tiff.extend_from_slice(&value);
    }

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM");
    tiff.extend_from_slice(&42u16.to_be_bytes());
    tiff.extend_from_slice(&ifd0_at.to_be_bytes());
    tiff.extend_from_slice(&1u16.to_be_bytes());
    entry(&mut tiff, 0x8769, 4, 1, exif_ifd_at.to_be_bytes());
    tiff.extend_from_slice(&0u32.to_be_bytes());
    tiff.extend_from_slice(&(entries as u16).to_be_bytes());
    entry(&mut tiff, 0x9003, 2, DTO.len() as u32, dto_at.to_be_bytes());
    if with_offset {
        entry(&mut tiff, 0x9011, 2, OTO.len() as u32, oto_at.to_be_bytes());
    }
    tiff.extend_from_slice(&0u32.to_be_bytes());
    assert_eq!(tiff.len() as u32, data_at);
    tiff.extend_from_slice(DTO);
    if with_offset {
        tiff.extend_from_slice(OTO);
    }

    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xE1];
    jpeg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    jpeg.extend_from_slice(&app1);
    jpeg.extend_from_slice(&[0xFF, 0xFE]);
    jpeg.extend_from_slice(&((salt.len() + 2) as u16).to_be_bytes());
    jpeg.extend_from_slice(salt);
    jpeg.extend_from_slice(&[0xFF, 0xD9]);
    jpeg
}

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("capsule-cli-repair-{}", nanoid::nanoid!()));
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn file(&self, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, bytes).expect("fixture file");
        path
    }

    fn workspace(&self) -> Workspace {
        let lib = self.0.join("lib");
        std::fs::create_dir_all(&lib).expect("library dir");
        let mut ws =
            Workspace::create_with_params(&lib, b"pw", FAST_KDF).expect("create workspace");
        let album = ws.default_album_id();
        ws.create_album_with_id(album, "Imports")
            .expect("create album");
        ws
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ts(secs: i64) -> Timestamp {
    Timestamp::from_second(secs).expect("in range")
}

// ── detect_one ───────────────────────────────────────────────────────────

#[test]
fn an_agreeing_timestamp_is_not_affected() {
    let scratch = Scratch::new();
    let original = scratch.file("a.jpg", &exif_jpeg(true, b"a"));
    assert_eq!(
        detect_one(&ts(EXIF_SECS).to_string(), &original),
        Verdict::Agrees
    );
}

#[test]
fn a_recorded_import_time_is_affected_with_the_exif_instant_recovered() {
    let scratch = Scratch::new();
    let original = scratch.file("a.jpg", &exif_jpeg(true, b"a"));
    let now = ts(Timestamp::now().as_second());
    assert_eq!(
        detect_one(&now.to_string(), &original),
        Verdict::Affected {
            recorded: Some(now),
            recovered: ts(EXIF_SECS),
        }
    );
    // An unparseable record is affected too, with nothing to compute a delta from.
    assert_eq!(
        detect_one("not a timestamp", &original),
        Verdict::Affected {
            recorded: None,
            recovered: ts(EXIF_SECS),
        }
    );
}

/// The importer's own rule: a floating `DateTimeOriginal` resolves to no instant, so the
/// pass has nothing to compare and must not guess UTC.
#[test]
fn a_floating_exif_time_or_no_exif_is_no_instant_not_affected() {
    let scratch = Scratch::new();
    let floating = scratch.file("floating.jpg", &exif_jpeg(false, b"f"));
    assert_eq!(
        detect_one(&Timestamp::now().to_string(), &floating),
        Verdict::NoInstant
    );
    let plain = scratch.file("plain.jpg", b"\xFF\xD8\xFF no exif at all");
    assert_eq!(
        detect_one(&Timestamp::now().to_string(), &plain),
        Verdict::NoInstant
    );
}

#[test]
fn a_missing_original_is_unreadable_not_no_instant() {
    let scratch = Scratch::new();
    let missing = scratch.0.join("gone.jpg");
    assert!(matches!(
        detect_one("2019-03-04T05:06:07Z", &missing),
        Verdict::Unreadable(_)
    ));
}

// ── detect / apply over a workspace ──────────────────────────────────────

/// The acceptance case, in-process: a library whose sidecars carry the wrong
/// instant reports every affected asset, corrects them under `--apply` as one signed
/// revision each, and a second pass finds nothing — while a post-fix import is
/// untouched from the start.
#[test]
fn detect_and_apply_correct_only_the_assets_that_disagree_with_their_exif() {
    let scratch = Scratch::new();
    let mut ws = scratch.workspace();
    let album = ws.default_album_id();
    let good = ws
        .import_asset(album, &scratch.file("good.jpg", &exif_jpeg(true, b"good")))
        .expect("import");
    let broken = ws
        .import_asset(
            album,
            &scratch.file("broken.jpg", &exif_jpeg(true, b"broken")),
        )
        .expect("import");
    let floating = ws
        .import_asset(
            album,
            &scratch.file("floating.jpg", &exif_jpeg(false, b"floating")),
        )
        .expect("import");

    // A post-fix import already carries the EXIF instant: a no-op by construction.
    assert_eq!(
        ws.asset(&good).expect("asset").sidecar.capture_timestamp,
        ts(EXIF_SECS).to_string()
    );
    let clean = detect(&ws);
    assert_eq!(clean.scanned, 3);
    assert!(clean.affected.is_empty(), "{clean:?}");
    assert_eq!((clean.agrees, clean.no_instant), (2, 1));

    // Reproduce the pre-fix state on one asset: its as-imported value says "now".
    let wrong = Timestamp::now();
    ws.correct_imported_capture_timestamp(&broken, wrong)
        .expect("stamp");

    let detection = detect(&ws);
    assert_eq!(detection.affected.len(), 1);
    let item = &detection.affected[0];
    assert_eq!(item.asset_id, broken);
    assert_eq!(item.recovered, ts(EXIF_SECS));
    assert_eq!(item.recorded_text, wrong.to_string());
    assert_eq!(
        item.delta_seconds(),
        Some(EXIF_SECS - wrong.as_second()),
        "the delta is recovered minus recorded"
    );

    // A dry run writes nothing.
    let dry = run(&mut ws, RepairRequest::default()).expect("dry run");
    assert!(!dry.applied);
    assert!(dry.corrected.is_empty());
    assert_eq!(ws.asset(&broken).expect("asset").chain.records().len(), 2);

    // `--apply` corrects it as a third signed record, and only it.
    let applied = run(
        &mut ws,
        RepairRequest {
            apply: true,
            limit: None,
        },
    )
    .expect("apply");
    assert_eq!(applied.corrected, vec![broken]);
    assert!(!applied.limit_reached);
    let fixed = ws.asset(&broken).expect("asset");
    assert_eq!(fixed.sidecar.capture_timestamp, ts(EXIF_SECS).to_string());
    assert_eq!(fixed.chain.records().len(), 3);
    assert_eq!(ws.verify(&broken).expect("verify"), VerifyOutcome::Accept);
    assert_eq!(ws.asset(&good).expect("asset").chain.records().len(), 1);
    assert_eq!(ws.asset(&floating).expect("asset").chain.records().len(), 1);

    // Idempotent: a second pass has nothing left to do.
    let again = detect(&ws);
    assert!(again.affected.is_empty(), "{again:?}");
    assert_eq!((again.agrees, again.no_instant), (2, 1));
}

/// A capture time the user set is not a parser fault: the pass compares the as-imported
/// value only, so a deliberate correction is neither reported nor overwritten — and when
/// the as-imported value *is* broken, repairing it leaves the user's correction effective.
#[test]
fn a_user_correction_is_neither_reported_nor_displaced() {
    let scratch = Scratch::new();
    let mut ws = scratch.workspace();
    let album = ws.default_album_id();
    let id = ws
        .import_asset(album, &scratch.file("u.jpg", &exif_jpeg(true, b"user")))
        .expect("import");
    let chosen = ts(1_234_567_890);
    ws.set_capture_timestamp(&id, chosen).expect("user edit");

    let detection = detect(&ws);
    assert!(detection.affected.is_empty(), "{detection:?}");
    assert_eq!(detection.agrees, 1);

    // The as-imported value goes wrong underneath the correction; the repair fixes only it.
    ws.correct_imported_capture_timestamp(&id, Timestamp::now())
        .expect("stamp");
    let summary = run(
        &mut ws,
        RepairRequest {
            apply: true,
            limit: None,
        },
    )
    .expect("apply");
    assert_eq!(summary.corrected, vec![id]);
    let sidecar = &ws.asset(&id).expect("asset").sidecar;
    assert_eq!(sidecar.capture_timestamp, ts(EXIF_SECS).to_string());
    assert_eq!(sidecar.effective_capture_timestamp(), chosen.to_string());
}

#[test]
fn a_limit_stops_after_that_many_corrections_and_says_so() {
    let scratch = Scratch::new();
    let mut ws = scratch.workspace();
    let album = ws.default_album_id();
    let ids: Vec<Uuid> = (0..3)
        .map(|n| {
            let file = scratch.file(
                &format!("p{n}.jpg"),
                &exif_jpeg(true, format!("asset {n}").as_bytes()),
            );
            ws.import_asset(album, &file).expect("import")
        })
        .collect();
    for id in &ids {
        ws.correct_imported_capture_timestamp(id, Timestamp::now())
            .expect("stamp");
    }

    let first = run(
        &mut ws,
        RepairRequest {
            apply: true,
            limit: Some(2),
        },
    )
    .expect("apply");
    assert_eq!(first.corrected.len(), 2);
    assert!(first.limit_reached);
    assert_eq!(
        first.detection.affected.len(),
        3,
        "detection is not limited"
    );

    let second = run(
        &mut ws,
        RepairRequest {
            apply: true,
            limit: Some(2),
        },
    )
    .expect("apply");
    assert_eq!(second.corrected.len(), 1, "the one the limit left over");
    assert!(!second.limit_reached);
    assert!(detect(&ws).affected.is_empty());
}

/// A trashed asset is neither corrected nor counted as affected, whatever its sidecar
/// says; it is its own category, and a restore brings it back into the next run.
#[test]
fn a_trashed_asset_is_skipped_and_counted_not_corrected() {
    let scratch = Scratch::new();
    let mut ws = scratch.workspace();
    let album = ws.default_album_id();
    let trashed = ws
        .import_asset(album, &scratch.file("t.jpg", &exif_jpeg(true, b"trashed")))
        .expect("import");
    ws.correct_imported_capture_timestamp(&trashed, Timestamp::now())
        .expect("stamp");
    ws.soft_delete(&trashed, 30).expect("trash");
    let records = ws.asset(&trashed).expect("asset").chain.records().len();

    let summary = run(
        &mut ws,
        RepairRequest {
            apply: true,
            limit: None,
        },
    )
    .expect("apply");
    assert!(summary.detection.affected.is_empty(), "{summary:?}");
    assert_eq!(summary.detection.trashed, vec![trashed]);
    assert_eq!(summary.detection.scanned, 1);
    assert!(summary.corrected.is_empty());
    assert_eq!(
        ws.asset(&trashed).expect("asset").chain.records().len(),
        records,
        "nothing was appended to a trashed asset's chain"
    );

    ws.restore(&trashed).expect("restore");
    let detection = detect(&ws);
    assert_eq!(
        detection.affected.len(),
        1,
        "restored, it is affected again"
    );
    assert!(detection.trashed.is_empty());
}

/// The tripwire: `apply` writes exactly `Affected::recovered` and nothing else. A
/// hand-built `Affected` naming an arbitrary instant lands as that instant, so any change
/// that made `apply` read a different field would fail here, and the fixture's EXIF
/// equality in the tests above shows the instant `detect` supplies is the EXIF one.
#[test]
fn apply_writes_exactly_the_recovered_instant() {
    let scratch = Scratch::new();
    let mut ws = scratch.workspace();
    let album = ws.default_album_id();
    let id = ws
        .import_asset(album, &scratch.file("a.jpg", &exif_jpeg(true, b"a")))
        .expect("import");
    let arbitrary = ts(1_234_567_890);
    let corrected = apply(
        &mut ws,
        &[Affected {
            asset_id: id,
            recorded_text: "irrelevant".into(),
            recorded: None,
            recovered: arbitrary,
        }],
        None,
    )
    .expect("apply");
    assert_eq!(corrected, vec![id]);
    assert_eq!(
        ws.asset(&id).expect("asset").sidecar.capture_timestamp,
        arbitrary.to_string()
    );
    // And the detect → apply path lands the EXIF instant itself, not merely "a change".
    let affected = detect(&ws).affected;
    apply(&mut ws, &affected, None).expect("apply");
    assert_eq!(
        ws.asset(&id).expect("asset").sidecar.capture_timestamp,
        ts(EXIF_SECS).to_string()
    );
}

// ── render ───────────────────────────────────────────────────────────────

fn sample_summary(apply: bool, corrected: bool) -> RepairSummary {
    let asset_id = Uuid::nil();
    RepairSummary {
        detection: Detection {
            scanned: 4,
            affected: vec![Affected {
                asset_id,
                recorded_text: "2026-09-02T10:00:00Z".into(),
                recorded: Some(ts(1_788_688_800)),
                recovered: ts(EXIF_SECS),
            }],
            agrees: 2,
            no_instant: 1,
            unreadable: vec![],
            trashed: vec![],
        },
        applied: apply,
        corrected: if corrected { vec![asset_id] } else { vec![] },
        limit_reached: false,
    }
}

#[test]
fn a_dry_run_page_reports_the_affected_asset_and_says_nothing_was_written() {
    let page = render(RepairRequest::default(), &sample_summary(false, false));
    assert!(page.contains("Dry run"), "{page}");
    assert!(page.contains("--apply"), "{page}");
    assert!(page.contains("2026-09-02T10:00:00Z"), "{page}");
    assert!(page.contains("2019-03-04T05:06:07Z"), "{page}");
    assert!(
        page.contains("1 affected, 0 corrected, 2 already correct, 1 without"),
        "{page}"
    );
}

#[test]
fn an_apply_page_names_the_corrected_asset_and_the_expected_drift() {
    let request = RepairRequest {
        apply: true,
        limit: None,
    };
    let page = render(request, &sample_summary(true, true));
    assert!(!page.contains("Dry run"), "{page}");
    assert!(page.contains(&Uuid::nil().to_string()), "{page}");
    assert!(page.contains("1 affected, 1 corrected"), "{page}");
    assert!(page.contains(DRIFT_NOTICE), "{page}");
}

#[test]
fn a_clean_library_reports_nothing_to_repair() {
    let summary = RepairSummary {
        detection: Detection {
            scanned: 2,
            agrees: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let page = render(RepairRequest::default(), &summary);
    assert!(page.contains(NOTHING_TO_REPAIR), "{page}");
    assert!(!page.contains("affected,"), "{page}");
}
