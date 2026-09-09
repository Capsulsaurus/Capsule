//! Slice `S-A10` acceptance for `capsule import`: what one process imports, a **different
//! process** reconstructs from the library directory alone.
//!
//! Every step below spawns the real `capsule` binary (`CARGO_BIN_EXE_capsule`, the path Cargo
//! hands integration tests of the crate that declares the bin). `capsule library init`,
//! `capsule import`, `capsule cull` and `capsule library rebuild` each get their own process,
//! and the only thing they share is the library directory on disk. That is the whole point:
//! `S-A10`'s criterion is that state survives a *new process*, and an in-process second
//! `Workspace::open` cannot tell durable state apart from state that merely survived in a
//! `HashMap`. A process boundary can — which is why this is a subprocess test, and why (like
//! `cull_round_trip.rs`, whose pattern it follows) it needs no test dependency to be one.
//!
//! **The read-back step is `capsule cull`, not `capsule list`.** `capsule list` takes no
//! `--library`: it reports what the *sync feed* has delivered into the CLI's own database under
//! the user's data directory, so it can say nothing about an offline import. That gap is pinned
//! by [`capsule_list_reports_the_sync_feed_not_the_library`] rather than papered over.
//!
//! **The fixture image.** A synthesized 8×8 baseline JPEG (see [`synthetic_jpeg`]) carrying a
//! real EXIF APP1 segment — no committed binary. The CLI links `capsule-core` **without** the
//! `media` feature, so nothing on this path decodes pixels (every still reports
//! `DerivativeStatus::DeferredNoCodec`, slice `S-B13`); the decode the importer genuinely
//! performs is EXIF, through `extract_exif`. The assertions therefore land on values that can
//! only have come from parsing the segment: the sidecar's 8×8 dimensions and its GPS fix. The
//! bytes are a real, decodable JPEG all the same, so the fixture stays honest if a build that
//! *does* carry a codec ever runs this path.
//!
//! **Argon2id.** `capsule library init` does not create the account — the first `capsule import`
//! does, at `DeviceTier::Normal` (256 MiB, t=3), which costs ~5 s per unlock in a debug build and
//! would dominate this test several times over. So the fixture seeds the account itself, once,
//! in-process at a fast cost. That is not a shortcut around the CLI's real work: `pwkdf` records
//! the wrap-time parameters inside the wrapped blob and `unwrap` reads them back, so every
//! spawned `capsule` below runs the full recorded unlock regardless of the tier the CLI passes
//! for a *first-time* account.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use capsule_core::crypto::hash;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::provenance::ProvenanceRecord;
use capsule_core::crypto::provenance::action::Action;
use capsule_core::db::AssetRow;
use capsule_core::library::open_library;
use capsule_core::lifecycle::Workspace;
use capsule_core::sidecar::{SIDECAR_SCHEMA_V1, SidecarV1};
use jiff::Timestamp;
use uuid::Uuid;

const PASSPHRASE: &str = "import-round-trip-passphrase";

/// Fast Argon2id for the fixture account; see the module header for why this does not weaken
/// what the spawned processes below actually run.
const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// The EXIF fix baked into [`synthetic_jpeg`]: 48°51'29.6"N, 2°17'40.2"W.
const EXIF_LAT: f64 = 48.0 + 51.0 / 60.0 + 29.6 / 3600.0;
const EXIF_LON: f64 = -(2.0 + 17.0 / 60.0 + 40.2 / 3600.0);

// ── Scratch plumbing ─────────────────────────────────────────────────────────

/// A temp directory that removes itself, so the test needs no `tempfile` dev-dependency.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-cli-s-a10-{}", nanoid::nanoid!()));
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

/// Run the real binary once. `home` redirects the CLI's config/data/cache directories, which
/// otherwise resolve under the developer's own home — no spawn here may read or write those.
fn spawn(home: &Path, args: &[&str], passphrase_stdin: bool) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_capsule"))
        .args(args)
        // Colour codes would make the stdout assertions brittle; the CLI honours NO_COLOR.
        .env("NO_COLOR", "1")
        // The binary's fmt subscriber writes to stdout and defaults to DEBUG in a debug build,
        // which would interleave log lines into the output under assertion here.
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
    if passphrase_stdin {
        child
            .stdin
            .as_mut()
            .expect("stdin piped")
            .write_all(format!("{PASSPHRASE}\n").as_bytes())
            .expect("write passphrase");
    }
    child.wait_with_output().expect("wait for capsule")
}

