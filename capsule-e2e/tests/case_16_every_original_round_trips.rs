//! **E2E case 16** — every original round-trips byte for byte, whatever the codecs support
//! (issue #695; SSoT: thumbnails § Media Is Rawshift's).
//!
//! A fixture corpus with one file per kind no codec in the build handles, or that Capsule does
//! not recognise at all — three video containers, a HEIC, a RAW, a document, an unknown suffix,
//! a file with no extension, a zero-byte file and a file spanning several STREAM chunks and
//! several restore windows — goes through the real pipeline end to end:
//!
//! 1. **Import** through the scanner, planner and executor `capsule import` runs: every file
//!    is admitted and imported, and the run reports which arrived without derivatives.
//! 2. **Push** through the CLI's own orchestration (`capsule_cli::remote::push`) against the
//!    real server composition root: every asset reaches verified custody.
//! 3. **Restore** through `capsule_cli::remote::restore` into an empty directory: every
//!    original comes back, and its bytes equal the source file's.

use std::collections::BTreeMap;
use std::path::PathBuf;

use capsule_cli::remote::{self, AssetPushStatus, PushOptions, RemoteConfig};
use capsule_cli::restore::{
    AssetRestoreStatus, RESTORE_RANGE_WINDOW, RestoreOptions, RestoreScope,
};
use capsule_cli::session::SessionStore;
use capsule_core::import::{
    CancellationToken, ImportConfig, ImportOutcome, execute, plan, scan_paths,
};
use capsule_core::lifecycle::DerivativeStatus;
use capsule_e2e::{Device, PROTOCOL_VERSION, Server};
use capsule_sdk::net::ConnectionClass;

/// Bytes that start with `head` and continue with `len` bytes of a pattern seeded by `seed`, so
/// every fixture is distinct (the planner would otherwise fold equal files into duplicates).
fn bytes(head: &[u8], seed: u8, len: usize) -> Vec<u8> {
    let mut out = head.to_vec();
    out.extend((0..len).map(|i| (i as u8).wrapping_mul(37).wrapping_add(seed) ^ (i >> 11) as u8));
    out
}

/// One file per kind the build cannot derive from, or does not recognise. Each entry is the
/// file name, its bytes, and the derivative status the import must report for it.
fn corpus() -> Vec<(&'static str, Vec<u8>, DerivativeStatus)> {
    use DerivativeStatus::{DeferredNoCodec, NotAKnownStill};
    vec![
        // Video: kept, derivatives deferred until video codecs land.
        (
            "clip.mp4",
            bytes(b"\0\0\0\x18ftypmp42\0\0\0\0mp42isom", 1, 200_000),
            DeferredNoCodec,
        ),
        (
            "clip.mov",
            bytes(b"\0\0\0\x14ftypqt  \0\0\0\0qt  ", 2, 150_000),
            DeferredNoCodec,
        ),
        (
            "clip.mkv",
            bytes(b"\x1A\x45\xDF\xA3\x9F\x42\x86\x81\x01", 3, 90_000),
            DeferredNoCodec,
        ),
        // A still whose codec is not in the build (HEIC decode is refused outright).
        (
            "iphone.heic",
            bytes(b"\0\0\0\x18ftypheic\0\0\0\0heicmif1", 4, 70_000),
            DeferredNoCodec,
        ),
        // A RAW whose bytes this build cannot develop.
        (
            "shot.ARW",
            bytes(b"fake sony raw", 5, 60_000),
            DeferredNoCodec,
        ),
        // Not media at all, an unknown suffix, and no suffix: nothing to derive.
        ("scan.pdf", bytes(b"%PDF-1.7\n", 6, 40_000), NotAKnownStill),
        (
            "notes.xyz",
            bytes(b"unknown kind ", 7, 1_000),
            NotAKnownStill,
        ),
        ("README", bytes(b"no extension ", 8, 500), NotAKnownStill),
        // Zero bytes is an original like any other.
        ("empty.dat", Vec::new(), NotAKnownStill),
        // Many STREAM chunks and more than two restore windows.
        (
            "huge.bin",
            bytes(b"large ", 9, 2 * RESTORE_RANGE_WINDOW as usize + 12_345),
            NotAKnownStill,
        ),
    ]
}

