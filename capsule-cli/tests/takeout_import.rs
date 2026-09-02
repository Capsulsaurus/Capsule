//! Slice `S-B11` acceptance for `capsule import --provider takeout`: the [Google Photos
//! migration guide]'s import + verification checklist, run against a **synthesized** Takeout
//! archive by spawning the real binary.
//!
//! Every step spawns `CARGO_BIN_EXE_capsule` (the pattern `import_round_trip.rs` and
//! `cull_round_trip.rs` established, and for the same reason: only a process boundary proves a
//! library reopened from disk carries what the import wrote). The scratch `HOME`/`XDG_*` keep
//! every spawn out of the developer's own directories.
//!
//! ## What a synthesized archive can and cannot prove
//!
//! The slice's criterion is the guide's checklist on a **real** export. This file covers the half
//! that is a property of the *format*, which a hand-built tree reproduces exactly:
//!
//! - **Step 2 (every part extracted).** The archive is built as **two parts**, with `split.jpg`
//!   in part 1 and its JSON sidecar in part 2 — imported by naming both parts in one run.
//! - **Step 3 (the import itself)** and its reported counts.
//! - **The quirks table**: a truncated sidecar name, a `(1)` duplicate whose counter sits after
//!   the extension, an edited/original pair collapsing into one stacked candidate, and per-album
//!   `metadata.json` manifests.
//! - **The precedence rule**: a file whose own EXIF disagrees with its JSON sidecar (EXIF wins
//!   time + GPS), and a file with no EXIF at all (the exporter fills both).
//! - **"Counts" (the idempotent re-run).** Re-running the identical command reports `Nothing to
//!   import` and adds no assets — the determinism/resume half of the criterion.
//!
//! What it cannot prove is anything that depends on a real export's *content*: the magnitude
//! sanity-check against Google's own item count, the spot-hash sample of originals a user
//! retained, real camera EXIF across the long tail of phone/camera makes, real HEIC/MP4/Live
//! Photo payloads, non-ASCII and emoji filenames as Google actually encodes them, and the
//! scale-dependent behaviour of a multi-gigabyte export. Those stay for the real-archive run.
//!
//! ## Test list (the contract this file implements)
//!
//! - `a_takeout_export_imports_through_the_cli_with_its_exporter_metadata` — one run over both
//!   parts: reported counts, then every mapping-table row read back off the signed sidecars,
//!   including the split-part pairing and the two quirk-paired files.
//! - `re_running_the_same_takeout_import_skips_completed_work` — the guide's re-run step.
//! - `without_the_provider_flag_the_exporter_metadata_is_not_applied` — the flag is what turns
//!   the adapter on: the same tree imported as a plain directory keeps the bytes and loses the
//!   exporter's record, which is what makes `--provider` observable rather than cosmetic.
//! - `the_guides_metadata_sampling_step_is_executable_with_show` — slice `S-B18`: the guide's
//!   sampling step, run as written, over the hash a user computed on the source file.
//!
//! [Google Photos migration guide]: ../../capsule-docs/src/content/docs/guides/google-photos-migration.md

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::verify_asset::VerifyOutcome;
use capsule_core::lifecycle::Workspace;
use capsule_core::sidecar::GpsSource;
use jiff::Timestamp;
use uuid::Uuid;

const PASSPHRASE: &str = "takeout-import-passphrase";

/// Fast Argon2id for the fixture account. The spawned processes still run the *recorded*
/// parameters read back out of the wrapped blob, exactly as `import_round_trip.rs` explains.
const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// The exporter's `photoTakenTime` for `beach.jpg`, which its own EXIF must beat.
const BEACH_EXPORTER_TAKEN: i64 = 1_000_000_000;
/// The exporter's `photoTakenTime` for `plain.jpg`, which nothing in the bytes contests.
const PLAIN_EXPORTER_TAKEN: i64 = 1_609_502_400;
/// `beach.jpg`'s embedded `DateTimeOriginal`, as a floating civil time.
const BEACH_EXIF_CIVIL: &str = "2019-03-04T05:06:07Z";

