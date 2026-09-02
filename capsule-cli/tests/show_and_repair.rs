//! Slices `S-B18` (`capsule show`) and `S-B17` (`capsule repair capture-time`) across a real
//! process boundary, by spawning the `capsule` binary the way `import_round_trip.rs` and
//! `takeout_import.rs` do — and for the same reason: `show` proves what a library *reopened
//! from disk* says about an asset, and a repair that only looked right inside the process
//! that wrote it would prove nothing.
//!
//! **The fixture image** is a synthesized JPEG carrying a real EXIF APP1 segment with
//! `DateTimeOriginal` **and** `OffsetTimeOriginal`, so its capture time resolves to a fixed
//! UTC instant — the case the importer writes as the capture timestamp since `S-B16`, and
//! therefore the case the repair can recover. The Argon2id seeding is the fast-cost trick
//! `import_round_trip.rs` explains; nothing here weakens what the spawned processes run.
//!
//! **The pre-`S-B16` library** the repair exists for is reproduced by importing under the
//! fixed parser and then stamping one asset's sidecar with the import clock through the very
//! API the repair uses (`Workspace::set_capture_timestamp`). That reproduces the half the
//! repair reads — a signed sidecar carrying `now` under a still-EXIF-bearing original — and it
//! is the only way to produce it from a build whose importer no longer has the bug. It does
//! **not** reproduce the other half of the old shape: the bug also sharded the bundle into the
//! import month, whereas here the shard is the EXIF month, so the post-repair
//! directory-vs-timestamp drift and `Workspace::open`'s reconciliation of it are exercised by
//! `capsule-core`'s `lifecycle::metadata` unit test (a no-EXIF import corrected to 2001), not
//! here.
//!
//! ## Test list
//!
//! - `show_prints_the_signed_sidecar_by_hash_prefix_or_id` — the guide's sampling step:
//!   a hash prefix from `shasum` and the asset id both resolve, and the page carries the
//!   EXIF-derived values.
//! - `show_refuses_a_malformed_or_unknown_selector` — non-zero exit with a localized reason.
//! - `repair_is_a_no_op_on_a_library_imported_after_the_parser_fix` — the `S-B17`
//!   "no-op post-`S-B16`" criterion.
//! - `repair_reports_by_default_and_corrects_under_apply` — detects the reproduced bug,
//!   writes nothing without `--apply`, corrects with it, keeps the bundle in its original
//!   month directory, survives `capsule library rebuild`, and finds nothing on a second run.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use capsule_core::crypto::hash;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::lifecycle::Workspace;
use uuid::Uuid;

const PASSPHRASE: &str = "show-and-repair-passphrase";

const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// The EXIF capture instant baked into [`exif_jpeg`], as the sidecar renders it.
const EXIF_CAPTURE: &str = "2019-03-04T05:06:07Z";

// ── Scratch plumbing ─────────────────────────────────────────────────────────

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-cli-s-b17-{}", nanoid::nanoid!()));
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

fn spawn(home: &Path, args: &[&str], passphrase_stdin: bool) -> Output {
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

/// The combined output of one **failing** `capsule` run.
fn capsule_fails(home: &Path, args: &[&str], passphrase_stdin: bool) -> String {
    let out = spawn(home, args, passphrase_stdin);
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "capsule {args:?} was expected to fail\noutput:\n{text}"
    );
    text
}

fn path(p: &Path) -> &str {
    p.to_str().expect("scratch paths are UTF-8")
}

// ── The fixture image ────────────────────────────────────────────────────────

