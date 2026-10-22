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
//! `push --json` is the surface that exists today. Every `--json` subcommand that lands joins
//! this file with its offline cases.
//!
//! ## Test list
//!
//! - `push_json_on_an_empty_library_prints_the_empty_summary` — exit 0, one JSON document.
//! - `push_json_without_a_session_prints_nothing_on_stdout` — a run-level failure: non-zero
//!   exit, empty stdout, the reason on stderr.
//! - `push_json_without_a_library_is_a_usage_error` — the parser's exit status 2.

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