/// The stdout of one successful `capsule` run.
fn capsule(home: &Path, args: &[&str], passphrase_stdin: bool) -> String {
    let out = spawn(home, args, passphrase_stdin);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "capsule {args:?} failed\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

// ── The fixture ──────────────────────────────────────────────────────────────

/// A library created by a real `capsule library init`, a source directory holding one
/// synthesized JPEG, and a fast-Argon2id account seeded into the library.
struct Fixture {
    _scratch: ScratchDir,
    home: PathBuf,
    library: PathBuf,
    source: PathBuf,
    /// The de facto album `capsule import` resolves — derived from the master key, so it is a
    /// fact about the account on disk, not about any one process.
    album: Uuid,
    image: Vec<u8>,
}

impl Fixture {
    /// `capsule import <source> --library <library>`, asserting the one candidate landed.
    fn import(&self) -> String {
        let out = self.run(
            &[
                "import",
                path(&self.source),
                "--library",
                path(&self.library),
                "--passphrase-stdin",
            ],
            true,
        );
        assert!(
            out.contains("Done: 1 imported, 0 duplicate(s), 0 error(s)."),
            "stdout:\n{out}"
        );
        out
    }

    fn run(&self, args: &[&str], passphrase_stdin: bool) -> String {
        capsule(&self.home, args, passphrase_stdin)
    }
}

/// `&str` view of a path built from `temp_dir()` + a nanoid, so it is always UTF-8.
fn path(p: &Path) -> &str {
    p.to_str().expect("scratch paths are UTF-8")
}

/// **Process 1** — `capsule library init` — plus the fast-cost account the module header explains.
fn fixture() -> Fixture {
    let scratch = ScratchDir::new();
    let home = scratch.path().join("home");
    let library = scratch.path().join("library");
    let source = scratch.path().join("source");
    std::fs::create_dir_all(&home).expect("create scratch home");
    std::fs::create_dir_all(&source).expect("create source dir");
    let image = synthetic_jpeg();
    std::fs::write(source.join("photo.jpg"), &image).expect("write fixture image");

    let out = capsule(
        &home,
        &["library", "init", path(&library), "--name", "Round trip"],
        false,
    );
    assert!(out.contains("Library created at"), "stdout:\n{out}");

    // Seeding the account is the one in-process step, and it is deliberately *before* the first
    // spawn that needs it: `Workspace::open` on an account-less library mints the account at the
    // parameters it is handed. Dropping the workspace releases the library lock the spawned
    // processes need.
    let album = {
        let ws = Workspace::open(&library, PASSPHRASE.as_bytes(), FAST_KDF)
            .expect("seed the fixture account");
        ws.default_album_id()
    };

    Fixture {
        _scratch: scratch,
        home,
        library,
        source,
        album,
        image,
    }
}

// ── Reading the library back from disk ───────────────────────────────────────

/// The `media/{YYYY}/{YYYY-MM}` bucket and asset id of the library's single asset, found by its
/// provenance chain — the per-asset anchor `Workspace::open` and `rebuild_index` both key off.
fn sole_asset_on_disk(library: &Path) -> (PathBuf, Uuid) {
    let mut found: Vec<(PathBuf, Uuid)> = Vec::new();
    let mut stack = vec![library.join("media")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(stem) = name.strip_suffix(".provenance.cbor") else {
                continue;
            };
            let id = Uuid::parse_str(stem).expect("provenance file is named by its asset id");
            found.push((dir.clone(), id));
        }
    }
    assert_eq!(
        found.len(),
        1,
        "expected exactly one asset, found {found:?}"
    );
    found.remove(0)
}

/// The index row for `asset_id`, read by opening the library's own SQLite index from disk.
/// The `Library` guard is dropped before returning, releasing the library lock.
fn index_row(library: &Path, asset_id: &Uuid) -> Option<AssetRow> {
    let lib = open_library(library).expect("open the library index");
    lib.db
        .find_by_uuid(&asset_id.to_string())
        .expect("query the library index")
}

