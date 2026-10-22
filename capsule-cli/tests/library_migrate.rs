//! `capsule library migrate` (issue `#456`, over slice `S-D24`) across a real process
//! boundary, by spawning the `capsule` binary the way `show_and_repair.rs` does: the verb's
//! whole point is what a library *reopened from disk* holds afterwards.
//!
//! **The pre-signed-path library** is reproduced by writing, into an initialized library's
//! `media/1970/1970-01` bucket, each asset's original `{uuid}.jpg` beside an unsigned
//! `{uuid}.cbor` holding the retired flat map (text keys, `version: 1`) — the shape the core
//! verb's own tests write, encoded here through `capsule_core::cbor` from a JSON map so the
//! CLI needs no CBOR dependency of its own. The Argon2id seeding is the fast-cost trick
//! `import_round_trip.rs` explains.
//!
//! ## Test list
//!
//! - `migrate_admits_the_legacy_assets_and_reports_every_outcome` — before the run, a
//!   workspace-opening verb notes the unmigrated count on stderr; the run migrates a plain and
//!   a deleted asset (the latter into trash), refuses a hash-mismatched one with its reason,
//!   prints the summary, and a reopened workspace sees exactly the two admitted assets; a
//!   second run is a no-op that still reports the refusal; once the bad
//!   sidecar is gone the run reports nothing to migrate and the notice is silent.
//! - `retain_days_is_a_non_negative_count` — the parser refuses `--retain-days -1`.

use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use capsule_core::crypto::hash;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::lifecycle::Workspace;
use serde_json::{Value, json};
use uuid::Uuid;

const PASSPHRASE: &str = "library-migrate-passphrase";

const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

// ── Scratch plumbing ─────────────────────────────────────────────────────────

struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-cli-migrate-{}", nanoid::nanoid!()));
        std::fs::create_dir_all(&path).expect("create scratch dir");
        Self(path)
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

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

/// `(stdout, stderr)` of one successful `capsule` run.
fn capsule(home: &Path, args: &[&str]) -> (String, String) {
    let out = spawn(home, args);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "capsule {args:?} failed\nstdout:\n{stdout}\nstderr:\n{stderr}"
    );
    (stdout, stderr)
}

fn path(p: &Path) -> &str {
    p.to_str().expect("scratch paths are UTF-8")
}

// ── The legacy library ───────────────────────────────────────────────────────

/// Write one legacy asset into `media/1970/1970-01`: the original and its unsigned sidecar,
/// recording `recorded_hash_of`'s SHA-256 (the original itself, unless a test wants a
/// mismatch) and `is_deleted`.
fn write_legacy(
    library: &Path,
    asset_id: Uuid,
    original: &[u8],
    recorded_hash_of: &[u8],
    is_deleted: bool,
) -> PathBuf {
    let dir = library.join("media/1970/1970-01");
    std::fs::create_dir_all(&dir).expect("create bucket");
    std::fs::write(dir.join(format!("{}.jpg", asset_id.simple())), original).expect("original");
    let record: BTreeMap<&str, Value> = BTreeMap::from([
        ("version", json!(1)),
        ("uuid", json!(asset_id.to_string())),
        ("asset_type", json!("photo")),
        ("original_filename", json!("IMG_0001.JPG")),
        ("import_timestamp", json!(1_720_000_000)),
        ("modified_timestamp", json!(1_720_000_000)),
        (
            "hash_sha256",
            json!(hash::hash_bytes(recorded_hash_of).to_hex()),
        ),
        ("file_size", json!(original.len())),
        ("is_deleted", json!(is_deleted)),
        ("rating", json!(0)),
        ("tags", json!([])),
        ("import_mode", json!("copy")),
        ("importer_version", json!("0.1.0")),
        ("rawshift_version", json!("0.1.0")),
    ]);
    let bytes = capsule_core::cbor::to_canonical_vec(&record).expect("encode legacy record");
    let sidecar = dir.join(format!("{}.cbor", asset_id.simple()));
    std::fs::write(&sidecar, bytes).expect("legacy sidecar");
    sidecar
}

struct Fixture {
    _scratch: ScratchDir,
    home: PathBuf,
    library: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let scratch = ScratchDir::new();
        let home = scratch.0.join("home");
        let library = scratch.0.join("library");
        std::fs::create_dir_all(&home).expect("create scratch home");
        let (out, _) = capsule(
            &home,
            &["library", "init", path(&library), "--name", "Legacy"],
        );
        assert!(out.contains("Library created at"), "stdout:\n{out}");
        drop(Workspace::open(&library, PASSPHRASE.as_bytes(), FAST_KDF).expect("seed account"));
        Self {
            _scratch: scratch,
            home,
            library,
        }
    }

    fn migrate(&self) -> (String, String) {
        capsule(
            &self.home,
            &[
                "library",
                "migrate",
                path(&self.library),
                "--passphrase-stdin",
            ],
        )
    }

    /// Any workspace-opening verb: `capsule cull` with no flags only prints the view.
    fn cull_stderr(&self) -> String {
        capsule(
            &self.home,
            &[
                "cull",
                "--library",
                path(&self.library),
                "--passphrase-stdin",
            ],
        )
        .1
    }

    fn reopen(&self) -> Workspace {
        Workspace::open(&self.library, PASSPHRASE.as_bytes(), FAST_KDF).expect("reopen")
    }
}

