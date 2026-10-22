use std::collections::HashMap;
use std::sync::Mutex;

use capsule_core::crypto::primitives::Argon2Params;
use capsule_sdk::sync::{BlobManifest, BlobRef};

use super::*;

/// A fast Argon2id cost: these tests exercise the restore, not the unlock.
const FAST_KDF: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// A directory removed when the test ends.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "capsule-cli-restore-{name}-{}-{}",
            std::process::id(),
            Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&path).expect("a scratch directory");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An original big enough to span several STREAM chunks and several bounded windows.
fn original_bytes(seed: u8, len: usize) -> Vec<u8> {
    let mut bytes = b"\xFF\xD8\xFF restore fixture ".to_vec();
    bytes.extend((0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed)));
    bytes
}

/// A library holding `count` imported assets in one album, and the feed entries and blobs a
/// server holding them would serve.
struct Fixture {
    _dirs: (Scratch, Scratch),
    workspace: Workspace,
    album: Uuid,
    assets: Vec<Uuid>,
    entries: Vec<FeedEntry>,
    blobs: HashMap<String, Vec<u8>>,
}

fn fixture(name: &str, count: u8) -> Fixture {
    let lib = Scratch::new(&format!("{name}-lib"));
    let src = Scratch::new(&format!("{name}-src"));
    let mut workspace = Workspace::create_with_params(lib.path(), b"passphrase", FAST_KDF)
        .expect("a fresh library");
    let album = workspace.create_album("Trip").expect("an album");
    let mut assets = Vec::new();
    let mut entries = Vec::new();
    let mut blobs = HashMap::new();
    for i in 0..count {
        let path = src.path().join(format!("photo-{i}.jpg"));
        std::fs::write(&path, original_bytes(i, 150_000)).expect("the source writes");
        let asset = workspace.import_asset(album, &path).expect("the import");
        let bundle = workspace.upload_bundle(&asset).expect("a bundle");
        let original_hash = bundle.ciphertext_hash.to_hex();
        let metadata_hash = bundle
            .metadata_blob_hash
            .expect("a create binds metadata")
            .to_hex();
        entries.push(FeedEntry {
            album_id: album.to_string().into_bytes(),
            sync_seq: u64::from(i) + 1,
            protocol_version: "2026-12-31".to_owned(),
            kind: ChangeKind::Created,
            asset_id: asset.to_string().into_bytes(),
            manifest_cbor: bundle.provenance_blob.clone(),
            metadata_blob: metadata_hash.clone().into_bytes(),
            blobs: BlobManifest {
                original: Some(BlobRef {
                    ciphertext_hash: original_hash.clone(),
                    role: "original".to_owned(),
                    size: bundle.ciphertext_size(),
                }),
                derivatives: vec![BlobRef {
                    ciphertext_hash: metadata_hash.clone(),
                    role: "metadata".to_owned(),
                    size: bundle.metadata_blob.len() as u64,
                }],
            },
            derivative_manifests: bundle.derivative_manifests.clone(),
            original_held: true,
            changed_at: "2026-10-21T00:00:00Z".to_owned(),
        });
        blobs.insert(original_hash, bundle.original.to_vec().unwrap());
        blobs.insert(metadata_hash, bundle.metadata_blob.clone());
        assets.push(asset);
    }
    Fixture {
        _dirs: (lib, src),
        workspace,
        album,
        assets,
        entries,
        blobs,
    }
}

/// A server: serves each blob's requested range whole, and records every request.
struct MockSource {
    blobs: HashMap<String, Vec<u8>>,
    requests: Mutex<Vec<(String, u64)>>,
    /// When set, a request for this blob that starts below the cut answers only up to it, and
    /// the next one fails on the wire: the connection drops mid-blob.
    cut: Option<(String, u64)>,
}

impl MockSource {
    fn new(blobs: HashMap<String, Vec<u8>>) -> Self {
        Self {
            blobs,
            requests: Mutex::new(Vec::new()),
            cut: None,
        }
    }

    fn requests_for(&self, hash: &str) -> Vec<u64> {
        self.requests
            .lock()
            .expect("the request log")
            .iter()
            .filter(|(h, _)| h == hash)
            .map(|(_, start)| *start)
            .collect()
    }

    fn request_count(&self) -> usize {
        self.requests.lock().expect("the request log").len()
    }
}