#[tokio::test]
async fn e2e_case_16_every_original_round_trips_whatever_the_codecs_support() {
    let server = Server::boot().await;
    let mut device = Device::register(&server, "every-original").await;

    // The corpus, written where a user's files would be.
    let source = device.staging.path().join("corpus");
    std::fs::create_dir_all(&source).expect("the corpus directory");
    let corpus = corpus();
    for (name, contents, _) in &corpus {
        std::fs::write(source.join(name), contents).expect("the fixture writes");
    }

    // 1. Import: the same scan → plan → execute `capsule import` runs.
    let scanned = scan_paths(std::slice::from_ref(&source)).expect("the corpus scans");
    assert_eq!(
        scanned.total_files(),
        corpus.len(),
        "every file is a candidate"
    );
    let config = ImportConfig::default();
    let planned = plan(&scanned, device.workspace.db(), &config).expect("the corpus plans");
    assert_eq!(
        planned.counts.to_import,
        corpus.len(),
        "{:?}",
        planned.counts
    );
    assert_eq!(planned.counts.errors, 0);
    let summary = execute(
        &planned,
        &mut device.workspace,
        &config,
        |_| {},
        &CancellationToken::new(),
    )
    .expect("the import runs");

    assert_eq!(
        summary.imported_count(),
        corpus.len(),
        "{:#?}",
        summary.outcomes
    );
    assert_eq!(summary.error_count(), 0);
    let deferred = corpus
        .iter()
        .filter(|(_, _, status)| *status == DerivativeStatus::DeferredNoCodec)
        .count();
    assert_eq!(
        summary.deferred_derivative_count(),
        deferred,
        "every video, HEIC and RAW is reported as deferred"
    );

    // Which asset holds which source file, and what the import said about its derivatives.
    let mut by_asset: BTreeMap<String, (PathBuf, Vec<u8>)> = BTreeMap::new();
    for (path, outcome) in &summary.outcomes {
        let ImportOutcome::Imported {
            asset_id,
            derivatives,
            ..
        } = outcome
        else {
            panic!("{} did not import: {outcome:?}", path.display());
        };
        let name = path.file_name().expect("a file name").to_string_lossy();
        let (_, contents, expected) = corpus
            .iter()
            .find(|(fixture, _, _)| *fixture == name)
            .expect("every outcome is a corpus file");
        assert_eq!(derivatives, expected, "derivatives for {name}");
        by_asset.insert(asset_id.to_string(), (path.clone(), contents.clone()));
    }
    assert_eq!(by_asset.len(), corpus.len());

    // 2. Push: the CLI's orchestration over the session `capsule auth login` persists.
    let signed_in = device.login_again(&server).await;
    let home = tempfile::tempdir().expect("a temp CLI home");
    let store = SessionStore::new(home.path().join("session.json"));
    store
        .save(&signed_in.export().await.expect("a live session exports"))
        .expect("the session persists");
    let remote = RemoteConfig {
        auth_endpoint: server.auth_base(),
        sync_endpoint: server.base_url().to_owned(),
        upload_endpoint: server.upload_base(),
        albums_endpoint: server.albums_base(),
        protocol_version: PROTOCOL_VERSION.to_owned(),
    };
    let pushed = remote::push(&remote, &store, &device.workspace, PushOptions::default())
        .await
        .expect("the push runs");
    assert_eq!(pushed.assets, corpus.len());
    assert_eq!(pushed.verified_assets, corpus.len(), "{pushed:#?}");
    assert!(
        pushed
            .outcomes
            .iter()
            .all(|o| o.status == AssetPushStatus::Verified),
        "{pushed:#?}"
    );

    // 3. Restore into an empty directory, and compare every original with its source.
    let out = tempfile::tempdir().expect("a restore directory");
    let options = RestoreOptions {
        scope: RestoreScope::Library,
        out: out.path().to_path_buf(),
        connection: ConnectionClass::Unmetered,
        range_window: RESTORE_RANGE_WINDOW,
    };
    let restored = remote::restore(&remote, &store, &device.workspace, &options)
        .await
        .expect("the restore runs");
    assert_eq!(restored.assets, corpus.len(), "{restored:#?}");
    assert_eq!(restored.restored, corpus.len(), "{restored:#?}");
    assert_eq!(restored.failed, 0, "{restored:#?}");

    for outcome in &restored.outcomes {
        assert_eq!(outcome.status, AssetRestoreStatus::Restored, "{outcome:#?}");
        let (source_path, contents) = by_asset
            .get(&outcome.asset_id)
            .expect("every restored asset was imported from the corpus");
        let written = std::fs::read(outcome.path.as_ref().expect("a restored path"))
            .expect("the restored original reads");
        assert!(
            written == *contents,
            "{} did not restore byte for byte ({} bytes in, {} out)",
            source_path.display(),
            contents.len(),
            written.len()
        );
    }
}

/// The original types the client declares by name must stay inside the server's default closed
/// enum, or an original the client thinks is declarable would be refused at upload.
#[test]
fn e2e_case_16_every_declared_original_type_is_in_the_servers_enum() {
    use capsule_core::lifecycle::{OPAQUE_CONTENT_TYPE, WIRE_ORIGINAL_CONTENT_TYPES};
    use capsule_server::upload::policy::DEFAULT_CONTENT_TYPES;

    for kind in WIRE_ORIGINAL_CONTENT_TYPES
        .iter()
        .chain(std::iter::once(&OPAQUE_CONTENT_TYPE))
    {
        assert!(
            DEFAULT_CONTENT_TYPES.contains(kind),
            "{kind} is declared by the client but refused by the server"
        );
    }
}
