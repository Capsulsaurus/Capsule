//! Slice `S-D16` acceptance: the `capsule cull` flag → filtered view → reject-sweep loop
//! round-trips on a **reopened** library — and it is reopened by a *different process* each time.
//!
//! Every step below spawns the real `capsule` binary (`CARGO_BIN_EXE_capsule`, the path Cargo
//! hands integration tests of the crate that declares the bin). Nothing is shared between steps
//! but the library directory on disk, so a flag written by one process is only visible to the
//! next because `S-A10` made album keys, authorities, and per-asset state durable and
//! `Workspace::open` restores them. An in-process second `Workspace::open` could not distinguish
//! that from state surviving in a `HashMap`; a process boundary can, which is why this is a
//! subprocess test rather than a unit test — and why it needs no test dependency to be one.
//!
//! The fixture library's account is created with **fast** Argon2id parameters. That is not a
//! shortcut around the CLI's own cost: `pwkdf` records the wrap-time parameters inside the
//! wrapped blob and `unwrap` reads them back, so every `capsule cull` unlock below runs at the
//! recorded cost regardless of the `DeviceTier` the CLI passes for a *first-time* account.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::lifecycle::Workspace;
use uuid::Uuid;

const PASSPHRASE: &str = "cull-round-trip-passphrase";

/// Fast Argon2id for the fixture account; the production tier would dominate this test's runtime
/// four times over (once per spawned process).
const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// A temp directory that removes itself, so the test needs no `tempfile` dev-dependency.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-cli-s-d16-{}", nanoid::nanoid!()));
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

/// The stdout of one `capsule cull` run against `library`, asserting the process exited 0.
fn cull(library: &Path, args: &[&str]) -> String {
    let mut child = Command::new(env!("CARGO_BIN_EXE_capsule"))
        .arg("cull")
        .arg("--library")
        .arg(library)
        .arg("--passphrase-stdin")
        .args(args)
        // Colour codes would make the stdout assertions brittle; the CLI honours NO_COLOR.
        .env("NO_COLOR", "1")
        // The binary's fmt subscriber writes to stdout and defaults to DEBUG in a debug build,
        // which would interleave log lines into the output under assertion here.
        .env("RUST_LOG", "off")
        .env("LC_ALL", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn capsule cull");
    child
        .stdin
        .as_mut()
        .expect("stdin piped")
        .write_all(format!("{PASSPHRASE}\n").as_bytes())
        .expect("write passphrase");
    let out = child.wait_with_output().expect("wait for capsule cull");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "capsule cull {args:?} failed\nstdout:\n{stdout}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// Build a fixture library holding three signed assets and return their ids (sorted).
fn fixture_library(root: &Path) -> Vec<Uuid> {
    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("create source dir");
    let library = root.join("library");

    let mut ws = Workspace::create_with_params(&library, PASSPHRASE.as_bytes(), FAST_KDF)
        .expect("create fixture workspace");
    let album = ws.create_album("Shoot").expect("create album");

    let mut ids = Vec::new();
    for n in 0..3u8 {
        let file = src.join(format!("frame-{n}.jpg"));
        let mut bytes = b"\xFF\xD8\xFF cull fixture frame ".to_vec();
        bytes.push(n);
        std::fs::write(&file, &bytes).expect("write fixture image");
        ids.push(ws.import_asset(album, &file).expect("import fixture asset"));
    }
    ids.sort();
    ids
}

/// **S-D16 "Done when".** Flag in one process, read the filtered view back in a second, sweep in a
/// third, and confirm the sweep in a fourth — the whole culling loop over a library that is
/// reopened from disk every single time.
#[test]
fn cull_flag_filter_sweep_loop_round_trips_across_processes() {
    let scratch = ScratchDir::new();
    let ids = fixture_library(scratch.path());
    let library = scratch.path().join("library");
    let (keeper, doomed, undecided) = (ids[0], ids[1], ids[2]);

    // ── Process 1: flag. One keeper, one reject; the third is left untouched. ──
    let flagged = cull(
        &library,
        &[
            "--pick",
            &keeper.to_string(),
            "--reject",
            &doomed.to_string(),
        ],
    );
    assert!(
        flagged.contains("Flagged 1 asset(s) as pick"),
        "stdout:\n{flagged}"
    );
    assert!(
        flagged.contains("Flagged 1 asset(s) as reject"),
        "stdout:\n{flagged}"
    );
    assert!(
        flagged.contains("Cull view: 1 pick, 1 neutral, 1 reject"),
        "stdout:\n{flagged}"
    );

    // ── Process 2: the filtered view. A *fresh* process must see the flags on disk. ──
    let rejects = cull(&library, &["--filter", "reject"]);
    assert!(
        rejects.contains("1 asset(s) flagged reject"),
        "stdout:\n{rejects}"
    );
    assert!(
        rejects.contains(&doomed.to_string()),
        "the rejected asset must survive the process boundary\nstdout:\n{rejects}"
    );
    assert!(
        !rejects.contains(&keeper.to_string()),
        "the filtered view must not leak other flags\nstdout:\n{rejects}"
    );

    let picks = cull(&library, &["--filter", "pick"]);
    assert!(picks.contains(&keeper.to_string()), "stdout:\n{picks}");
    let neutrals = cull(&library, &["--filter", "neutral"]);
    assert!(
        neutrals.contains(&undecided.to_string()),
        "a never-flagged asset reads as neutral\nstdout:\n{neutrals}"
    );

    // ── Process 3: the reject sweep — the loop's only destructive step. ──
    let swept = cull(&library, &["--sweep", "--retain-days", "7"]);
    assert!(
        swept.contains("Swept 1 rejected asset(s) to trash; recoverable for 7 day(s)"),
        "stdout:\n{swept}"
    );

    // ── Process 4: confirm the sweep is durable and did not touch the other two. ──
    let after = cull(&library, &["--filter", "reject"]);
    assert!(
        after.contains("No assets are flagged reject"),
        "a swept asset is trashed, so it leaves the live filtered view\nstdout:\n{after}"
    );
    assert!(
        after.contains("Cull view: 1 pick, 1 neutral, 0 reject"),
        "the sweep must move exactly the rejects\nstdout:\n{after}"
    );

    // Sweeping again finds nothing — the loop is idempotent once the rejects are gone.
    let again = cull(&library, &["--sweep"]);
    assert!(
        again.contains("No rejected assets to sweep"),
        "stdout:\n{again}"
    );
}

/// Flagging an asset the library does not manage is refused, not silently ignored: a review
/// decision that was never recorded must not be reported as success.
#[test]
fn flagging_an_unknown_asset_fails_the_command() {
    let scratch = ScratchDir::new();
    let _ids = fixture_library(scratch.path());
    let library = scratch.path().join("library");

    let stray = Uuid::from_u128(0x0057_13A9);
    let out = Command::new(env!("CARGO_BIN_EXE_capsule"))
        .arg("cull")
        .arg("--library")
        .arg(&library)
        .arg("--passphrase-stdin")
        .arg("--reject")
        .arg(stray.to_string())
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "off")
        .env("LC_ALL", "en_US.UTF-8")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            child
                .stdin
                .as_mut()
                .expect("stdin piped")
                .write_all(format!("{PASSPHRASE}\n").as_bytes())?;
            child.wait_with_output()
        })
        .expect("run capsule cull");

    assert!(!out.status.success(), "an unknown asset id must fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&stray.to_string()),
        "the failure must name the offending asset\nstderr:\n{stderr}"
    );
}