impl BlobSource for MockSource {
    async fn get_range(&self, hash: &str, start: u64, max_len: Option<u64>) -> RangeOutcome {
        self.requests
            .lock()
            .expect("the request log")
            .push((hash.to_owned(), start));
        let Some(blob) = self.blobs.get(hash) else {
            return RangeOutcome::Status {
                status: 404,
                code: None,
            };
        };
        let len = blob.len() as u64;
        let mut end = max_len.map_or(len, |window| (start + window).min(len));
        if let Some((cut_hash, cut_at)) = &self.cut
            && cut_hash == hash
        {
            if start >= *cut_at {
                return RangeOutcome::Status {
                    status: 0,
                    code: None,
                };
            }
            end = end.min(*cut_at);
            return RangeOutcome::Partial {
                bytes: blob[start as usize..end as usize].to_vec(),
            };
        }
        RangeOutcome::Complete {
            bytes: blob[start as usize..end as usize].to_vec(),
        }
    }
}

fn options(out: &Path, scope: RestoreScope) -> RestoreOptions {
    RestoreOptions {
        scope,
        out: out.to_path_buf(),
        connection: ConnectionClass::Unmetered,
        range_window: RESTORE_RANGE_WINDOW,
    }
}

/// A server reached the way [`capsule_sdk::fetch::HttpBlobSource`] reaches it: each `Range` is
/// read whole and answered `Complete`, and a response whose body drops hands over nothing
/// (`Partial` with no bytes). After `windows` answered requests for `drop_hash`, the next body
/// drops and every later request finds the network gone (a transport error).
struct HttpLikeSource {
    blobs: HashMap<String, Vec<u8>>,
    requests: Mutex<Vec<(String, u64, Option<u64>)>>,
    drop_after: Option<(String, usize)>,
}

impl HttpLikeSource {
    fn new(blobs: HashMap<String, Vec<u8>>) -> Self {
        Self {
            blobs,
            requests: Mutex::new(Vec::new()),
            drop_after: None,
        }
    }

    fn requests_for(&self, hash: &str) -> Vec<(u64, Option<u64>)> {
        self.requests
            .lock()
            .expect("the request log")
            .iter()
            .filter(|(h, _, _)| h == hash)
            .map(|(_, start, max_len)| (*start, *max_len))
            .collect()
    }
}

impl BlobSource for HttpLikeSource {
    async fn get_range(&self, hash: &str, start: u64, max_len: Option<u64>) -> RangeOutcome {
        let answered = {
            let mut log = self.requests.lock().expect("the request log");
            let answered = log.iter().filter(|(h, _, _)| h == hash).count();
            log.push((hash.to_owned(), start, max_len));
            answered
        };
        let Some(blob) = self.blobs.get(hash) else {
            return RangeOutcome::Status {
                status: 404,
                code: None,
            };
        };
        if let Some((drop_hash, windows)) = &self.drop_after
            && drop_hash == hash
        {
            if answered == *windows {
                return RangeOutcome::Partial { bytes: Vec::new() };
            }
            if answered > *windows {
                return RangeOutcome::Status {
                    status: 0,
                    code: None,
                };
            }
        }
        let len = blob.len() as u64;
        let end = max_len.map_or(len, |window| (start + window).min(len));
        RangeOutcome::Complete {
            bytes: blob[start as usize..end as usize].to_vec(),
        }
    }
}

/// A restore never asks for the open-ended remainder: whatever the class, a window bounds it,
/// and `adverse` keeps its own smaller one.
#[test]
fn a_restore_always_asks_for_bounded_windows() {
    let mut options = options(Path::new("out"), RestoreScope::Library);
    assert_eq!(options.window(), RESTORE_RANGE_WINDOW);
    options.connection = ConnectionClass::Adverse;
    assert_eq!(
        options.window(),
        capsule_sdk::net::ADVERSE_RANGE_WINDOW.min(RESTORE_RANGE_WINDOW)
    );
    options.connection = ConnectionClass::Unmetered;
    options.range_window = 0;
    assert_eq!(options.window(), 1, "a zero window is never open-ended");
}

