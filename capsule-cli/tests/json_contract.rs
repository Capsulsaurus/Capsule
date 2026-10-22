//! The CLI's machine contract, snapshotted (testing plan, `capsule-cli` row: "Every
//! subcommand's `--json` output and exit code are a stable contract").
//!
//! Each case spawns the real `capsule` binary in a scratch home, with no server and no network,
//! and snapshots one rendering of the run: its exit status, its stdout and its stderr. A change
//! to any of the three is a snapshot diff a reviewer accepts (`cargo insta review`, or
//! `INSTA_UPDATE=always` on the test run) or refuses; CI never writes one. Scratch paths are
//! redacted, and stderr stops at color-eyre's `Location:` section, whose line numbers are not
//! part of the contract.
//!
//! The `--json` subcommands today are `import`, `push`, `restore`, `verify`, `cache budget`,
//! `cache sweep` and `sync reannounce`; each has its offline cases here, and every `--json`
//! subcommand that lands joins this file the same way.
//! What needs a live server (a push or restore that reaches its assets) is the end-to-end
//! suites' to cover.
//!
//! ## Test list
//!
//! - `push_json_on_an_empty_library_prints_the_empty_summary` — exit 0, one JSON document.
//! - `push_json_without_a_session_prints_nothing_on_stdout` — a run-level failure: non-zero
//!   exit, empty stdout, the reason on stderr.
//! - `push_json_without_a_library_is_a_usage_error` — the parser's exit status 2.
//! - `restore_json_without_a_session_prints_nothing_on_stdout` — a run-level failure, as for
//!   push: non-zero exit, empty stdout, the reason on stderr, and nothing written to `--out`.
//! - `restore_json_without_an_out_directory_is_a_usage_error` — the parser's exit status 2.
//! - `verify_json_without_a_session_prints_nothing_on_stdout` — a run-level failure, as for
//!   push: non-zero exit, empty stdout, the reason on stderr, and no attestation stored.
//! - `verify_json_without_a_library_is_a_usage_error` — the parser's exit status 2.
//! - `sync_reannounce_json_without_a_session_prints_nothing_on_stdout` — a run-level failure, as
//!   for push: non-zero exit, empty stdout, the reason on stderr.
//! - `sync_reannounce_json_without_a_library_is_a_usage_error` — the parser's exit status 2.
//! - `import_json_reports_every_file_and_its_deferred_derivatives` — exit 0, one JSON document:
//!   a HEIC, a video, a document and a zero-byte file all imported, with each one's
//!   derivative status and the run's deferred count.
//! - `import_json_reports_an_unreadable_source_when_nothing_is_planned` — exit 0, one JSON
//!   document naming the unreadable source as a failed file and counting it as an error.
//! - `import_json_with_push_is_a_usage_error` — the parser's exit status 2.
//! - `cache_budget_json_reports_the_default_plan` — exit 0, one JSON document: the desktop
//!   class (`unmetered`) and the 2 GiB default base budget.
//! - `cache_budget_json_reports_a_stored_setting` — a `--set` base budget persists into a later
//!   run's plan.
//! - `cache_budget_set_and_reset_is_a_usage_error` — the parser's exit status 2.
//! - `cache_sweep_json_on_an_empty_library_evicts_nothing` — exit 0, one JSON document: the plan
//!   and an empty eviction list.
//! - `cache_sweep_json_without_a_library_is_a_usage_error` — the parser's exit status 2.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::lifecycle::Workspace;

const PASSPHRASE: &str = "json-contract-passphrase";

/// The import tests' fast-cost KDF: seeding the account with it keeps each spawned unlock cheap
/// without changing what the spawned process runs.
const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-cli-json-{}", nanoid::nanoid!()));
        std::fs::create_dir_all(path.join("home")).expect("create scratch home");
        Self(path)
    }

    fn home(&self) -> PathBuf {
        self.0.join("home")
    }

    fn library(&self) -> PathBuf {
        self.0.join("library")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn path(p: &Path) -> &str {
    p.to_str().expect("scratch paths are UTF-8")
}

/// `text` without its ANSI SGR sequences: color-eyre tints its report whatever `NO_COLOR` says,
/// and the tint is not part of the contract.
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Run `capsule` with `args` in `scratch`'s home, the passphrase on stdin, and render the run
/// as the snapshot sees it.
fn run(scratch: &Scratch, args: &[&str]) -> String {
    let home = scratch.home();
    let mut child = Command::new(env!("CARGO_BIN_EXE_capsule"))
        .args(args)
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "off")
        .env("RUST_BACKTRACE", "0")
        .env("LC_ALL", "en_US.UTF-8")
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_CACHE_HOME", home.join("cache"))
        // No server is listening here: a case that reached for the network would fail fast
        // rather than reach a real one.
        .env("CAPSULE_ENDPOINT", "http://127.0.0.1:9")
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
    let out = child.wait_with_output().expect("wait for capsule");
    let redact = |text: &[u8]| {
        strip_ansi(&String::from_utf8_lossy(text)).replace(path(&scratch.0), "[SCRATCH]")
    };
    let stderr = redact(&out.stderr);
    let stderr = stderr
        .split("\nLocation:")
        .next()
        .unwrap_or_default()
        .trim_end();
    format!(
        "status: {}\n--- stdout ---\n{}\n--- stderr ---\n{stderr}\n",
        out.status
            .code()
            .map_or_else(|| "signal".to_string(), |c| c.to_string()),
        redact(&out.stdout).trim_end(),
    )
}

