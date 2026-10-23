use std::fs;
use std::path::Path;

use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::verify_asset::VerifyOutcome;
use capsule_core::domain::ImportMode;
use tempfile::TempDir;

use super::*;
use crate::import::planner::{ImportConfig, plan};
use crate::import::scanner::scan;

fn noop_event(_: ImportProgressEvent) {}

/// A workspace with fast Argon2 params + its default album created, so imports have a signed
/// destination (the executor resolves an unbound context to the derived de facto album).
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
    .unwrap();
    let default = ws.default_album_id();
    ws.create_album_with_id(default, "Imports").unwrap();
    ws
}

/// **The no-codec contract.** An original whose format has no codec in this
/// build is imported as a signed, encrypted, verifiable asset — it simply arrives without a
/// thumbnail, and the run summary says so.
///
/// **The distinction is observable.** With `capsule_core::media` on `rawshift-image` the two
/// files below are told apart, and for the reason that matters rather than by extension:
/// `iphone.heic` is a format Capsule *recognises and cannot decode* (an expected,
/// backfillable gap), while `snap.jpg` is a format it can decode whose bytes are not a JPEG
/// (a real problem). Both still land as signed, encrypted, self-verifying backups, which is
/// the half that would be worst to lose.
#[test]
fn originals_with_no_codec_are_still_imported_and_signed() {
    use crate::lifecycle::DerivativeStatus;

    let src = TempDir::new().unwrap();
    let lib_dir = TempDir::new().unwrap();
    // A real ISO-BMFF `ftyp heic` header, so the classification rests on the **bytes** —
    // which is what the doc above claims. `b"fake heic bytes"` carried no `ftyp` and
    // silently exercised the extension fallback instead.
    let mut heic = vec![0, 0, 0, 0x20];
    heic.extend_from_slice(b"ftypheic");
    heic.extend_from_slice(&[0; 16]);
    fs::write(src.path().join("iphone.heic"), &heic).unwrap();
    fs::write(src.path().join("snap.jpg"), b"not really a jpeg").unwrap();

    let mut ws = signed_workspace(lib_dir.path());
    let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
    let config = ImportConfig::default();
    let plan_result = plan(&scan_result, ws.db(), &config).unwrap();

    // Admission is untouched by codec coverage.
    assert_eq!(plan_result.counts.to_import, 2);
    assert_eq!(plan_result.counts.errors, 0);

    let token = CancellationToken::new();
    let summary = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();

    assert_eq!(summary.imported_count(), 2, "both originals are backed up");
    assert_eq!(
        summary.deferred_derivative_count(),
        1,
        "the HEIC is an expected codec deferral"
    );
    assert_eq!(
        summary.decode_failed_count(),
        1,
        "the .jpg is a format we do decode, failing on these bytes — a real problem"
    );
    assert_eq!(
        summary.deferred_format_count(),
        0,
        "nothing decoded, so no per-format variant was even attempted"
    );

    // Reported per file rather than only in aggregate, so a caller reads the reason for the
    // file in front of it.
    for (path, outcome) in &summary.outcomes {
        let ImportOutcome::Imported { derivatives, .. } = outcome else {
            panic!("{} should have imported, got {outcome:?}", path.display());
        };
        let expected = if path.extension().is_some_and(|e| e == "heic") {
            DerivativeStatus::DeferredNoCodec
        } else {
            DerivativeStatus::DecodeFailed
        };
        assert_eq!(*derivatives, expected, "for {}", path.display());
    }

    // Both land on the signed path and self-verify — a missing thumbnail is not a missing
    // backup.
    let ids = ws.asset_ids();
    assert_eq!(ids.len(), 2);
    for id in &ids {
        assert_eq!(ws.verify(id).unwrap(), VerifyOutcome::Accept);
    }
}

/// A RAW-only candidate — no same-stem JPEG to fall back on — still lands as a signed,
/// self-verifying original even when no derivative can be made from it, which is why this
/// needs pinning: the archive is the whole point, the derivative is a bonus.
///
/// ARW decodes in this build (issue #589), so the deferral here is not "RAW has no decoder".
/// A Sony ARW is a TIFF container, so its *header* says TIFF and only the extension names
/// the family. This fixture is not a real ARW, so its classification rests on the extension
/// fallback, and its `DeferredNoCodec` outcome rests on `rawshift-image` reporting these
/// non-TIFF bytes as `RawError::Unsupported`, which the media layer maps to
/// `MediaError::UnsupportedFormat` rather than a decode failure.
#[test]
fn raw_only_candidate_lands_as_a_signed_original() {
    use crate::lifecycle::DerivativeStatus;

    let src = TempDir::new().unwrap();
    let lib_dir = TempDir::new().unwrap();
    fs::write(src.path().join("shot.ARW"), b"fake sony raw bytes").unwrap();

    let mut ws = signed_workspace(lib_dir.path());
    let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
    let config = ImportConfig::default();
    let plan_result = plan(&scan_result, ws.db(), &config).unwrap();
    assert_eq!(plan_result.counts.to_import, 1);
    assert_eq!(plan_result.counts.errors, 0);

    let token = CancellationToken::new();
    let summary = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();

    assert_eq!(summary.imported_count(), 1);
    assert_eq!(summary.deferred_derivative_count(), 1);
    assert_eq!(summary.decode_failed_count(), 0);
    assert!(matches!(
        summary.outcomes[0].1,
        ImportOutcome::Imported {
            derivatives: DerivativeStatus::DeferredNoCodec,
            deferred_formats: 0,
            ..
        }
    ));

    let ids = ws.asset_ids();
    assert_eq!(ids.len(), 1);
    assert_eq!(ws.verify(&ids[0]).unwrap(), VerifyOutcome::Accept);
}