/// The resume property over the production source's semantics: an `unmetered` run whose
/// connection drops mid-original keeps every whole window that landed, and the next run asks
/// only for the bytes after them. Time is paused, so the fetcher's backoff after the dropped
/// body costs nothing.
#[tokio::test(start_paused = true)]
async fn an_interrupted_restore_resumes_over_whole_window_responses() {
    const WINDOW: u64 = 32 * 1024;
    let fx = fixture("http-resume", 1);
    let out = Scratch::new("http-resume-out");
    let asset = fx.assets[0];
    let original = fx.entries[0].blobs.original.clone().expect("an original");
    assert!(
        original.size > 3 * WINDOW,
        "the original spans several windows"
    );
    let mut run_options = options(out.path(), RestoreScope::Library);
    run_options.range_window = WINDOW;

    let mut dropping = HttpLikeSource::new(fx.blobs.clone());
    dropping.drop_after = Some((original.ciphertext_hash.clone(), 2));
    let first = restore_entries(&fx.workspace, &dropping, fx.entries.clone(), &run_options)
        .await
        .expect("the run completes");
    let failed = outcome_of(&first, asset);
    assert_eq!(failed.status, AssetRestoreStatus::Failed, "{first:#?}");
    assert_eq!(failed.fetched_bytes, 2 * WINDOW);
    assert!(
        dropping
            .requests_for(&original.ciphertext_hash)
            .iter()
            .all(|(_, max_len)| *max_len == Some(WINDOW)),
        "an unmetered restore still asks for bounded windows"
    );
    let part = out
        .path()
        .join(STAGING_DIR)
        .join(format!("{}.part", original.ciphertext_hash));
    assert_eq!(
        std::fs::metadata(&part)
            .expect("the part file is kept")
            .len(),
        2 * WINDOW,
        "every whole window that landed is on disk"
    );

    let resumed = HttpLikeSource::new(fx.blobs.clone());
    let second = restore_entries(&fx.workspace, &resumed, fx.entries.clone(), &run_options)
        .await
        .expect("the run completes");
    let restored = outcome_of(&second, asset);
    assert_eq!(restored.status, AssetRestoreStatus::Restored, "{second:#?}");
    assert_eq!(
        resumed
            .requests_for(&original.ciphertext_hash)
            .first()
            .copied(),
        Some((2 * WINDOW, Some(WINDOW))),
        "the first request for the original starts where the part file ends"
    );
    assert_eq!(restored.resumed_bytes, 2 * WINDOW);
    assert_eq!(restored.fetched_bytes, original.size - 2 * WINDOW);
    assert_eq!(
        restored_bytes(restored),
        fx.workspace
            .read_plaintext(&asset)
            .expect("the library's copy")
    );
    assert!(!part.exists(), "a verified blob's part file is removed");
}

fn outcome_of(summary: &RestoreSummary, asset: Uuid) -> &AssetRestoreOutcome {
    summary
        .outcomes
        .iter()
        .find(|outcome| outcome.asset_id == asset.to_string())
        .expect("every asset in scope has an outcome")
}

fn restored_bytes(outcome: &AssetRestoreOutcome) -> Vec<u8> {
    std::fs::read(
        outcome
            .path
            .as_ref()
            .expect("a restored asset names its file"),
    )
    .expect("the restored file reads")
}

/// The acceptance property: every original comes back byte for byte, written into the output
/// directory under the asset's id with nothing left in staging.
#[tokio::test]
async fn a_restore_writes_byte_identical_originals() {
    let fx = fixture("identical", 2);
    let out = Scratch::new("identical-out");
    let source = MockSource::new(fx.blobs.clone());

    let summary = restore_entries(
        &fx.workspace,
        &source,
        fx.entries.clone(),
        &options(out.path(), RestoreScope::Library),
    )
    .await
    .expect("the run completes");

    assert_eq!(summary.assets, 2);
    assert_eq!(summary.restored, 2, "{summary:#?}");
    assert_eq!(failed_run_reason(&summary), None);
    for asset in &fx.assets {
        let outcome = outcome_of(&summary, *asset);
        assert_eq!(outcome.status, AssetRestoreStatus::Restored);
        assert_eq!(
            restored_bytes(outcome),
            fx.workspace
                .read_plaintext(asset)
                .expect("the library's copy"),
            "the restored original is byte-identical"
        );
        assert!(
            outcome
                .path
                .as_deref()
                .is_some_and(|path| path.ends_with(&format!("{asset}.jpg")))
        );
    }
    let staged: Vec<_> = std::fs::read_dir(out.path().join(STAGING_DIR))
        .expect("the staging directory exists")
        .collect();
    assert!(
        staged.is_empty(),
        "no part or temporary file is left behind"
    );
}