#[test]
fn migrate_admits_the_legacy_assets_and_reports_every_outcome() {
    let fx = Fixture::new();
    let plain = Uuid::from_u128(0xA1);
    let deleted = Uuid::from_u128(0xA2);
    let corrupt = Uuid::from_u128(0xA3);
    write_legacy(
        &fx.library,
        plain,
        b"\xFF\xD8\xFF plain legacy asset",
        b"\xFF\xD8\xFF plain legacy asset",
        false,
    );
    write_legacy(
        &fx.library,
        deleted,
        b"\xFF\xD8\xFF deleted legacy asset",
        b"\xFF\xD8\xFF deleted legacy asset",
        true,
    );
    let corrupt_sidecar = write_legacy(
        &fx.library,
        corrupt,
        b"\xFF\xD8\xFF corrupt legacy asset",
        b"the bytes the record was written for",
        false,
    );

    // Before the run: every verb that opens the workspace says what is waiting, and where.
    let notice = fx.cull_stderr();
    assert!(
        notice.contains("3 sidecars in this library are unsigned")
            && notice.contains("capsule library migrate"),
        "stderr:\n{notice}"
    );

    let (out, err) = fx.migrate();
    assert!(
        !err.contains("are unsigned"),
        "the verb does not tell the user to run itself:\n{err}"
    );
    assert!(out.contains("(3 found)"), "stdout:\n{out}");
    assert!(out.contains(&format!("Migrated {plain}")), "stdout:\n{out}");
    assert!(
        out.contains(&format!("Migrated {deleted}")),
        "stdout:\n{out}"
    );
    assert!(
        out.contains(&format!("Moved {deleted} to trash")),
        "stdout:\n{out}"
    );
    assert!(
        out.contains(&format!("Skipped {}", corrupt_sidecar.display()))
            && out.contains(&format!("asset {corrupt}: the original's SHA-256 is")),
        "stdout:\n{out}"
    );
    assert!(
        out.contains(
            "Migration: 2 migrated, 1 moved to trash, 0 stack(s), 1 skipped, 1 still unmigrated."
        ),
        "stdout:\n{out}"
    );
    assert!(
        out.contains("Migrating the unsigned sidecars in "),
        "the in-progress line:\n{out}"
    );

    // What a reopened library holds: the two admitted assets (the legacy records carry no
    // album id, so they landed in the default album the open resolved — the core verb refuses
    // before writing anything when its fallback album does not exist), the deleted one in
    // trash, and the refused one still waiting.
    let ws = fx.reopen();
    let mut ids = ws.asset_ids();
    ids.sort();
    assert_eq!(ids, vec![plain, deleted]);
    assert!(!ws.is_trashed(&plain));
    assert!(ws.is_trashed(&deleted));
    assert_eq!(ws.unmigrated_sidecars().len(), 1);
    assert_eq!(ws.unmigrated_sidecars()[0].asset_id, Some(corrupt));
    drop(ws);

    // Idempotent: a second run admits nothing new and still reports the refusal.
    let (out, _) = fx.migrate();
    assert!(
        out.contains(
            "Migration: 0 migrated, 0 moved to trash, 0 stack(s), 1 skipped, 1 still unmigrated."
        ),
        "stdout:\n{out}"
    );
    assert!(!out.contains("Migrated "), "stdout:\n{out}");

    // With the bad sidecar gone there is nothing left, and the notice goes quiet.
    std::fs::remove_file(&corrupt_sidecar).expect("remove the corrupt sidecar");
    let (out, _) = fx.migrate();
    assert!(
        out.contains("No unsigned sidecars to migrate."),
        "stdout:\n{out}"
    );
    let notice = fx.cull_stderr();
    assert!(!notice.contains("unsigned"), "stderr:\n{notice}");
}

#[test]
fn retain_days_is_a_non_negative_count() {
    let fx = Fixture::new();
    let out = spawn(
        &fx.home,
        &[
            "library",
            "migrate",
            path(&fx.library),
            "--passphrase-stdin",
            "--retain-days=-1",
        ],
    );
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("invalid value '-1' for '--retain-days"),
        "stderr:\n{stderr}"
    );
}