/// A JPEG container holding one EXIF APP1 segment: `DateTimeOriginal` 2019-03-04 05:06:07
/// with `OffsetTimeOriginal` +00:00, and 8×8 pixel dimensions. `salt` is appended after the
/// segment so several files share the EXIF and differ in bytes (and therefore in hash).
fn exif_jpeg(salt: &[u8]) -> Vec<u8> {
    const DTO: &[u8] = b"2019:03:04 05:06:07\0";
    const OTO: &[u8] = b"+00:00\0";
    const IFD0_AT: u32 = 8;
    // IFD0 holds one entry (the Exif pointer): 2 + 12 + 4 bytes.
    const EXIF_IFD_AT: u32 = IFD0_AT + 2 + 12 + 4;
    // The Exif IFD holds four entries.
    const DATA_AT: u32 = EXIF_IFD_AT + 2 + 4 * 12 + 4;
    const DTO_AT: u32 = DATA_AT;
    const OTO_AT: u32 = DTO_AT + DTO.len() as u32;

    fn entry(tiff: &mut Vec<u8>, tag: u16, kind: u16, count: u32, value: [u8; 4]) {
        tiff.extend_from_slice(&tag.to_be_bytes());
        tiff.extend_from_slice(&kind.to_be_bytes());
        tiff.extend_from_slice(&count.to_be_bytes());
        tiff.extend_from_slice(&value);
    }

    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM");
    tiff.extend_from_slice(&42u16.to_be_bytes());
    tiff.extend_from_slice(&IFD0_AT.to_be_bytes());

    tiff.extend_from_slice(&1u16.to_be_bytes());
    entry(&mut tiff, 0x8769, 4, 1, EXIF_IFD_AT.to_be_bytes());
    tiff.extend_from_slice(&0u32.to_be_bytes());

    tiff.extend_from_slice(&4u16.to_be_bytes());
    entry(&mut tiff, 0x9003, 2, DTO.len() as u32, DTO_AT.to_be_bytes());
    entry(&mut tiff, 0x9011, 2, OTO.len() as u32, OTO_AT.to_be_bytes());
    entry(&mut tiff, 0xA002, 4, 1, 8u32.to_be_bytes());
    entry(&mut tiff, 0xA003, 4, 1, 8u32.to_be_bytes());
    tiff.extend_from_slice(&0u32.to_be_bytes());

    assert_eq!(
        tiff.len() as u32,
        DATA_AT,
        "the IFDs end where the data starts"
    );
    tiff.extend_from_slice(DTO);
    tiff.extend_from_slice(OTO);

    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    let mut jpeg = vec![0xFF, 0xD8];
    jpeg.extend_from_slice(&[0xFF, 0xE1]);
    jpeg.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    jpeg.extend_from_slice(&app1);
    // A COM segment carrying the salt keeps the container well-formed while making each
    // fixture's bytes (and hash) distinct.
    jpeg.extend_from_slice(&[0xFF, 0xFE]);
    jpeg.extend_from_slice(&((salt.len() + 2) as u16).to_be_bytes());
    jpeg.extend_from_slice(salt);
    jpeg.extend_from_slice(&[0xFF, 0xD9]);
    jpeg
}

// ── The fixture ──────────────────────────────────────────────────────────────

struct Fixture {
    _scratch: ScratchDir,
    home: PathBuf,
    library: PathBuf,
    /// The bytes of every imported fixture image, in source order.
    images: Vec<Vec<u8>>,
}

impl Fixture {
    fn run(&self, args: &[&str]) -> String {
        capsule(&self.home, args, true)
    }

    fn run_fails(&self, args: &[&str]) -> String {
        capsule_fails(&self.home, args, true)
    }

    /// `capsule show --library <library> <selector>`.
    fn show(&self, selector: &str) -> String {
        self.run(&[
            "show",
            selector,
            "--library",
            path(&self.library),
            "--passphrase-stdin",
        ])
    }

    fn hash_hex(&self, image: &[u8]) -> String {
        hash::hash_bytes(image).to_hex()
    }

    /// The library reopened in this process, after every spawned `capsule` has exited.
    fn reopen(&self) -> Workspace {
        Workspace::open(&self.library, PASSPHRASE.as_bytes(), FAST_KDF).expect("reopen")
    }

    /// The asset holding exactly `image`.
    fn asset_for(&self, ws: &Workspace, image: &[u8]) -> Uuid {
        ws.asset_ids()
            .into_iter()
            .find(|id| ws.read_plaintext(id).is_ok_and(|p| p == image))
            .expect("an imported asset holding these bytes")
    }
}

/// `capsule library init` + a fast-cost account + `capsule import` of `count` EXIF JPEGs.
fn fixture(count: usize) -> Fixture {
    let scratch = ScratchDir::new();
    let home = scratch.path().join("home");
    let library = scratch.path().join("library");
    let source = scratch.path().join("source");
    std::fs::create_dir_all(&home).expect("create scratch home");
    std::fs::create_dir_all(&source).expect("create source dir");
    let images: Vec<Vec<u8>> = (0..count)
        .map(|n| {
            let image = exif_jpeg(format!("fixture {n}").as_bytes());
            std::fs::write(source.join(format!("photo-{n}.jpg")), &image).expect("fixture");
            image
        })
        .collect();

    let out = capsule(
        &home,
        &["library", "init", path(&library), "--name", "Repair"],
        false,
    );
    assert!(out.contains("Library created at"), "stdout:\n{out}");
    drop(Workspace::open(&library, PASSPHRASE.as_bytes(), FAST_KDF).expect("seed the account"));

    let out = capsule(
        &home,
        &[
            "import",
            path(&source),
            "--library",
            path(&library),
            "--passphrase-stdin",
        ],
        true,
    );
    assert!(
        out.contains(&format!(
            "Done: {count} imported, 0 duplicate(s), 0 error(s)."
        )),
        "stdout:\n{out}"
    );

    Fixture {
        _scratch: scratch,
        home,
        library,
        images,
    }
}