/// The resume property: a run cut off mid-original leaves the bytes it received in a part
/// file, and the next run asks the server only for the bytes after them.
#[tokio::test]
async fn an_interrupted_restore_resumes_without_refetching_held_bytes() {
    let fx = fixture("resume", 1);
    let out = Scratch::new("resume-out");
    let asset = fx.assets[0];
    let original = fx.entries[0].blobs.original.clone().expect("an original");
    let cut_at = original.size / 3;

    let mut dropping = MockSource::new(fx.blobs.clone());
    dropping.cut = Some((original.ciphertext_hash.clone(), cut_at));
    let first = restore_entries(
        &fx.workspace,
        &dropping,
        fx.entries.clone(),
        &options(out.path(), RestoreScope::Library),
    )
    .await
    .expect("the run completes");
    assert_eq!(first.failed, 1);
    let failed = outcome_of(&first, asset);
    assert_eq!(failed.error.as_ref().expect("a reason").kind, "fetch");
    assert_eq!(failed.fetched_bytes, cut_at);
    let part = out
        .path()
        .join(STAGING_DIR)
        .join(format!("{}.part", original.ciphertext_hash));
    assert_eq!(
        std::fs::metadata(&part)
            .expect("the part file is kept")
            .len(),
        cut_at
    );
    assert!(
        failed_run_reason(&first).is_some(),
        "the interrupted run fails"
    );

    let resumed = MockSource::new(fx.blobs.clone());
    let second = restore_entries(
        &fx.workspace,
        &resumed,
        fx.entries.clone(),
        &options(out.path(), RestoreScope::Library),
    )
    .await
    .expect("the run completes");
    let restored = outcome_of(&second, asset);
    assert_eq!(restored.status, AssetRestoreStatus::Restored, "{second:#?}");
    assert_eq!(
        resumed.requests_for(&original.ciphertext_hash),
        vec![cut_at],
        "the only request for the original starts where the part file ends"
    );
    assert_eq!(restored.resumed_bytes, cut_at);
    assert_eq!(restored.fetched_bytes, original.size - cut_at);
    assert_eq!(
        restored_bytes(restored),
        fx.workspace
            .read_plaintext(&asset)
            .expect("the library's copy")
    );
    assert!(!part.exists(), "a verified blob's part file is removed");
}

/// A restored asset is in place: the next run reports it and sends no request at all.
#[tokio::test]
async fn a_restored_asset_is_not_fetched_again() {
    let fx = fixture("again", 1);
    let out = Scratch::new("again-out");
    let first = MockSource::new(fx.blobs.clone());
    restore_entries(
        &fx.workspace,
        &first,
        fx.entries.clone(),
        &options(out.path(), RestoreScope::Library),
    )
    .await
    .expect("the first run completes");

    let second = MockSource::new(fx.blobs.clone());
    let summary = restore_entries(
        &fx.workspace,
        &second,
        fx.entries.clone(),
        &options(out.path(), RestoreScope::Library),
    )
    .await
    .expect("the second run completes");
    assert_eq!(summary.already_present, 1);
    assert_eq!(summary.restored, 0);
    assert_eq!(second.request_count(), 0, "nothing crosses the wire");
    assert_eq!(failed_run_reason(&summary), None);
}

/// `--asset` restores that asset alone; `--album` restores its album and nothing else.
#[tokio::test]
async fn the_scope_selects_what_is_restored() {
    let fx = fixture("scope", 2);
    let out = Scratch::new("scope-out");
    let source = MockSource::new(fx.blobs.clone());
    let one = restore_entries(
        &fx.workspace,
        &source,
        fx.entries.clone(),
        &options(out.path(), RestoreScope::Asset(fx.assets[1])),
    )
    .await
    .expect("the run completes");
    assert_eq!(one.assets, 1);
    assert_eq!(one.outcomes[0].asset_id, fx.assets[1].to_string());

    let elsewhere = Scratch::new("scope-out-2");
    let none = restore_entries(
        &fx.workspace,
        &source,
        fx.entries.clone(),
        &options(elsewhere.path(), RestoreScope::Album(Uuid::now_v7())),
    )
    .await
    .expect("the run completes");
    assert_eq!(none.assets, 0, "another album holds none of these");

    let album = restore_entries(
        &fx.workspace,
        &source,
        fx.entries.clone(),
        &options(elsewhere.path(), RestoreScope::Album(fx.album)),
    )
    .await
    .expect("the run completes");
    assert_eq!(album.restored, 2);
}