// ── Scratch plumbing ─────────────────────────────────────────────────────────

/// A temp directory that removes itself, so the test needs no `tempfile` dev-dependency.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-cli-s-b11-{}", nanoid::nanoid!()));
        std::fs::create_dir_all(&path).expect("create scratch dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run the real binary once, with the CLI's config/data/cache directories redirected into the
/// scratch home so no spawn touches the developer's own.
fn spawn(home: &Path, args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_capsule"))
        .args(args)
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "off")
        .env("LC_ALL", "en_US.UTF-8")
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn capsule");
    child
        .stdin
        .as_mut()
        .expect("stdin piped")
        .write_all(format!("{PASSPHRASE}\n").as_bytes())
        .expect("write passphrase");
    child.wait_with_output().expect("wait for capsule")
}

/// The stdout of one successful `capsule` run.
fn capsule(home: &Path, args: &[&str]) -> String {
    let out = spawn(home, args);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "capsule {args:?} failed\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// `&str` view of a path built from `temp_dir()` + a nanoid, so it is always UTF-8.
fn path(p: &Path) -> &str {
    p.to_str().expect("scratch paths are UTF-8")
}

fn write(path: &Path, bytes: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("fixture directory");
    }
    std::fs::write(path, bytes).expect("fixture file");
}

// ── The synthesized archive ──────────────────────────────────────────────────

/// One 12-byte IFD entry.
fn ifd_entry(out: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: [u8; 4]) {
    out.extend_from_slice(&tag.to_be_bytes());
    out.extend_from_slice(&kind.to_be_bytes());
    out.extend_from_slice(&count.to_be_bytes());
    out.extend_from_slice(&value);
}