// ── `capsule show` (S-B18) ───────────────────────────────────────────────────

/// The guide's metadata-sampling step, executed: the SHA-256 a user computed over the source
/// file (here, its first eight hex digits) resolves to the imported asset, and the page shows
/// the values that can only have come from the EXIF segment. The asset id resolves too.
#[test]
fn show_prints_the_signed_sidecar_by_hash_prefix_or_id() {
    let fx = fixture(2);
    let image = &fx.images[0];
    let hex = fx.hash_hex(image);

    let page = fx.show(&hex[..8]);
    assert!(page.contains(&hex), "the full hash is printed:\n{page}");
    assert!(
        page.contains(EXIF_CAPTURE),
        "the EXIF capture instant:\n{page}"
    );
    assert!(page.contains("8×8"), "the EXIF dimensions:\n{page}");
    assert!(page.contains("image/jpeg"), "{page}");
    assert!(
        page.contains("(unset)"),
        "absent fields are spelled out:\n{page}"
    );
    assert!(
        page.contains("Provenance:      1 signed record(s)"),
        "a fresh import on this build is a chain of one:\n{page}"
    );
    assert!(!page.contains("cli.show."), "no raw catalog key:\n{page}");

    let ws = fx.reopen();
    let id = fx.asset_for(&ws, image);
    assert!(
        page.contains(&id.to_string()),
        "the resolved asset's id:\n{page}"
    );
    drop(ws);
    let by_id = fx.show(&id.to_string());
    assert_eq!(
        by_id, page,
        "the id and the hash prefix name the same asset"
    );
}

/// `show` refuses rather than guesses: a malformed selector, an unknown one, and a prefix
/// too short to be accepted each fail with a localized reason and a non-zero exit.
#[test]
fn show_refuses_a_malformed_or_unknown_selector() {
    let fx = fixture(1);
    let library = path(&fx.library);

    let malformed = fx.run_fails(&[
        "show",
        "not-a-selector",
        "--library",
        library,
        "--passphrase-stdin",
    ]);
    assert!(
        malformed.contains("neither an asset id nor a hex prefix"),
        "{malformed}"
    );

    let unknown = fx.run_fails(&[
        "show",
        "0000000000000000",
        "--library",
        library,
        "--passphrase-stdin",
    ]);
    assert!(
        unknown.contains("No asset in this library matches"),
        "{unknown}"
    );

    let short = fx.run_fails(&["show", "abcdef", "--library", library, "--passphrase-stdin"]);
    assert!(short.contains("at least 8"), "{short}");
}

// ── `capsule repair capture-time` (S-B17) ────────────────────────────────────

impl Fixture {
    /// `capsule repair capture-time --library <library> [--apply]`.
    fn repair(&self, apply: bool) -> String {
        let mut args = vec![
            "repair",
            "capture-time",
            "--library",
            path(&self.library),
            "--passphrase-stdin",
        ];
        if apply {
            args.push("--apply");
        }
        self.run(&args)
    }

    /// The month directory holding the asset's original, relative to the library root.
    fn month_dir(&self, ws: &Workspace, id: Uuid) -> PathBuf {
        ws.original_path(&id)
            .expect("original")
            .parent()
            .expect("month directory")
            .strip_prefix(&self.library)
            .expect("inside the library")
            .to_path_buf()
    }
}

/// **The "no-op after `S-B16`" half of the criterion.** Every asset here was imported by the
/// fixed parser, so each sidecar already carries its EXIF instant, and the pass reports nothing.
#[test]
fn repair_is_a_no_op_on_a_library_imported_after_the_parser_fix() {
    let fx = fixture(2);
    let out = fx.repair(false);
    assert!(out.contains("Checked 2 asset(s)"), "{out}");
    assert!(out.contains("nothing to repair"), "{out}");
    assert!(!out.contains("affected,"), "{out}");
    let ws = fx.reopen();
    for image in &fx.images {
        let id = fx.asset_for(&ws, image);
        assert_eq!(ws.asset(&id).expect("asset").chain.records().len(), 1);
    }
}