/// `capsule library init` plus a fast-cost account, and `count` imported files.
fn library(scratch: &Scratch, count: usize) {
    let library = scratch.library();
    let init = run(
        scratch,
        &["library", "init", path(&library), "--name", "Contract"],
    );
    assert!(init.starts_with("status: 0"), "{init}");
    drop(Workspace::open(&library, PASSPHRASE.as_bytes(), FAST_KDF).expect("seed the account"));
    if count == 0 {
        return;
    }
    let source = scratch.0.join("source");
    std::fs::create_dir_all(&source).expect("create source dir");
    for n in 0..count {
        // A JPEG container (SOI, one comment segment, EOI): enough for the importer to store it
        // as an original, and distinct per file.
        let salt = format!("json contract {n}");
        let mut jpeg = vec![0xFF, 0xD8, 0xFF, 0xFE];
        jpeg.extend_from_slice(&u16::try_from(salt.len() + 2).expect("short").to_be_bytes());
        jpeg.extend_from_slice(salt.as_bytes());
        jpeg.extend_from_slice(&[0xFF, 0xD9]);
        std::fs::write(source.join(format!("photo-{n}.jpg")), jpeg).expect("write fixture");
    }
    let import = run(
        scratch,
        &[
            "import",
            path(&source),
            "--library",
            path(&library),
            "--passphrase-stdin",
        ],
    );
    assert!(import.starts_with("status: 0"), "{import}");
}

#[test]
fn push_json_on_an_empty_library_prints_the_empty_summary() {
    let scratch = Scratch::new();
    library(&scratch, 0);
    insta::assert_snapshot!(run(
        &scratch,
        &[
            "push",
            "--library",
            path(&scratch.library()),
            "--passphrase-stdin",
            "--json",
        ],
    ));
}

#[test]
fn push_json_without_a_session_prints_nothing_on_stdout() {
    let scratch = Scratch::new();
    library(&scratch, 1);
    insta::assert_snapshot!(run(
        &scratch,
        &[
            "push",
            "--library",
            path(&scratch.library()),
            "--passphrase-stdin",
            "--json",
        ],
    ));
}

#[test]
fn push_json_without_a_library_is_a_usage_error() {
    let scratch = Scratch::new();
    insta::assert_snapshot!(run(&scratch, &["push", "--json"]));
}

#[test]
fn sync_reannounce_json_without_a_session_prints_nothing_on_stdout() {
    let scratch = Scratch::new();
    library(&scratch, 1);
    insta::assert_snapshot!(run(
        &scratch,
        &[
            "sync",
            "reannounce",
            "--library",
            path(&scratch.library()),
            "--passphrase-stdin",
            "--json",
        ],
    ));
}

#[test]
fn sync_reannounce_json_without_a_library_is_a_usage_error() {
    let scratch = Scratch::new();
    insta::assert_snapshot!(run(&scratch, &["sync", "reannounce", "--json"]));
}

#[test]
fn restore_json_without_a_session_prints_nothing_on_stdout() {
    let scratch = Scratch::new();
    library(&scratch, 0);
    let out = scratch.0.join("restored");
    insta::assert_snapshot!(run(
        &scratch,
        &[
            "restore",
            "--library",
            path(&scratch.library()),
            "--out",
            path(&out),
            "--passphrase-stdin",
            "--json",
        ],
    ));
    let written = std::fs::read_dir(&out).map_or(0, Iterator::count);
    assert_eq!(written, 0, "a restore that never ran wrote into --out");
}