/// The feed carries every change, so the last entry for an asset is its head; a head that is a
/// deletion is left out of the restore rather than failed.
#[tokio::test]
async fn the_last_entry_is_the_head_and_a_deleted_head_is_skipped() {
    let fx = fixture("deleted", 2);
    let out = Scratch::new("deleted-out");
    let mut entries = fx.entries.clone();
    let mut tombstone = entries[0].clone();
    tombstone.kind = ChangeKind::Deleted;
    tombstone.sync_seq = 99;
    entries.push(tombstone);

    let source = MockSource::new(fx.blobs.clone());
    let summary = restore_entries(
        &fx.workspace,
        &source,
        entries,
        &options(out.path(), RestoreScope::Library),
    )
    .await
    .expect("the run completes");
    assert_eq!(summary.skipped_deleted, 1);
    assert_eq!(summary.assets, 1);
    assert_eq!(summary.restored, 1);
    assert_eq!(summary.outcomes[0].asset_id, fx.assets[1].to_string());
}

/// A library that holds no keys for the album cannot open its originals: each asset fails with
/// a reason that says so, nothing is written, and the run fails.
#[tokio::test]
async fn a_library_without_the_album_keys_restores_nothing() {
    let fx = fixture("keyless", 1);
    let other = Scratch::new("keyless-other");
    let keyless = Workspace::create_with_params(other.path(), b"passphrase", FAST_KDF)
        .expect("another library");
    let out = Scratch::new("keyless-out");
    let source = MockSource::new(fx.blobs.clone());

    let summary = restore_entries(
        &keyless,
        &source,
        fx.entries.clone(),
        &options(out.path(), RestoreScope::Library),
    )
    .await
    .expect("the run completes");
    assert_eq!(summary.failed, 1);
    let failure = summary.outcomes[0].error.as_ref().expect("a reason");
    assert_eq!(failure.kind, "keys_unavailable");
    assert!(failure.message.contains(&fx.album.to_string()));
    assert_eq!(
        present_stems(out.path()).expect("the output reads").len(),
        0,
        "no file is written"
    );
    assert_eq!(
        failed_run_reason(&summary).as_deref(),
        Some("1 of 1 assets were not restored")
    );
}

/// Bytes that do not hash to their content address are refused, and their part file is
/// removed so the next run fetches them afresh rather than resuming from a bad prefix.
#[tokio::test]
async fn a_corrupt_blob_is_refused_and_its_part_file_discarded() {
    let fx = fixture("corrupt", 1);
    let out = Scratch::new("corrupt-out");
    let original = fx.entries[0].blobs.original.clone().expect("an original");
    let mut blobs = fx.blobs.clone();
    blobs
        .get_mut(&original.ciphertext_hash)
        .expect("the original is served")[10] ^= 0x01;
    let source = MockSource::new(blobs);

    let summary = restore_entries(
        &fx.workspace,
        &source,
        fx.entries.clone(),
        &options(out.path(), RestoreScope::Library),
    )
    .await
    .expect("the run completes");
    assert_eq!(summary.failed, 1);
    assert_eq!(
        summary.outcomes[0].error.as_ref().expect("a reason").kind,
        "fetch"
    );
    assert!(
        !out.path()
            .join(STAGING_DIR)
            .join(format!("{}.part", original.ciphertext_hash))
            .exists()
    );
    assert_eq!(
        present_stems(out.path()).expect("the output reads").len(),
        0
    );
}

/// An original the server does not hold yet is a failure that names the remedy.
#[tokio::test]
async fn an_original_awaiting_upload_fails_without_a_request() {
    let fx = fixture("awaiting", 1);
    let out = Scratch::new("awaiting-out");
    let mut entries = fx.entries.clone();
    entries[0].original_held = false;
    let source = MockSource::new(fx.blobs.clone());

    let summary = restore_entries(
        &fx.workspace,
        &source,
        entries,
        &options(out.path(), RestoreScope::Library),
    )
    .await
    .expect("the run completes");
    assert_eq!(
        summary.outcomes[0].error.as_ref().expect("a reason").kind,
        "awaiting_original"
    );
    assert_eq!(source.request_count(), 0);
}