/// **The repair itself**, across process boundaries: one of two assets is stamped with the
/// import clock (the pre-`S-B16` shape); a dry run reports it and writes nothing; `--apply`
/// corrects it as a signed record without moving the bundle; `capsule show` and a
/// `capsule library rebuild` both read the corrected instant; a second `--apply` finds nothing.
#[test]
fn repair_reports_by_default_and_corrects_under_apply() {
    let fx = fixture(2);
    let (broken_image, good_image) = (&fx.images[0], &fx.images[1]);

    // Reproduce the bug on one asset, in-process, and remember where its bundle lives.
    let (broken, month_before) = {
        let mut ws = fx.reopen();
        let broken = fx.asset_for(&ws, broken_image);
        ws.set_capture_timestamp(&broken, jiff::Timestamp::now())
            .expect("reproduce the pre-S-B16 stamp");
        let month = fx.month_dir(&ws, broken);
        (broken, month)
    };
    assert!(
        !fx.show(&broken.to_string()).contains(EXIF_CAPTURE),
        "the reproduced stamp is no longer the EXIF instant"
    );

    // Dry run (the default): reported, not written.
    let dry = fx.repair(false);
    assert!(dry.contains("Checked 2 asset(s)"), "{dry}");
    assert!(dry.contains("Dry run"), "{dry}");
    assert!(dry.contains(&broken.to_string()), "{dry}");
    assert!(
        dry.contains(EXIF_CAPTURE),
        "the recovered instant is named:\n{dry}"
    );
    assert!(
        dry.contains("1 affected, 0 corrected, 1 already correct"),
        "{dry}"
    );
    {
        let ws = fx.reopen();
        assert_eq!(ws.asset(&broken).expect("asset").chain.records().len(), 2);
        assert!(
            !ws.asset(&broken)
                .expect("asset")
                .sidecar
                .capture_timestamp
                .starts_with("2019")
        );
    }

    // `--apply`: corrected as a third signed record; the bundle stays where it was.
    let applied = fx.repair(true);
    assert!(!applied.contains("Dry run"), "{applied}");
    assert!(
        applied.contains("1 affected, 1 corrected, 1 already correct"),
        "{applied}"
    );
    assert!(
        applied.contains("month directory"),
        "the drift notice:\n{applied}"
    );
    {
        let ws = fx.reopen();
        let asset = ws.asset(&broken).expect("asset");
        assert_eq!(asset.sidecar.capture_timestamp, EXIF_CAPTURE);
        assert_eq!(asset.chain.records().len(), 3);
        assert_eq!(
            fx.month_dir(&ws, broken),
            month_before,
            "the bundle is not relocated"
        );
        assert!(ws.original_path(&broken).expect("original").is_file());
        let good = fx.asset_for(&ws, good_image);
        assert_eq!(
            ws.asset(&good).expect("asset").chain.records().len(),
            1,
            "untouched"
        );
    }
    let page = fx.show(&broken.to_string());
    assert!(
        page.contains(&format!("Captured:        {EXIF_CAPTURE}")),
        "{page}"
    );
    assert!(
        page.contains("Provenance:      3 signed record(s)"),
        "{page}"
    );

    // The two index projections agree: a rebuild from disk reads the same corrected instant.
    let rebuilt = capsule(&fx.home, &["library", "rebuild", path(&fx.library)], false);
    assert!(rebuilt.contains("Index rebuilt successfully."), "{rebuilt}");
    {
        let library = capsule_core::library::open_library(&fx.library).expect("open");
        let row = library
            .db
            .find_by_uuid(&broken.to_string())
            .expect("query")
            .expect("indexed");
        assert_eq!(row.capture_timestamp, 1_551_675_967, "2019-03-04T05:06:07Z");
    }
    assert!(fx.show(&broken.to_string()).contains(EXIF_CAPTURE));

    // Idempotent: nothing left to do.
    let again = fx.repair(true);
    assert!(again.contains("nothing to repair"), "{again}");
    let ws = fx.reopen();
    assert_eq!(ws.asset(&broken).expect("asset").chain.records().len(), 3);
}