#[test]
fn single_file_import_is_verify_asset_accepting() {
    let src = TempDir::new().unwrap();
    let lib_dir = TempDir::new().unwrap();
    fs::write(src.path().join("test.jpg"), b"fake jpeg content for test").unwrap();

    let mut ws = signed_workspace(lib_dir.path());
    let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
    let config = ImportConfig::default();
    let plan_result = plan(&scan_result, ws.db(), &config).unwrap();
    assert_eq!(plan_result.counts.to_import, 1);

    let token = CancellationToken::new();
    let summary = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();
    assert_eq!(summary.imported_count(), 1);

    // The imported asset lands on the signed path and self-verifies through the chokepoint.
    let ids = ws.asset_ids();
    assert_eq!(ids.len(), 1);
    assert_eq!(ws.verify(&ids[0]).unwrap(), VerifyOutcome::Accept);

    // The queryable index has exactly one visible row.
    assert_eq!(ws.db().query_timeline(0, 100).unwrap().len(), 1);
}

/// Run a Move-mode import of `src` into a fresh signed library at `lib`.
fn move_import(src: &Path, lib: &Path) -> (Workspace, ImportExecutionSummary) {
    let mut ws = signed_workspace(lib);
    let scan_result = scan(&[src.to_path_buf()]).unwrap();
    let config = ImportConfig {
        import_mode: ImportMode::Move,
        ..Default::default()
    };
    let plan_result = plan(&scan_result, ws.db(), &config).unwrap();
    let token = CancellationToken::new();
    let summary = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();
    (ws, summary)
}

/// Invariant 7: the local commit is not custody, so a Move import keeps every source and
/// reports which asset holds it, for the caller to release through the server gate.
#[test]
fn move_mode_keeps_the_source_until_the_server_holds_it() {
    let src = TempDir::new().unwrap();
    let lib_dir = TempDir::new().unwrap();
    let photo = src.path().join("move_me.jpg");
    fs::write(&photo, b"jpeg to move").unwrap();

    let (ws, summary) = move_import(src.path(), lib_dir.path());

    assert_eq!(
        fs::read(&photo).unwrap(),
        b"jpeg to move",
        "the local commit alone never deletes a Move source"
    );
    let ids = ws.asset_ids();
    assert_eq!(ws.verify(&ids[0]).unwrap(), VerifyOutcome::Accept);
    assert_eq!(summary.held_sources(), vec![(photo, ids[0])]);
}

/// A re-run over sources the first run kept finds them as duplicates and still names the
/// asset that holds each, so the release can run then.
#[test]
fn a_rerun_lists_kept_sources_as_held_by_their_existing_asset() {
    let src = TempDir::new().unwrap();
    let lib_dir = TempDir::new().unwrap();
    let photo = src.path().join("kept.jpg");
    fs::write(&photo, b"kept by the first run").unwrap();
    let (mut ws, first) = move_import(src.path(), lib_dir.path());
    let asset = first.held_sources()[0].1;

    let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
    let config = ImportConfig {
        import_mode: ImportMode::Move,
        ..Default::default()
    };
    let plan_result = plan(&scan_result, ws.db(), &config).unwrap();
    let token = CancellationToken::new();
    let again = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();

    assert_eq!(again.duplicate_count(), 1);
    assert_eq!(again.held_sources(), vec![(photo, asset)]);
}

/// The child half of [`sigkill_after_the_library_write_loses_neither_copy`]: run only when
/// that test re-executes this binary with `CAPSULE_TEST_SIGKILL_CHILD` naming the source
/// and library directories. It imports in Move mode, says so on stdout, then blocks on stdin
/// — the point between the library write and any source release — until it is killed.
#[test]
fn sigkill_child_imports_then_waits_to_be_killed() {
    let Ok(dirs) = std::env::var("CAPSULE_TEST_SIGKILL_CHILD") else {
        return;
    };
    let (src, lib) = dirs.split_once('\n').expect("source and library dirs");
    let (_ws, summary) = move_import(Path::new(src), Path::new(lib));
    assert_eq!(summary.imported_count(), 1);
    println!("CAPSULE-SIGKILL-READY");
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
}