/// A JPEG carrying nothing but a valid EXIF APP1 segment: `DateTimeOriginal` and a GPS fix at
/// whole degrees north/east. Synthesized rather than committed — the archive under test is built
/// by the test, so there is no binary fixture to keep in step with it. (The same construction
/// `capsule-core`'s `import::enrichment::archive_tests` uses; the CLI links `capsule-core`
/// without the `media` feature, so nothing here decodes pixels and the assertions below land on
/// values that can only have come from parsing this segment.)
fn jpeg_with_exif(date: &[u8; 20], lat_deg: u32, lon_deg: u32) -> Vec<u8> {
    // Every offset below is relative to the start of the TIFF header.
    let ifd0 = 8u32;
    let sub_ifd = ifd0 + 2 + 12 * 2 + 4;
    let gps_ifd = sub_ifd + 2 + 12 + 4; // the SubIFD holds one entry
    let values = gps_ifd + 2 + 12 * 4 + 4;
    let (ascii, lat_at, lon_at) = (values, values + 20, values + 44);

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM"); // big-endian
    tiff.extend_from_slice(&0x002Au16.to_be_bytes());
    tiff.extend_from_slice(&ifd0.to_be_bytes());

    // IFD0: the Exif-SubIFD and GPS-IFD pointers.
    tiff.extend_from_slice(&2u16.to_be_bytes());
    ifd_entry(&mut tiff, 0x8769, 4, 1, sub_ifd.to_be_bytes());
    ifd_entry(&mut tiff, 0x8825, 4, 1, gps_ifd.to_be_bytes());
    tiff.extend_from_slice(&0u32.to_be_bytes());

    // Exif SubIFD: DateTimeOriginal only.
    tiff.extend_from_slice(&1u16.to_be_bytes());
    ifd_entry(&mut tiff, 0x9003, 2, 20, ascii.to_be_bytes());
    tiff.extend_from_slice(&0u32.to_be_bytes());

    // GPS IFD: refs inline, coordinates out of line.
    tiff.extend_from_slice(&4u16.to_be_bytes());
    ifd_entry(&mut tiff, 0x0001, 2, 2, *b"N\0\0\0");
    ifd_entry(&mut tiff, 0x0002, 5, 3, lat_at.to_be_bytes());
    ifd_entry(&mut tiff, 0x0003, 2, 2, *b"E\0\0\0");
    ifd_entry(&mut tiff, 0x0004, 5, 3, lon_at.to_be_bytes());
    tiff.extend_from_slice(&0u32.to_be_bytes());

    // Value area, in the order the offsets above name.
    tiff.extend_from_slice(date);
    for deg in [lat_deg, lon_deg] {
        for numerator in [deg, 0, 0] {
            tiff.extend_from_slice(&numerator.to_be_bytes());
            tiff.extend_from_slice(&1u32.to_be_bytes());
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

/// The media bytes of every file in the archive, keyed by the short name the assertions use.
/// Each file's bytes are distinct, so nothing is deduplicated by accident and every imported
/// asset is identifiable by its plaintext alone.
struct Media {
    beach: Vec<u8>,
    beach_dup: Vec<u8>,
    plain: Vec<u8>,
    sunset: Vec<u8>,
    sunset_edited: Vec<u8>,
    truncated: Vec<u8>,
    split: Vec<u8>,
}

/// Build a two-part Takeout export under `root`, exercising every quirk the adapter claims to
/// reconcile. Returns the media bytes so each imported asset can be identified.
///
/// Part 1 holds the tree; part 2 holds exactly one thing — `split.jpg`'s JSON sidecar, whose
/// media file lives in part 1. That is the split-archive case the guide's Step 2 insists on.
fn build_archive(root: &Path) -> Media {
    let p1 = root.join("part-001/Takeout/Google Photos");
    let p2 = root.join("part-002/Takeout/Google Photos");
    let album = p1.join("Vacation");
    let year = p1.join("Photos from 2021");

    // ── An album folder with its manifest ──────────────────────────────────
    write(
        &album.join("metadata.json"),
        br#"{"title":"Vacation 2021"}"#,
    );

    // The contested file: its own EXIF says 2019-03-04T05:06:07 at 10°N 20°E, its sidecar says
    // 2001-09-09 at 40°N 70°W — and carries the three exporter-authoritative constructs.
    let beach = jpeg_with_exif(b"2019:03:04 05:06:07\0", 10, 20);
    write(&album.join("beach.jpg"), &beach);
    write(
        &album.join("beach.jpg.json"),
        br#"{"title":"beach.jpg","description":"On the beach","photoTakenTime":{"timestamp":"1000000000"},"geoData":{"latitude":40.0,"longitude":-70.0},"favorited":true}"#,
    );

    // ── Quirk: a `(1)` duplicate, whose sidecar keeps the counter after the extension ──
    let beach_dup = b"a second file that google renamed beach(1).jpg".to_vec();
    write(&album.join("beach(1).jpg"), &beach_dup);
    write(
        &album.join("beach.jpg(1).json"),
        br#"{"title":"beach.jpg","description":"Second copy","photoTakenTime":{"timestamp":"1000000500"},"favorited":false}"#,
    );

    // ── A year bucket (never an album) holding bytes with no EXIF at all ──
    let plain = b"plain-bytes-with-no-exif".to_vec();
    write(&year.join("plain.jpg"), &plain);
    write(
        &year.join("plain.jpg.supplemental-metadata.json"),
        br#"{"title":"plain.jpg","description":"Snowy morning","photoTakenTime":{"timestamp":"1609502400"},"geoData":{"latitude":21.3,"longitude":-157.8},"favorited":false}"#,
    );

    // ── Quirk: an edited/original pair, which collapses into one stacked candidate ──
    let sunset = b"the original sunset frame".to_vec();
    let sunset_edited = b"google's edited rendition of the sunset frame".to_vec();
    write(&year.join("sunset.jpg"), &sunset);
    write(&year.join("sunset-edited.jpg"), &sunset_edited);
    write(
        &year.join("sunset.jpg.json"),
        br#"{"title":"sunset.jpg","description":"Golden hour","photoTakenTime":{"timestamp":"1620000000"},"favorited":false}"#,
    );

    // ── Quirk: a truncated sidecar name, with no `title` to fall back on, so only the
    //    shared-prefix rule can pair the two. ──
    let truncated = b"bytes of a file whose sidecar name google truncated".to_vec();
    write(
        &year.join("very-long-holiday-photo-name-002.jpg"),
        &truncated,
    );
    write(
        &year.join("very-long-holiday-photo-name-0.json"),
        br#"{"description":"Truncated sidecar","photoTakenTime":{"timestamp":"1630000000"},"favorited":false}"#,
    );

    // ── Quirk: the media file and its sidecar in *different* export parts ──
    let split = b"bytes whose json sidecar landed in another part".to_vec();
    write(&year.join("split.jpg"), &split);
    write(
        &p2.join("Photos from 2021/split.jpg.json"),
        br#"{"title":"split.jpg","description":"Reunited across parts","photoTakenTime":{"timestamp":"1234567890"},"favorited":true}"#,
    );

    Media {
        beach,
        beach_dup,
        plain,
        sunset,
        sunset_edited,
        truncated,
        split,
    }
}

// ── The fixture ──────────────────────────────────────────────────────────────

/// A scratch home, a two-part export, and a lazily-initialized library per test.
struct Fixture {
    _scratch: ScratchDir,
    home: PathBuf,
    parts: [PathBuf; 2],
    media: Media,
    next_library: std::cell::Cell<u32>,
}

impl Fixture {
    /// A fresh library created by a real `capsule library init`, with the fast-cost account
    /// seeded in-process before any spawn needs it (see `import_round_trip.rs` for why).
    fn library(&self) -> PathBuf {
        let n = self.next_library.get();
        self.next_library.set(n + 1);
        let library = self._scratch.path().join(format!("library-{n}"));
        let out = capsule(
            &self.home,
            &["library", "init", path(&library), "--name", "Takeout"],
        );
        assert!(out.contains("Library created at"), "stdout:\n{out}");
        drop(Workspace::open(&library, PASSPHRASE.as_bytes(), FAST_KDF).expect("seed the account"));
        library
    }

    /// `capsule import <part-1> <part-2> [--provider takeout] --library <library>`.
    fn import(&self, library: &Path, provider: bool) -> String {
        let mut args = vec!["import", path(&self.parts[0]), path(&self.parts[1])];
        if provider {
            args.extend_from_slice(&["--provider", "takeout"]);
        }
        args.extend_from_slice(&["--library", path(library), "--passphrase-stdin"]);
        capsule(&self.home, &args)
    }
}

fn fixture() -> Fixture {
    let scratch = ScratchDir::new();
    let home = scratch.path().join("home");
    let source = scratch.path().join("takeout-extracted");
    std::fs::create_dir_all(&home).expect("create scratch home");
    let media = build_archive(&source);
    Fixture {
        parts: [source.join("part-001"), source.join("part-002")],
        _scratch: scratch,
        home,
        media,
        next_library: std::cell::Cell::new(0),
    }
}

// ── Reading the library back ─────────────────────────────────────────────────

/// The library reopened from disk in this process, after every spawned `capsule` has exited.
fn reopen(library: &Path) -> Workspace {
    Workspace::open(library, PASSPHRASE.as_bytes(), FAST_KDF).expect("reopen the library")
}

/// The asset holding exactly these plaintext bytes.
fn asset_for(ws: &Workspace, bytes: &[u8]) -> Uuid {
    ws.asset_ids()
        .into_iter()
        .find(|id| ws.read_plaintext(id).is_ok_and(|p| p == bytes))
        .expect("an imported asset holding these bytes")
}

/// `(caption, rating, tags)` — the three exporter-authoritative registers of one asset.
fn authored(ws: &Workspace, bytes: &[u8]) -> (Option<String>, Option<u8>, Vec<String>) {
    let sidecar = &ws.asset(&asset_for(ws, bytes)).expect("asset").sidecar;
    let mut tags: Vec<String> = sidecar.tags_user.value().into_iter().collect();
    tags.sort();
    (
        sidecar.caption.get().cloned(),
        sidecar.rating.get().copied(),
        tags,
    )
}

fn capture_secs(ws: &Workspace, bytes: &[u8]) -> i64 {
    ws.asset(&asset_for(ws, bytes))
        .expect("asset")
        .sidecar
        .capture_timestamp
        .parse::<Timestamp>()
        .expect("rfc3339 capture timestamp")
        .as_second()
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// **The guide's Step 2 + Step 3 + mapping table, in one run.** Both extracted parts are named
/// on one command line; every quirk pairs; every exporter-authoritative field lands inside the
/// signed sidecar; and the precedence rule holds in both directions.
#[test]
fn a_takeout_export_imports_through_the_cli_with_its_exporter_metadata() {
    let fx = fixture();
    let library = fx.library();

    let out = fx.import(&library, true);
    // Six candidates over seven files: the edited/original pair is *one* candidate.
    assert!(
        out.contains("Found 6 candidate(s) (7 file(s) total)."),
        "the adapter must collapse the edited pair and pair every sidecar\nstdout:\n{out}"
    );
    assert!(
        out.contains("Plan: 6 to import, 0 duplicate(s) skipped, 0 unsupported or errored."),
        "stdout:\n{out}"
    );
    assert!(
        out.contains("Done: 7 imported, 0 duplicate(s), 0 error(s)."),
        "stdout:\n{out}"
    );

    let ws = reopen(&library);
    assert_eq!(ws.asset_ids().len(), 7, "one asset per media file");

    // ── Row: EXIF beats the exporter for capture time and GPS ──────────────
    let beach = asset_for(&ws, &fx.media.beach);
    let beach_sidecar = &ws.asset(&beach).expect("asset").sidecar;
    let exif_instant = BEACH_EXIF_CIVIL
        .parse::<Timestamp>()
        .expect("the fixture's EXIF instant");
    assert_eq!(
        capture_secs(&ws, &fx.media.beach),
        exif_instant.as_second(),
        "the file's own EXIF capture time must win over the exporter's"
    );
    assert_ne!(capture_secs(&ws, &fx.media.beach), BEACH_EXPORTER_TAKEN);
    let gps = beach_sidecar
        .gps
        .as_ref()
        .expect("a fix reached the sidecar");
    assert_eq!((gps.lat, gps.lon), (10.0, 20.0), "the embedded fix wins");
    assert_eq!(gps.source, GpsSource::Exif);

    // ── Rows: the three exporter-authoritative constructs, plus album membership ──
    assert_eq!(
        authored(&ws, &fx.media.beach),
        (
            Some("On the beach".to_string()),
            Some(5),
            vec!["Vacation 2021".to_string()]
        ),
        "caption, favorite→rating, and the album title as a user tag"
    );

    // Enrichment is written *inside* the signed sidecar, so the asset must still verify.
    assert_eq!(ws.verify(&beach).expect("verify"), VerifyOutcome::Accept);

    // ── Row: the exporter fills capture + GPS when the bytes carry neither ──
    assert_eq!(capture_secs(&ws, &fx.media.plain), PLAIN_EXPORTER_TAKEN);
    let plain_sidecar = &ws
        .asset(&asset_for(&ws, &fx.media.plain))
        .expect("asset")
        .sidecar;
    let plain_gps = plain_sidecar.gps.as_ref().expect("the exporter's fix");
    assert_eq!((plain_gps.lat, plain_gps.lon), (21.3, -157.8));
    assert_eq!(
        plain_gps.source,
        GpsSource::Manual,
        "a fix read from the exporter's record must not claim to be this file's EXIF"
    );
    assert_eq!(
        authored(&ws, &fx.media.plain),
        (Some("Snowy morning".to_string()), None, Vec::new()),
        "a year bucket is not an album, and an unstarred photo writes no rating"
    );

    // ── Quirk: the `(1)` duplicate is its own asset with its own sidecar ──
    assert_eq!(
        authored(&ws, &fx.media.beach_dup),
        (
            Some("Second copy".to_string()),
            None,
            vec!["Vacation 2021".to_string()]
        ),
        "the duplicate must pair with `beach.jpg(1).json`, not with `beach.jpg.json`"
    );

    // ── Quirk: the truncated sidecar pairs on its shared prefix ──
    assert_eq!(
        authored(&ws, &fx.media.truncated).0,
        Some("Truncated sidecar".to_string())
    );

    // ── Quirk: the sidecar that landed in the other export part ──
    assert_eq!(
        authored(&ws, &fx.media.split),
        (
            Some("Reunited across parts".to_string()),
            Some(5),
            Vec::new()
        ),
        "naming both parts in one run must reunite the media file with its sidecar"
    );

    // ── Quirk: the edited rendition is stacked onto its original, not orphaned ──
    let original = ws
        .asset(&asset_for(&ws, &fx.media.sunset))
        .expect("asset")
        .sidecar
        .stack_membership
        .get()
        .cloned()
        .flatten()
        .expect("the original is a stack member");
    let edited = ws
        .asset(&asset_for(&ws, &fx.media.sunset_edited))
        .expect("asset")
        .sidecar
        .stack_membership
        .get()
        .cloned()
        .flatten()
        .expect("the edited rendition is a stack member");
    assert_eq!(
        original.stack_id, edited.stack_id,
        "an edited/original pair must land as one stack"
    );
    // Both renditions inherit the photograph's exporter record.
    assert_eq!(
        authored(&ws, &fx.media.sunset_edited).0,
        Some("Golden hour".to_string())
    );
}

/// **The guide's "Counts" checklist step.** Re-running the identical command imports nothing and
/// adds no assets — the determinism/resume half of the slice's criterion, which a synthesized
/// archive proves as well as a real one does.
#[test]
fn re_running_the_same_takeout_import_skips_completed_work() {
    let fx = fixture();
    let library = fx.library();

    let first = fx.import(&library, true);
    assert!(
        first.contains("Done: 7 imported, 0 duplicate(s), 0 error(s)."),
        "stdout:\n{first}"
    );
    let after_first = reopen(&library).asset_ids().len();
    assert_eq!(after_first, 7);

    let second = fx.import(&library, true);
    assert!(
        second.contains("Nothing to import."),
        "the guide's re-run step must report nothing to do\nstdout:\n{second}"
    );
    assert_eq!(
        reopen(&library).asset_ids().len(),
        after_first,
        "a re-run must add no assets"
    );
}

/// **What `--provider` is for.** The same tree imported as a plain directory brings the bytes
/// across and nothing else: no album tag, no favorite, no caption, and a capture time that is
/// the import clock rather than the exporter's `photoTakenTime`. This is the state the migration
/// guide described before this slice.
#[test]
fn without_the_provider_flag_the_exporter_metadata_is_not_applied() {
    let fx = fixture();
    let library = fx.library();

    fx.import(&library, false);
    let ws = reopen(&library);

    for bytes in [&fx.media.beach, &fx.media.plain, &fx.media.split] {
        assert_eq!(
            authored(&ws, bytes),
            (None, None, Vec::new()),
            "a plain filesystem import must write no exporter metadata at all"
        );
    }
    assert_ne!(
        capture_secs(&ws, &fx.media.plain),
        PLAIN_EXPORTER_TAKEN,
        "a file whose time lived only in the Takeout JSON gets the import clock instead"
    );
    // The bytes themselves still come across, EXIF and all — that never depended on the flag.
    let beach = ws.asset(&asset_for(&ws, &fx.media.beach)).expect("asset");
    assert_eq!(
        beach.sidecar.gps.as_ref().map(|g| g.source),
        Some(GpsSource::Exif)
    );
}

/// Slice `S-I5`: the import arm's output is rendered from the `locales/` catalog, not from
/// literals in `capsule-cli`. The assertion is deliberately written against the catalog
/// message rather than against English text — an English-only assertion would still pass if
/// somebody re-hardcoded the string, which is exactly the regression `S-I5` exists to stop.
#[test]
fn the_import_arm_prints_catalog_messages_not_hardcoded_english() {
    let fx = fixture();
    let library = fx.library();
    let bundle = capsule_cli::i18n::cli_bundle();

    let out = fx.import(&library, true);

    // The provider notice is new in `S-I5`: a `--provider` run says which adapter read the
    // source. The product name is substituted in, never translated.
    let notice = bundle.format(
        capsule_cli::i18n::keys::IMPORT_PROVIDER_NOTICE,
        &[("provider", capsule_cli::i18n::Value::Str("Google Takeout"))],
    );
    assert!(
        notice.contains("Google Takeout"),
        "the key must interpolate the provider name, got {notice:?}"
    );
    assert!(out.contains(&notice), "stdout:\n{out}");

    for key in [
        capsule_cli::i18n::keys::IMPORT_SCANNING,
        capsule_cli::i18n::keys::IMPORT_EXECUTING,
    ] {
        let message = bundle.format(key, &[]);
        assert_ne!(message, key, "{key} is missing from the catalog");
        assert!(out.contains(&message), "{key} missing from stdout:\n{out}");
    }

    // A plain filesystem import takes the other arm, so it must NOT claim a provider.
    let plain_library = fx.library();
    let plain = fx.import(&plain_library, false);
    assert!(
        !plain.contains(&notice),
        "a plain import must not announce an export adapter\nstdout:\n{plain}"
    );
}

/// Slice `S-B18`: the migration guide's metadata-sampling step, executed as written. The user
/// holds the source file's SHA-256 from the guide's spot-hash step; `capsule show` takes a
/// prefix of it and prints the exporter-authoritative values the import folded into the
/// signed sidecar — the caption, the favourite as five stars, the album as a user tag — and
/// the file's own EXIF fix, which beat the exporter's.
#[test]
fn the_guides_metadata_sampling_step_is_executable_with_show() {
    let fx = fixture();
    let library = fx.library();
    fx.import(&library, true);

    // `shasum -a 256 beach.jpg`, as the guide instructs, and its first eight hex digits.
    let hex = capsule_core::crypto::hash::hash_bytes(&fx.media.beach).to_hex();
    let page = capsule(
        &fx.home,
        &[
            "show",
            &hex[..8],
            "--library",
            path(&library),
            "--passphrase-stdin",
        ],
    );

    for expected in [
        hex.as_str(),
        "On the beach",
        "5/5",
        "Vacation 2021",
        "10.000000, 20.000000 (WGS-84, EXIF)",
        BEACH_EXIF_CIVIL,
    ] {
        assert!(page.contains(expected), "missing {expected:?} in:\n{page}");
    }

    // The exporter-filled fix is labelled as such, so a user can tell it apart from EXIF.
    let plain_hex = capsule_core::crypto::hash::hash_bytes(&fx.media.plain).to_hex();
    let plain = capsule(
        &fx.home,
        &[
            "show",
            &plain_hex[..8],
            "--library",
            path(&library),
            "--passphrase-stdin",
        ],
    );
    assert!(
        plain.contains("21.300000, -157.800000 (WGS-84, manual)"),
        "{plain}"
    );
    assert!(plain.contains("Snowy morning"), "{plain}");
    assert!(
        plain.contains("Rating:          (unset)"),
        "an unstarred photo's rating is spelled out as unset:\n{plain}"
    );
}