/// `text` with the value of every `"asset_id"` line replaced: asset ids are minted fresh by
/// each import, so they are not part of the contract.
fn redact_asset_ids(text: &str) -> String {
    text.lines()
        .map(|line| match line.split_once("\"asset_id\": \"") {
            Some((head, _)) => format!("{head}\"asset_id\": \"[ASSET_ID]\","),
            None => line.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every file is kept whatever the build can decode (issue #695): a HEIC, a video, a document
/// and a zero-byte file all import, and the report says which arrived without derivatives and
/// why.
#[test]
fn import_json_reports_every_file_and_its_deferred_derivatives() {
    let scratch = Scratch::new();
    library(&scratch, 0);
    let source = scratch.0.join("source");
    std::fs::create_dir_all(&source).expect("create source dir");
    let mut heic = vec![0, 0, 0, 0x18];
    heic.extend_from_slice(b"ftypheic\0\0\0\0heicmif1");
    std::fs::write(source.join("iphone.heic"), heic).expect("write heic");
    let mut mp4 = vec![0, 0, 0, 0x18];
    mp4.extend_from_slice(b"ftypmp42\0\0\0\0mp42isom");
    std::fs::write(source.join("clip.mp4"), mp4).expect("write mp4");
    std::fs::write(source.join("scan.pdf"), b"%PDF-1.4 contract").expect("write pdf");
    std::fs::write(source.join("empty"), b"").expect("write empty");

    insta::assert_snapshot!(redact_asset_ids(&run(
        &scratch,
        &[
            "import",
            path(&source),
            "--library",
            path(&scratch.library()),
            "--passphrase-stdin",
            "--json",
        ],
    )));
}

/// A source that cannot be read plans nothing, and the report still names it as a failed file
/// and counts it as an error rather than claiming an empty success.
#[test]
fn import_json_reports_an_unreadable_source_when_nothing_is_planned() {
    let scratch = Scratch::new();
    library(&scratch, 0);
    let missing = scratch.0.join("not-there");
    insta::assert_snapshot!(run(
        &scratch,
        &[
            "import",
            path(&missing),
            "--library",
            path(&scratch.library()),
            "--passphrase-stdin",
            "--json",
        ],
    ));
}

#[test]
fn import_json_with_push_is_a_usage_error() {
    let scratch = Scratch::new();
    insta::assert_snapshot!(run(
        &scratch,
        &[
            "import",
            path(&scratch.0),
            "--library",
            path(&scratch.library()),
            "--push",
            "--json",
        ],
    ));
}

#[test]
fn restore_json_without_an_out_directory_is_a_usage_error() {
    let scratch = Scratch::new();
    insta::assert_snapshot!(run(
        &scratch,
        &["restore", "--library", path(&scratch.library()), "--json"],
    ));
}

/// Every file under `dir` whose name ends with `suffix`.
fn files_ending_with(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            found.extend(files_ending_with(&path, suffix));
        } else if path.to_string_lossy().ends_with(suffix) {
            found.push(path);
        }
    }
    found
}

#[test]
fn verify_json_without_a_session_prints_nothing_on_stdout() {
    let scratch = Scratch::new();
    library(&scratch, 1);
    insta::assert_snapshot!(run(
        &scratch,
        &[
            "verify",
            "--library",
            path(&scratch.library()),
            "--signed",
            "--passphrase-stdin",
            "--json",
        ],
    ));
    assert!(
        files_ending_with(&scratch.library(), ".attestations.cbor").is_empty(),
        "a verify that never ran stored an attestation"
    );
}

#[test]
fn verify_json_without_a_library_is_a_usage_error() {
    let scratch = Scratch::new();
    insta::assert_snapshot!(run(&scratch, &["verify", "--signed", "--json"]));
}

#[test]
fn cache_budget_json_reports_the_default_plan() {
    let scratch = Scratch::new();
    insta::assert_snapshot!(run(&scratch, &["cache", "budget", "--json"]));
}

/// The setting is persisted: a later run that does not pass `--set` still reports it.
#[test]
fn cache_budget_json_reports_a_stored_setting() {
    let scratch = Scratch::new();
    let set = run(&scratch, &["cache", "budget", "--set", "1048576"]);
    assert!(set.starts_with("status: 0"), "{set}");
    insta::assert_snapshot!(run(&scratch, &["cache", "budget", "--json"]));
}

#[test]
fn cache_budget_set_and_reset_is_a_usage_error() {
    let scratch = Scratch::new();
    insta::assert_snapshot!(run(
        &scratch,
        &["cache", "budget", "--set", "1", "--reset", "--json"]
    ));
}

#[test]
fn cache_sweep_json_on_an_empty_library_evicts_nothing() {
    let scratch = Scratch::new();
    library(&scratch, 0);
    insta::assert_snapshot!(run(
        &scratch,
        &[
            "cache",
            "sweep",
            "--library",
            path(&scratch.library()),
            "--json"
        ],
    ));
}

#[test]
fn cache_sweep_json_without_a_library_is_a_usage_error() {
    let scratch = Scratch::new();
    insta::assert_snapshot!(run(&scratch, &["cache", "sweep", "--json"]));
}