/// The issue's crash case: a process SIGKILLed between the library write and the source
/// delete never loses both copies. The child is this test binary, re-executed to run
/// [`sigkill_child_imports_then_waits_to_be_killed`], and killed with `SIGKILL` once it has
/// written the library — so nothing in it gets to run a destructor or flush a buffer. The
/// source must be intact, and a fresh process must open the library and verify the asset.
#[cfg(unix)]
#[test]
fn sigkill_after_the_library_write_loses_neither_copy() {
    use std::io::BufRead as _;
    use std::process::{Command, Stdio};

    if std::env::var_os("CAPSULE_TEST_SIGKILL_CHILD").is_some() {
        return;
    }
    let src = TempDir::new().unwrap();
    let lib_dir = TempDir::new().unwrap();
    let photo = src.path().join("only_copy.jpg");
    fs::write(&photo, b"bytes that exist nowhere else").unwrap();

    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "import::executor::tests::sigkill_child_imports_then_waits_to_be_killed",
            "--nocapture",
            "--test-threads=1",
        ])
        .env(
            "CAPSULE_TEST_SIGKILL_CHILD",
            format!("{}\n{}", src.path().display(), lib_dir.path().display()),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = std::io::BufReader::new(child.stdout.take().unwrap());
    let ready = stdout
        .lines()
        .map_while(std::result::Result::ok)
        .any(|line| line.contains("CAPSULE-SIGKILL-READY"));
    assert!(ready, "the child never reached the post-write point");
    // `Child::kill` is SIGKILL on Unix.
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success(), "the child was killed, not finished");

    assert_eq!(
        fs::read(&photo).unwrap(),
        b"bytes that exist nowhere else",
        "the source survives the kill"
    );
    let ws = Workspace::open(
        lib_dir.path(),
        b"passphrase",
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .unwrap();
    let ids = ws.asset_ids();
    assert_eq!(ids.len(), 1, "the library copy survives the kill");
    assert_eq!(ws.verify(&ids[0]).unwrap(), VerifyOutcome::Accept);
}

#[test]
fn cancellation_stops_execution() {
    let src = TempDir::new().unwrap();
    let lib_dir = TempDir::new().unwrap();
    for i in 0..3 {
        fs::write(
            src.path().join(format!("photo_{i}.jpg")),
            format!("content_{i}").as_bytes(),
        )
        .unwrap();
    }

    let mut ws = signed_workspace(lib_dir.path());
    let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
    let config = ImportConfig::default();
    let plan_result = plan(&scan_result, ws.db(), &config).unwrap();

    let token = CancellationToken::new();
    token.cancel(); // Cancel before starting.

    let summary = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();
    assert_eq!(
        summary.outcomes.len(),
        0,
        "no files imported after immediate cancellation"
    );
    assert_eq!(ws.asset_ids().len(), 0);
}

#[test]
fn raw_jpeg_stack_import_hides_secondary() {
    let src = TempDir::new().unwrap();
    let lib_dir = TempDir::new().unwrap();
    fs::write(src.path().join("img_0001.jpg"), b"jpeg content").unwrap();
    fs::write(src.path().join("img_0001.ARW"), b"raw content").unwrap();

    let mut ws = signed_workspace(lib_dir.path());
    let scan_result = scan(&[src.path().to_path_buf()]).unwrap();
    assert_eq!(
        scan_result.candidates.len(),
        1,
        "should form a single stack candidate"
    );

    let config = ImportConfig::default();
    let plan_result = plan(&scan_result, ws.db(), &config).unwrap();
    let token = CancellationToken::new();
    let summary = execute(&plan_result, &mut ws, &config, noop_event, &token).unwrap();

    assert_eq!(summary.imported_count(), 2, "both RAW and JPEG imported");
    // Both members verify on the signed path.
    for id in ws.asset_ids() {
        assert_eq!(ws.verify(&id).unwrap(), VerifyOutcome::Accept);
    }
    // Only the primary is visible in the timeline (the RAW is stack-hidden).
    assert_eq!(
        ws.db().query_timeline(0, 100).unwrap().len(),
        1,
        "only the primary member is visible"
    );

    // The grouping is *durable* — every member carries the signed `stack_membership`
    // register, sharing one stack id, with exactly one primary, rather than only index
    // columns.
    let mut stack_ids = Vec::new();
    let mut primaries = 0;
    for id in ws.asset_ids() {
        let membership = ws
            .asset(&id)
            .unwrap()
            .sidecar
            .stack_membership
            .get()
            .and_then(Option::as_ref)
            .expect("the executor wrote the stack register")
            .clone();
        assert_eq!(membership.stack_type, StackType::RawJpeg);
        primaries += usize::from(membership.role == StackRole::Primary);
        stack_ids.push(membership.stack_id);
    }
    assert_eq!(primaries, 1, "exactly one member represents the stack");
    stack_ids.dedup();
    assert_eq!(stack_ids.len(), 1, "both members share one stack id");
}