/// Delete the SQLite index — the exact disaster `capsule library rebuild` exists for.
fn destroy_index(library: &Path) {
    let db = library.join("index").join("library.sqlite");
    for suffix in ["", "-wal", "-shm"] {
        let mut path = db.clone().into_os_string();
        path.push(suffix);
        let _ = std::fs::remove_file(PathBuf::from(path));
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// **`S-A10` "Done when" for `capsule import`.** Import in one process; read the asset back in a
/// second that shares nothing but the directory; then check that what is on disk is the full
/// signed artifact set, in the layout the design fixes, carrying the metadata the importer
/// parsed out of the source file.
#[test]
fn an_import_is_reconstructed_by_a_later_process_from_disk_alone() {
    let fx = fixture();

    // ── Process 2: import. ──
    fx.import();

    let (bucket, asset_id) = sole_asset_on_disk(&fx.library);

    // ── Process 3: read the library back. A fresh `Workspace::open` in a fresh process must
    //    recover the album key, the authority, and this asset from disk, or it sees nothing. ──
    let view = fx.run(
        &[
            "cull",
            "--library",
            path(&fx.library),
            "--passphrase-stdin",
            "--filter",
            "neutral",
        ],
        true,
    );
    assert!(
        view.contains("Cull view: 0 pick, 1 neutral, 0 reject"),
        "a new process must see the imported asset\nstdout:\n{view}"
    );
    assert!(
        view.contains(&asset_id.to_string()),
        "the reopened library must name the asset the import wrote\nstdout:\n{view}"
    );

    // ── The artifacts, beside the original, in `media/{YYYY}/{YYYY-MM}/`. ──
    let simple = asset_id.simple().to_string();
    for name in [
        format!("{simple}.jpg"),             // the original
        format!("{simple}.cbor"),            // the signed sidecar
        format!("{simple}.provenance.cbor"), // the append-only chain
        format!("{simple}.metadata.bin"),    // the sealed metadata blob
    ] {
        assert!(
            bucket.join(&name).is_file(),
            "{name} must exist beside the original in {}",
            bucket.display()
        );
    }
    assert_eq!(
        std::fs::read(bucket.join(format!("{simple}.jpg"))).expect("read the stored original"),
        fx.image,
        "the stored original must be the bytes that were imported"
    );

    // ── The signed sidecar, decoded from disk. ──
    let bytes = std::fs::read(bucket.join(format!("{simple}.cbor"))).expect("read the sidecar");
    let sidecar =
        SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1).expect("decode the sidecar");
    assert_eq!(sidecar.uuid, asset_id);
    assert_eq!(sidecar.content_type, "image/jpeg");
    assert_eq!(
        sidecar.hash,
        hash::hash_bytes(&fx.image),
        "the sidecar commits to the content address of the imported plaintext"
    );
    assert!(
        sidecar.signature.is_some(),
        "imports land on the signed path (S-B2), never the legacy unsigned sidecar"
    );

    // The EXIF the importer genuinely parsed: neither value exists anywhere but inside the
    // APP1 segment `synthetic_jpeg` wrote, so a stub input could not produce them.
    let dimensions = sidecar
        .dimensions
        .as_ref()
        .expect("dimensions come from the EXIF PixelXDimension/PixelYDimension tags");
    assert_eq!((dimensions.width, dimensions.height), (8, 8));
    let gps = sidecar.gps.as_ref().expect("the EXIF GPS fix");
    assert!(
        (gps.lat - EXIF_LAT).abs() < 1e-6 && (gps.lon - EXIF_LON).abs() < 1e-6,
        "the EXIF GPS rationals must survive as the sidecar fix, got {gps:?}"
    );

    // The bucket is not an arbitrary directory: it is the one `media_dir` derives from this
    // asset's own capture timestamp, which is what makes the layout reconstructible.
    let captured: Timestamp = sidecar
        .capture_timestamp
        .parse()
        .expect("capture_timestamp is RFC 3339");
    let date = captured.to_zoned(jiff::tz::TimeZone::UTC).date();
    assert_eq!(
        bucket,
        fx.library
            .join("media")
            .join(format!("{:04}", date.year()))
            .join(format!("{:04}-{:02}", date.year(), date.month())),
        "the asset must live in the month bucket its capture timestamp names"
    );

    // ── The provenance chain: one signed `create` naming the derived de facto album. ──
    let bytes = std::fs::read(bucket.join(format!("{simple}.provenance.cbor")))
        .expect("read the provenance chain");
    let records: Vec<ProvenanceRecord> =
        capsule_core::cbor::from_slice(&bytes).expect("decode the provenance chain");
    assert_eq!(records.len(), 1, "a fresh import is a chain of one");
    let core = &records[0].manifest.core;
    assert_eq!(core.file_id, asset_id);
    assert_eq!(core.album_id, fx.album, "S-B12: the derived de facto album");
    assert_eq!(core.action, Action::Create);
    assert_eq!(core.plaintext_size as usize, fx.image.len());
    assert!(
        records[0].prior_provenance_hash.is_none(),
        "the create is the head of a new chain"
    );
}

/// **`S-D21`.** `capsule library rebuild` reconstructs the index of a *signed* library — the
/// case that was rebuilding zero rows — and it does so in a process that starts from nothing but
/// the sidecars and chains on disk.
#[test]
fn library_rebuild_reconstructs_the_index_in_a_new_process() {
    let fx = fixture();
    fx.import();
    let (_bucket, asset_id) = sole_asset_on_disk(&fx.library);

    // The row the importing process wrote, read back from its SQLite file.
    assert!(
        index_row(&fx.library, &asset_id).is_some(),
        "import writes through to the queryable index"
    );

    destroy_index(&fx.library);
    assert!(
        index_row(&fx.library, &asset_id).is_none(),
        "the index must actually be gone before the rebuild is asked to earn its keep"
    );

    // ── A new process rebuilds it. ──
    let out = capsule(&fx.home, &["library", "rebuild", path(&fx.library)], false);
    assert!(out.contains("Index rebuilt successfully"), "stdout:\n{out}");

    let row = index_row(&fx.library, &asset_id)
        .expect("S-D21: a signed library must rebuild its rows, not zero of them");
    assert_eq!(row.uuid, asset_id.to_string());
    assert_eq!(row.asset_type, "photo");
    assert_eq!(
        row.album_id.as_deref(),
        Some(fx.album.to_string().as_str()),
        "the owning album is replayed from the provenance chain, not guessed"
    );
    assert_eq!(
        row.hash_sha256,
        hash::hash_bytes(&fx.image).to_hex(),
        "the rebuilt row addresses the same content the sidecar signed"
    );
    assert_eq!((row.width, row.height), (Some(8), Some(8)));
    assert!(!row.is_deleted, "a live asset must not rebuild as trashed");
    assert!(
        !row.is_hidden,
        "a never-written `hidden` register rebuilds as visible (S-D19/S-D21)"
    );

    // And the workspace's own restore never depended on the index: a further process still sees
    // the asset, because `Workspace::open` reconstructs it from the sidecars (`S-A10`).
    let view = fx.run(
        &[
            "cull",
            "--library",
            path(&fx.library),
            "--passphrase-stdin",
            "--filter",
            "neutral",
        ],
        true,
    );
    assert!(
        view.contains("Cull view: 0 pick, 1 neutral, 0 reject"),
        "stdout:\n{view}"
    );
}

/// `capsule list` is the **sync feed's** view, not the library's: it takes no `--library` and
/// reads the CLI's own database under the user's data directory. A locally imported asset is
/// therefore invisible to it, which is why the round trip above reads back through `capsule
/// cull`. Pinned here so the gap is a stated fact rather than a surprise.
#[test]
fn capsule_list_reports_the_sync_feed_not_the_library() {
    let fx = fixture();
    fx.import();
    let (_bucket, asset_id) = sole_asset_on_disk(&fx.library);

    let listed = capsule(&fx.home, &["list"], false);
    assert!(
        !listed.contains(&asset_id.to_string()),
        "`capsule list` reports synced assets; an offline import is not one\nstdout:\n{listed}"
    );
}

// ── The fixture image ────────────────────────────────────────────────────────

/// A real 8×8 grayscale baseline JPEG carrying an EXIF APP1 segment, built byte by byte so the
/// repository carries no binary fixture.
///
/// The EXIF block is a big-endian TIFF structure with three IFDs — IFD0 (make/model + pointers),
/// the Exif SubIFD (`DateTimeOriginal`, `OffsetTimeOriginal`, pixel dimensions) and the GPS IFD
/// (a latitude/longitude fix as EXIF rationals). The image itself is a genuine baseline JPEG:
/// a flat quantization table, a single-component `SOF0`, minimal-but-valid DC and AC Huffman
/// tables (one code each), and a one-byte entropy segment encoding a single DC-only block.
fn synthetic_jpeg() -> Vec<u8> {
    const ASCII: u16 = 2;
    const LONG: u16 = 4;
    const RATIONAL: u16 = 5;

    const MAKE: &[u8] = b"Capsule\0";
    const MODEL: &[u8] = b"Synth\0";
    // The EXIF capture time, with an explicit UTC offset so the resolved capture instant is a
    // fixed one rather than a timezone lookup.
    const DATE_TIME_ORIGINAL: &[u8] = b"2019:03:04 05:06:07\0";
    const OFFSET_TIME_ORIGINAL: &[u8] = b"+00:00\0";

    // Each IFD here holds four entries: 2 count bytes + 4×12 entry bytes + 4 next-IFD bytes.
    const IFD_LEN: u32 = 2 + 4 * 12 + 4;
    const IFD0_AT: u32 = 8; // immediately after the 8-byte TIFF header
    const EXIF_IFD_AT: u32 = IFD0_AT + IFD_LEN;
    const GPS_IFD_AT: u32 = EXIF_IFD_AT + IFD_LEN;
    const DATA_AT: u32 = GPS_IFD_AT + IFD_LEN;
    const MAKE_AT: u32 = DATA_AT;
    const MODEL_AT: u32 = MAKE_AT + MAKE.len() as u32;
    const DTO_AT: u32 = MODEL_AT + MODEL.len() as u32;
    const OTO_AT: u32 = DTO_AT + DATE_TIME_ORIGINAL.len() as u32;
    // Rationals are 4-byte quantities; one pad byte keeps them aligned.
    const LAT_AT: u32 = OTO_AT + OFFSET_TIME_ORIGINAL.len() as u32 + 1;
    const LON_AT: u32 = LAT_AT + 24;

    /// One 12-byte IFD entry whose value is an offset into the TIFF block.
    fn at(tag: u16, kind: u16, count: u32, offset: u32) -> Vec<u8> {
        let mut e = Vec::with_capacity(12);
        e.extend_from_slice(&tag.to_be_bytes());
        e.extend_from_slice(&kind.to_be_bytes());
        e.extend_from_slice(&count.to_be_bytes());
        e.extend_from_slice(&offset.to_be_bytes());
        e
    }

    /// One 12-byte IFD entry whose value fits in the 4 inline bytes.
    fn inline(tag: u16, kind: u16, count: u32, value: [u8; 4]) -> Vec<u8> {
        let mut e = Vec::with_capacity(12);
        e.extend_from_slice(&tag.to_be_bytes());
        e.extend_from_slice(&kind.to_be_bytes());
        e.extend_from_slice(&count.to_be_bytes());
        e.extend_from_slice(&value);
        e
    }

    fn rational(numerator: u32, denominator: u32) -> Vec<u8> {
        let mut r = Vec::with_capacity(8);
        r.extend_from_slice(&numerator.to_be_bytes());
        r.extend_from_slice(&denominator.to_be_bytes());
        r
    }

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM"); // big-endian
    tiff.extend_from_slice(&42u16.to_be_bytes());
    tiff.extend_from_slice(&IFD0_AT.to_be_bytes());

    // IFD0: Make, Model, and the pointers to the two sub-IFDs.
    tiff.extend_from_slice(&4u16.to_be_bytes());
    tiff.extend(at(0x010F, ASCII, MAKE.len() as u32, MAKE_AT));
    tiff.extend(at(0x0110, ASCII, MODEL.len() as u32, MODEL_AT));
    tiff.extend(at(0x8769, LONG, 1, EXIF_IFD_AT));
    tiff.extend(at(0x8825, LONG, 1, GPS_IFD_AT));
    tiff.extend_from_slice(&0u32.to_be_bytes());

    // Exif SubIFD: capture time, its UTC offset, and the pixel dimensions.
    tiff.extend_from_slice(&4u16.to_be_bytes());
    tiff.extend(at(0x9003, ASCII, DATE_TIME_ORIGINAL.len() as u32, DTO_AT));
    tiff.extend(at(0x9011, ASCII, OFFSET_TIME_ORIGINAL.len() as u32, OTO_AT));
    tiff.extend(inline(0xA002, LONG, 1, 8u32.to_be_bytes()));
    tiff.extend(inline(0xA003, LONG, 1, 8u32.to_be_bytes()));
    tiff.extend_from_slice(&0u32.to_be_bytes());

    // GPS IFD: 48°51'29.6"N, 2°17'40.2"W.
    tiff.extend_from_slice(&4u16.to_be_bytes());
    tiff.extend(inline(0x0001, ASCII, 2, *b"N\0\0\0"));
    tiff.extend(at(0x0002, RATIONAL, 3, LAT_AT));
    tiff.extend(inline(0x0003, ASCII, 2, *b"W\0\0\0"));
    tiff.extend(at(0x0004, RATIONAL, 3, LON_AT));
    tiff.extend_from_slice(&0u32.to_be_bytes());

    // The out-of-line values, in the order the offsets above declare.
    tiff.extend_from_slice(MAKE);
    tiff.extend_from_slice(MODEL);
    tiff.extend_from_slice(DATE_TIME_ORIGINAL);
    tiff.extend_from_slice(OFFSET_TIME_ORIGINAL);
    tiff.push(0); // alignment pad
    for (numerator, denominator) in [(48, 1), (51, 1), (296, 10), (2, 1), (17, 1), (402, 10)] {
        tiff.extend(rational(numerator, denominator));
    }
    assert_eq!(
        tiff.len() as u32,
        LON_AT + 24,
        "the TIFF block must be exactly as long as its own offsets claim"
    );

    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);

    let mut jpeg = vec![0xFF, 0xD8]; // SOI
    jpeg.extend_from_slice(&[0xFF, 0xE1]); // APP1
    jpeg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    jpeg.extend_from_slice(&app1);

    // DQT: one flat 8-bit luminance table.
    jpeg.extend_from_slice(&[0xFF, 0xDB]);
    jpeg.extend_from_slice(&(2u16 + 1 + 64).to_be_bytes());
    jpeg.push(0x00);
    jpeg.extend(std::iter::repeat_n(1u8, 64));

    // SOF0: baseline, 8-bit, 8×8, one component with no subsampling.
    jpeg.extend_from_slice(&[0xFF, 0xC0]);
    jpeg.extend_from_slice(&11u16.to_be_bytes());
    jpeg.extend_from_slice(&[0x08]); // sample precision
    jpeg.extend_from_slice(&8u16.to_be_bytes()); // height
    jpeg.extend_from_slice(&8u16.to_be_bytes()); // width
    jpeg.extend_from_slice(&[0x01, 0x01, 0x11, 0x00]); // 1 component, id 1, 1×1, table 0

    // DHT: a DC and an AC table each holding a single 1-bit code for symbol 0 — the shortest
    // pair of tables a conformant decoder accepts.
    for class_and_id in [0x00u8, 0x10] {
        jpeg.extend_from_slice(&[0xFF, 0xC4]);
        jpeg.extend_from_slice(&(2u16 + 1 + 16 + 1).to_be_bytes());
        jpeg.push(class_and_id);
        jpeg.push(1); // one code of length 1
        jpeg.extend(std::iter::repeat_n(0u8, 15));
        jpeg.push(0x00); // the symbol that code means
    }

    // SOS, then the entropy-coded data: DC category 0 ("0") followed by EOB ("0"), padded to a
    // byte with 1 bits — a single all-zero block.
    jpeg.extend_from_slice(&[0xFF, 0xDA]);
    jpeg.extend_from_slice(&8u16.to_be_bytes());
    jpeg.extend_from_slice(&[0x01, 0x01, 0x00, 0x00, 0x3F, 0x00]);
    jpeg.push(0x3F);

    jpeg.extend_from_slice(&[0xFF, 0xD9]); // EOI
    jpeg
}