/// The original's address becomes a part file's name, so an address that is not a SHA-256 in
/// hex never reaches the filesystem: a hostile feed cannot name a path outside staging.
#[tokio::test]
async fn an_original_address_that_is_not_a_hash_never_names_a_file() {
    let fx = fixture("traversal", 1);
    let out = Scratch::new("traversal-out");
    let mut entries = fx.entries.clone();
    entries[0]
        .blobs
        .original
        .as_mut()
        .expect("an original")
        .ciphertext_hash = "../../escaped".to_owned();
    let source = MockSource::new(fx.blobs.clone());

    let summary = restore_entries(
        &fx.workspace,
        &source,
        entries,
        &options(out.path(), RestoreScope::Library),
    )
    .await
    .expect("the run completes");
    assert_eq!(
        summary.outcomes[0].error.as_ref().expect("a reason").kind,
        "malformed_feed"
    );
    assert!(!out.path().join("escaped.part").exists());
    assert!(
        source.requests_for("../../escaped").is_empty(),
        "nothing is requested for it either"
    );
}

/// A part file longer than its blob cannot be a prefix of it, so it is started over.
#[tokio::test]
async fn a_part_file_longer_than_its_blob_is_started_over() {
    let dir = Scratch::new("long-part");
    let part = dir.path().join("x.part");
    std::fs::write(&part, vec![7u8; 32]).expect("the part file writes");
    let source = MockSource::new(HashMap::new());
    let persisting = PersistingSource::open(&source, &part, 16).expect("it opens");
    assert_eq!(persisting.held_len(), 0);
    assert_eq!(std::fs::metadata(&part).expect("it exists").len(), 0);
}

/// **The `--json` contract.** One document: the totals, and per asset its status, path and
/// byte counts, and — for a failed one — a stable `kind`, the server's code, and the detail.
#[test]
fn the_json_report_lists_each_assets_outcome() {
    let mut summary = RestoreSummary {
        assets: 2,
        out: "/tmp/out".to_owned(),
        ..Default::default()
    };
    summary.record(AssetRestoreOutcome {
        asset_id: Uuid::from_u128(1).to_string(),
        album_id: Uuid::from_u128(9).to_string(),
        status: AssetRestoreStatus::Restored,
        path: Some("/tmp/out/a.jpg".to_owned()),
        fetched_bytes: 10,
        resumed_bytes: 5,
        error: None,
    });
    summary.record(AssetRestoreOutcome {
        asset_id: Uuid::from_u128(2).to_string(),
        album_id: Uuid::from_u128(9).to_string(),
        status: AssetRestoreStatus::Failed,
        path: None,
        fetched_bytes: 0,
        resumed_bytes: 0,
        error: Some(AssetRestoreFailure::fetch(
            "original",
            &FetchError::AuthorizationChanged {
                code: Some("error.blob.access_revoked".to_owned()),
            },
        )),
    });

    let json = serde_json::to_value(&summary).expect("the summary serializes");
    assert_eq!(json["restored"], 1);
    assert_eq!(json["failed"], 1);
    assert_eq!(json["fetched_bytes"], 10);
    assert_eq!(json["resumed_bytes"], 5);
    let outcomes = json["outcomes"].as_array().expect("an outcome per asset");
    assert_eq!(outcomes[0]["status"], "restored");
    assert_eq!(outcomes[0]["path"], "/tmp/out/a.jpg");
    assert!(outcomes[0]["error"].is_null());
    assert_eq!(outcomes[1]["status"], "failed");
    assert_eq!(outcomes[1]["error"]["kind"], "fetch");
    assert_eq!(outcomes[1]["error"]["code"], "error.blob.access_revoked");

    assert_eq!(
        summary_line(&summary),
        "Restored 1 of 2 assets to /tmp/out (0 already present, 1 failed, 0 deleted skipped)."
    );
    assert!(failure_line(&summary.outcomes[1]).starts_with(&format!(
        "asset {} failed: fetching the original failed",
        Uuid::from_u128(2)
    )));
}

#[test]
fn a_media_type_names_its_extension_and_anything_else_is_bin() {
    assert_eq!(extension_for("image/jpeg"), "jpg");
    assert_eq!(extension_for("video/quicktime"), "mov");
    assert_eq!(extension_for("application/x-unknown"), "bin");
}
