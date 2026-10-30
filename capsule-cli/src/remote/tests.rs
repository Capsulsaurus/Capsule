use super::*;

/// The write-back that closes the CLI's session-persistence bug.
///
/// The server rotates on refresh and closes the old session, so a command that refreshed and
/// then dropped the rotated pair left the store holding a dead refresh token — and roughly
/// fifteen minutes after signing in every command demanded an interactive login. This is the
/// plumbing half: whatever the session holds when the work is done reaches the store.
#[tokio::test]
async fn a_checkpoint_writes_the_sessions_current_pair_back_to_the_store() {
    let dir = std::env::temp_dir().join(format!("capsule-cli-checkpoint-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("a temp dir");
    let store = SessionStore::new(dir.join("session.json"));

    let persisted = capsule_sdk::auth::PersistedSession {
        access_token: "access-1".to_owned().into(),
        refresh_token: "refresh-1".to_owned().into(),
        access_expires_at_unix: jiff::Timestamp::now().as_second() + 3600,
    };
    store.save(&persisted).expect("the store writes");

    let client = AuthClient::new("http://127.0.0.1:1/v1/auth").expect("a base url");
    let expected_refresh = {
        use secrecy::ExposeSecret as _;
        persisted.refresh_token.expose_secret().to_owned()
    };
    let session = client.resume(persisted).expect("a resumed session");
    checkpoint(&store, &session).await;

    let reloaded = store
        .load()
        .expect("the store reads")
        .expect("a session is stored");
    use secrecy::ExposeSecret as _;
    assert_eq!(reloaded.refresh_token.expose_secret(), expected_refresh);

    // And a session with nothing left to export leaves the store as it was, rather than
    // clearing it: logging out is `auth_logout`'s job and nobody else's.
    std::fs::remove_dir_all(&dir).ok();
}

/// `from_env` reads process-wide state, so these run under one lock and restore what they
/// touched — otherwise a parallel test would see another's variables.
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Clear every endpoint variable, run `f`, and restore the prior values.
fn with_clean_env<T>(f: impl FnOnce() -> T) -> T {
    const VARS: [&str; 5] = [
        "CAPSULE_ENDPOINT",
        "CAPSULE_AUTH_ENDPOINT",
        "CAPSULE_SYNC_ENDPOINT",
        "CAPSULE_UPLOAD_ENDPOINT",
        "CAPSULE_PROTOCOL",
    ];
    let guard = ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let saved: Vec<_> = VARS.iter().map(|k| (*k, std::env::var(k).ok())).collect();
    for k in VARS {
        unsafe { std::env::remove_var(k) };
    }
    let out = f();
    for (k, v) in saved {
        match v {
            Some(v) => unsafe { std::env::set_var(k, v) },
            None => unsafe { std::env::remove_var(k) },
        }
    }
    drop(guard);
    out
}

#[test]
fn endpoint_defaults_target_a_single_origin() {
    let cfg = with_clean_env(RemoteConfig::from_env);
    // The old defaults were :8080 and :8081 — two ports that exist only in the
    // integration harness. A real server serves everything from one.
    assert_eq!(cfg.auth_endpoint, "http://127.0.0.1:3000/v1/auth");
    assert_eq!(cfg.sync_endpoint, "http://127.0.0.1:3000");
    assert_eq!(cfg.upload_endpoint, "http://127.0.0.1:3000/v1/upload");
    assert_eq!(
        cfg.protocol_version,
        capsule_core::crypto::primitives::PROTOCOL_VERSION
    );
}

/// One protocol version per binary (system invariant 8, issue #674): the CLI speaks
/// `capsule-core`'s, the same date the SDK transport sends on every other request, and the
/// environment cannot move it.
#[test]
fn the_protocol_version_is_the_build_s_and_not_configurable() {
    let cfg = with_clean_env(|| {
        unsafe { std::env::set_var("CAPSULE_PROTOCOL", "2026-12-31") };
        RemoteConfig::from_env()
    });
    assert_eq!(
        cfg.protocol_version,
        capsule_core::crypto::primitives::PROTOCOL_VERSION
    );
    let handshake = capsule_sdk::net::protocol_headers();
    assert_eq!(
        handshake
            .get(capsule_sdk::net::negotiation::PROTOCOL_HEADER)
            .and_then(|value| value.to_str().ok()),
        Some(cfg.protocol_version.as_str()),
        "the upload pin and the transport's default header are one value"
    );
}

#[test]
fn a_single_base_derives_every_surface() {
    let cfg = with_clean_env(|| {
        unsafe { std::env::set_var("CAPSULE_ENDPOINT", "https://capsule.example.com") };
        RemoteConfig::from_env()
    });
    assert_eq!(cfg.auth_endpoint, "https://capsule.example.com/v1/auth");
    assert_eq!(cfg.sync_endpoint, "https://capsule.example.com");
    assert_eq!(cfg.upload_endpoint, "https://capsule.example.com/v1/upload");
}

#[test]
fn a_trailing_slash_does_not_double_up() {
    let cfg = with_clean_env(|| {
        unsafe { std::env::set_var("CAPSULE_ENDPOINT", "http://host:3000/") };
        RemoteConfig::from_env()
    });
    assert_eq!(cfg.auth_endpoint, "http://host:3000/v1/auth");
    assert_eq!(cfg.sync_endpoint, "http://host:3000");
    assert_eq!(cfg.upload_endpoint, "http://host:3000/v1/upload");
}

/// Split deployments (auth behind one ingress, sync behind another) stay expressible.
#[test]
fn per_surface_overrides_win_over_the_base() {
    let cfg = with_clean_env(|| {
        unsafe {
            std::env::set_var("CAPSULE_ENDPOINT", "http://ignored:1");
            std::env::set_var("CAPSULE_AUTH_ENDPOINT", "https://auth.example.com/v1/auth");
            std::env::set_var("CAPSULE_SYNC_ENDPOINT", "https://sync.example.com");
            std::env::set_var(
                "CAPSULE_UPLOAD_ENDPOINT",
                "https://upload.example.com/v1/upload",
            );
        }
        RemoteConfig::from_env()
    });
    assert_eq!(cfg.auth_endpoint, "https://auth.example.com/v1/auth");
    assert_eq!(cfg.sync_endpoint, "https://sync.example.com");
    assert_eq!(cfg.upload_endpoint, "https://upload.example.com/v1/upload");
}

/// The sync endpoint must stay a bare origin. tonic's `AddOrigin` keeps only the scheme
/// and authority of the endpoint URI and lets the generated stub write the path, so any
/// prefix here is silently dropped and the request lands on a path the server does not
/// serve. This is the regression guard for that.
#[test]
fn the_default_sync_endpoint_carries_no_path() {
    let cfg = with_clean_env(RemoteConfig::from_env);
    let after_scheme = cfg
        .sync_endpoint
        .split_once("://")
        .expect("sync endpoint has a scheme")
        .1;
    assert!(
        !after_scheme.contains('/'),
        "sync endpoint must be a bare origin, got {}",
        cfg.sync_endpoint
    );
}

/// A minted secret is shown as hex and wrapped under its bytes, so the decoded bytes are
/// tried first; the text as typed is still tried, because a chosen passphrase may happen
/// to be 64 hex characters.
#[test]
fn a_hex_recovery_secret_is_tried_decoded_and_then_as_typed() {
    let hex = "00ff".repeat(16);
    let candidates = recovery_secret_candidates(&format!("  {hex}\n"));
    assert_eq!(
        candidates,
        vec![[0x00, 0xff].repeat(16), hex.as_bytes().to_vec()]
    );
}

/// A library laid out the way a recovery leaves one: nested directories and files.
fn lay_out_a_library(root: &std::path::Path) {
    std::fs::create_dir_all(root.join(".library/keys")).expect("the skeleton creates");
    std::fs::write(root.join(".library/keys/device.key"), b"k").expect("a key file writes");
    std::fs::write(root.join("library.toml"), b"name").expect("a top-level file writes");
}

/// A recovery that created its root removes it whole when the directory join fails, so the
/// path is absent again and a rerun starts from nothing.
#[test]
fn a_failed_join_removes_the_root_the_recovery_created() {
    let parent = std::env::temp_dir().join(format!(
        "capsule-cli-discard-created-{}",
        std::process::id()
    ));
    let root = parent.join("recovered");
    lay_out_a_library(&root);

    discard_new_library(&root, false);

    assert!(!root.exists(), "the created root is gone");
    assert!(parent.exists(), "and nothing above it is touched");
    std::fs::remove_dir_all(&parent).ok();
}

/// A recovery into an empty directory the user made leaves that directory, empty, in place.
#[test]
fn a_failed_join_empties_a_root_that_existed_and_keeps_it() {
    let root = std::env::temp_dir().join(format!(
        "capsule-cli-discard-existing-{}",
        std::process::id()
    ));
    lay_out_a_library(&root);

    discard_new_library(&root, true);

    assert!(root.is_dir(), "the user's directory stays");
    assert_eq!(
        std::fs::read_dir(&root).expect("it reads").count(),
        0,
        "and is empty again, as it was before the recovery"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[test]
fn a_chosen_recovery_secret_is_tried_as_typed_only() {
    assert_eq!(
        recovery_secret_candidates("seven words the user wrote down"),
        vec![b"seven words the user wrote down".to_vec()]
    );
    // Hex of the wrong length is not a minted secret.
    assert_eq!(recovery_secret_candidates("00ff"), vec![b"00ff".to_vec()]);
    // 64 characters that are not hex.
    let not_hex = "z".repeat(64);
    assert_eq!(
        recovery_secret_candidates(&not_hex),
        vec![not_hex.as_bytes().to_vec()]
    );
}

/// A verified asset and a failed one, as a run that carried on past the failure records them.
fn mixed_run() -> PushSummary {
    let mut summary = PushSummary {
        assets: 2,
        ..Default::default()
    };
    let mut verified = AssetPushOutcome::new(Uuid::from_u128(1), AssetPushStatus::Verified);
    verified.uploaded = 3;
    verified.bytes = 4096;
    verified.receipts = 3;
    summary.record(verified);

    let mut failed = AssetPushOutcome::new(Uuid::from_u128(2), AssetPushStatus::Failed);
    failed.uploaded = 2;
    failed.error = Some(
        AssetError::from(PushError::NotDurable {
            asset_id: Uuid::from_u128(2).to_string(),
            not_durable: vec!["ab".repeat(32)],
        })
        .failure(),
    );
    summary.record(failed);
    summary
}

/// One asset's failure is that asset's: the run's totals count both, the failure is listed,
/// and the run is reported as failed — which is what makes the command exit non-zero.
#[test]
fn a_failed_asset_is_counted_listed_and_fails_the_run() {
    let summary = mixed_run();
    assert_eq!(summary.verified_assets, 1);
    assert_eq!(summary.failed_assets, 1);
    assert_eq!(summary.pushed_assets, 2, "both opened sessions");
    assert_eq!(summary.uploaded_blobs, 5);
    assert_eq!(summary.receipts, 3);

    let failures: Vec<_> = summary.failures().collect();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].asset_id, Uuid::from_u128(2).to_string());
    assert!(failure_line(failures[0]).contains("does not durably hold"));
    assert_eq!(
        failed_run_reason(&summary).as_deref(),
        Some("1 of 2 assets did not reach verified custody")
    );
    assert_eq!(
        custody_line(&summary),
        "Custody verified for 1 of 2 assets; 3 receipts stored in the library."
    );
}

/// A run with no failure exits zero.
#[test]
fn a_run_with_no_failure_is_not_failed() {
    let mut summary = PushSummary {
        assets: 1,
        ..Default::default()
    };
    summary.record(AssetPushOutcome::new(
        Uuid::from_u128(1),
        AssetPushStatus::Verified,
    ));
    assert_eq!(failed_run_reason(&summary), None);
}

/// **The `--json` contract.** One document: the totals, and per asset its status, counts,
/// receipts, and — for a failed one — a stable `kind`, the server's code, and the detail.
#[test]
fn the_json_report_lists_each_assets_outcome() {
    let json = serde_json::to_value(mixed_run()).expect("the summary serializes");
    assert_eq!(json["assets"], 2);
    assert_eq!(json["verified_assets"], 1);
    assert_eq!(json["failed_assets"], 1);
    assert_eq!(json["receipts"], 3);
    assert_eq!(json["dry_run"], false);

    let outcomes = json["outcomes"].as_array().expect("an outcome per asset");
    assert_eq!(outcomes.len(), 2);
    assert_eq!(outcomes[0]["asset_id"], Uuid::from_u128(1).to_string());
    assert_eq!(outcomes[0]["status"], "verified");
    assert_eq!(outcomes[0]["receipts"], 3);
    assert!(outcomes[0]["error"].is_null());

    assert_eq!(outcomes[1]["status"], "failed");
    assert_eq!(outcomes[1]["error"]["kind"], "not_durable");
    assert!(outcomes[1]["error"]["code"].is_null());
    assert!(
        outcomes[1]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not durably hold 1 of its blobs")
    );
}

/// A server's stable code rides the failure, so a client can localize it.
#[test]
fn a_rejected_upload_carries_the_servers_code() {
    let failure = AssetError::from(PushError::Upload(
        capsule_sdk::upload::UploadError::Rejected {
            status: 500,
            code: Some("error.upload.unavailable".into()),
            message: "the asset index could not answer".into(),
        },
    ))
    .failure();
    assert_eq!(failure.kind, "upload");
    assert_eq!(failure.code.as_deref(), Some("error.upload.unavailable"));

    // A coded answer the transfer waited out and gave up on keeps its code too.
    let failure = AssetError::from(PushError::Upload(
        capsule_sdk::upload::UploadError::WaitsExhausted {
            status: 500,
            code: Some("error.upload.storage_inconsistent".into()),
            message: "the staged upload and the session's byte count disagree".into(),
            reason: "server-requested wait: exceeded 64 recovery attempts".into(),
        },
    ))
    .failure();
    assert_eq!(failure.kind, "upload");
    assert_eq!(
        failure.code.as_deref(),
        Some("error.upload.storage_inconsistent")
    );
}

/// **The terminal outcome reaches `--json`.** No request waits on finalization, so the SDK reads
/// a refused finalization from `HEAD` — the `200` that reported `failed_processing` and its
/// `X-Capsule-Upload-Error` code — and the report names the asset failed with that code.
#[test]
fn a_finalization_the_server_refused_reaches_the_json_report_with_its_code() {
    let mut summary = PushSummary {
        assets: 1,
        ..Default::default()
    };
    let mut failed = AssetPushOutcome::new(Uuid::from_u128(3), AssetPushStatus::Failed);
    failed.error = Some(
        AssetError::from(PushError::Upload(UploadError::Rejected {
            status: 200,
            code: Some("error.upload.content_hash_mismatch".into()),
            message: "the server refused session s at finalization \
                      (error.upload.content_hash_mismatch)"
                .into(),
        }))
        .failure(),
    );
    summary.record(failed);

    let json = serde_json::to_value(&summary).expect("the summary serializes");
    let outcome = &json["outcomes"][0];
    assert_eq!(outcome["status"], "failed");
    assert_eq!(
        outcome["uploaded"], 0,
        "a refused blob is not counted uploaded"
    );
    assert_eq!(outcome["error"]["kind"], "upload");
    assert_eq!(
        outcome["error"]["code"], "error.upload.content_hash_mismatch",
        "the session's terminal failure code"
    );
    assert_eq!(json["failed_assets"], 1);
}

/// The server's refusal of a create, as finalization answers it (issue #822).
fn refused_create() -> UploadError {
    UploadError::Rejected {
        status: 400,
        code: Some("error.upload.envelope_rejected".into()),
        message: "the manifest envelope did not survive re-validation at finalization".into(),
    }
}

/// **No newer epoch to re-seal at.** A refused create this device cannot re-seal because it
/// holds no newer album epoch keeps the `upload` kind any refusal has and the server's code.
/// Its detail must not claim a key is missing: the same refusal answers a revoked device,
/// withdrawn write access or a role conflict, where no key is missing at all. A re-seal that
/// failed for any other reason is `reseal`.
#[test]
fn a_create_with_no_newer_epoch_stays_an_upload_refusal() {
    let asset_id = Uuid::from_u128(7);
    let failure = AssetError::Reseal {
        refused: refused_create(),
        error: LifecycleError::ResealRefused {
            asset_id,
            reason: ResealRefusal::NoNewerEpoch {
                sealed: 1,
                current: 1,
            },
        },
    }
    .failure();
    assert_eq!(failure.kind, "upload");
    assert_eq!(
        failure.code.as_deref(),
        Some("error.upload.envelope_rejected")
    );
    assert!(
        failure.message.contains("no newer epoch to re-seal it at"),
        "{}",
        failure.message
    );
    assert!(
        !failure.message.contains("key"),
        "the detail must not claim a key is missing: {}",
        failure.message
    );

    let other = AssetError::Reseal {
        refused: refused_create(),
        error: LifecycleError::ResealRefused {
            asset_id,
            reason: ResealRefusal::NotALoneCreate,
        },
    }
    .failure();
    assert_eq!(other.kind, "reseal");
    assert_eq!(
        other.code.as_deref(),
        Some("error.upload.envelope_rejected")
    );
}

/// A second refusal after the re-seal fails the asset as the refusal it is.
#[test]
fn a_refused_create_that_reaches_the_report_is_named() {
    let failure = AssetError::from(PushError::CreateRefused {
        asset_id: Uuid::from_u128(7).to_string(),
        amk_version: 2,
        source: Box::new(refused_create()),
    })
    .failure();
    assert_eq!(failure.kind, "create_refused");
    assert_eq!(
        failure.code.as_deref(),
        Some("error.upload.envelope_rejected")
    );
}

// ── Move release (#671) ─────────────────────────────────────────────────

/// A scratch directory that removes itself.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-cli-move-{}", nanoid::nanoid!()));
        std::fs::create_dir_all(&path).expect("a temp dir");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// A library holding `files.len()` assets imported in Move mode, their sources left in
/// place, and the `MovedSource` for each.
fn moved_library(scratch: &Scratch, files: &[&[u8]]) -> (Workspace, Vec<MovedSource>) {
    let lib = scratch.0.join("library");
    std::fs::create_dir_all(&lib).unwrap();
    let mut ws = Workspace::create_with_params(
        &lib,
        b"passphrase",
        capsule_core::crypto::primitives::Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    let opts = capsule_library::lifecycle::SignedImportOptions {
        move_source: true,
        defer_source_release: true,
        stack: None,
        enrichment: None,
    };
    let moved = files
        .iter()
        .enumerate()
        .map(|(i, bytes)| {
            let source = scratch.0.join(format!("photo-{i}.jpg"));
            std::fs::write(&source, bytes).unwrap();
            let asset_id = ws
                .import_asset_with(album, &source, &opts)
                .unwrap()
                .asset_id;
            MovedSource { source, asset_id }
        })
        .collect();
    (ws, moved)
}

/// A server that holds the assets in `durable` and nothing else.
struct HoldsOnly(Vec<Uuid>);

impl StorageVerifier for HoldsOnly {
    fn verify(
        &self,
        asset_id: Uuid,
        blob_hashes: &[capsule_core::crypto::hash::Hash32],
    ) -> Result<capsule_library::library::StorageVerdict, capsule_library::library::VerifierError>
    {
        let durable = self.0.contains(&asset_id);
        Ok(capsule_library::library::StorageVerdict {
            asset_id,
            durable,
            blobs: blob_hashes
                .iter()
                .map(|hash| capsule_library::library::BlobVerdict {
                    hash: *hash,
                    role: capsule_library::library::BlobRole::Original,
                    stored: durable,
                    indexed: durable,
                    retrievable: durable,
                })
                .collect(),
            checked_at: "2026-10-22T00:00:00Z".into(),
        })
    }

    fn receipt_verified(
        &self,
        asset_id: Uuid,
        _blob_hashes: &[capsule_core::crypto::hash::Hash32],
    ) -> Result<bool, capsule_library::library::VerifierError> {
        Ok(self.0.contains(&asset_id))
    }
}

/// Only the source whose asset the server holds is deleted; the other is kept and says why.
#[test]
fn a_move_release_deletes_only_the_sources_the_server_holds() {
    let scratch = Scratch::new();
    let (ws, moved) = moved_library(&scratch, &[b"held", b"not yet held"]);

    let summary = release_sources(&ws, &HoldsOnly(vec![moved[0].asset_id]), &moved);

    assert_eq!((summary.released, summary.kept), (1, 1));
    assert!(
        !moved[0].source.exists(),
        "the held asset's source is released"
    );
    assert!(
        moved[1].source.exists(),
        "the unconfirmed asset's source is kept"
    );
    assert!(summary.outcomes[0].released);
    assert_eq!(summary.outcomes[1].kept_because, Some("not_durable"));
    assert_eq!(
        kept_source_line(&summary.outcomes[1]),
        format!("kept {}: not_durable", moved[1].source.display())
    );
    assert_eq!(
        move_release_line(&summary),
        "Deleted 1 source files the server holds; kept 1."
    );
}

/// An asset the library does not hold is reported, and its source left alone.
#[test]
fn a_source_naming_an_unknown_asset_is_kept() {
    let scratch = Scratch::new();
    let (ws, mut moved) = moved_library(&scratch, &[b"orphan"]);
    moved[0].asset_id = Uuid::now_v7();

    let summary = release_sources(&ws, &HoldsOnly(vec![moved[0].asset_id]), &moved);

    assert_eq!(summary.outcomes[0].kept_because, Some("unknown_asset"));
    assert!(moved[0].source.exists());
}

/// One custody question per asset, declaring the original and its metadata blob, and none
/// for an asset the library does not hold.
#[test]
fn held_releases_asks_once_per_asset_about_its_original() {
    let scratch = Scratch::new();
    let (ws, mut moved) = moved_library(&scratch, &[b"one asset"]);
    moved.push(moved[0].clone());
    moved.push(MovedSource {
        source: scratch.0.join("absent.jpg"),
        asset_id: Uuid::now_v7(),
    });

    let releases = held_releases(&ws, &moved);

    assert_eq!(releases.len(), 1);
    assert_eq!(releases[0].asset_id, moved[0].asset_id);
    let roles: Vec<_> = releases[0].blobs.iter().map(|b| b.role).collect();
    assert_eq!(
        roles,
        vec![
            capsule_library::library::BlobRole::Original,
            capsule_library::library::BlobRole::Metadata
        ]
    );
}

/// An asset edited since its create is still asked about, and about its chain head's blobs:
/// exactly what the release gate declares, so the server's answer can release its source.
#[test]
fn held_releases_asks_about_an_edited_assets_head() {
    let scratch = Scratch::new();
    let (mut ws, moved) = moved_library(&scratch, &[b"edited"]);
    ws.tag_add(&moved[0].asset_id, "beach").expect("tagged");
    let head = ws
        .asset(&moved[0].asset_id)
        .unwrap()
        .chain
        .records()
        .last()
        .unwrap()
        .manifest
        .core
        .clone();
    assert_ne!(
        ws.upload_bundle(&moved[0].asset_id)
            .expect("the create's bundle")
            .metadata_blob_hash,
        head.metadata_blob_hash,
        "the edit superseded the create's metadata blob, which the upload still carries"
    );

    let releases = held_releases(&ws, &moved);

    assert_eq!(releases.len(), 1, "{releases:#?}");
    let hashes: Vec<_> = releases[0]
        .blobs
        .iter()
        .map(|b| b.ciphertext_hash)
        .collect();
    assert_eq!(
        hashes,
        vec![
            head.ciphertext_hash,
            head.metadata_blob_hash
                .expect("an edit binds a metadata blob")
        ]
    );
}

#[test]
fn every_retain_reason_has_a_stable_name() {
    assert_eq!(kept_because(&RetainReason::NotDurable), "not_durable");
    assert_eq!(
        kept_because(&RetainReason::ReceiptMissing),
        "receipt_missing"
    );
    assert_eq!(
        kept_because(&RetainReason::VerifyUnavailable),
        "verify_unavailable"
    );
    assert_eq!(
        kept_because(&RetainReason::ReceiptUnavailable),
        "receipt_unavailable"
    );
    assert_eq!(
        kept_because(&RetainReason::ReadBackMismatch),
        "read_back_mismatch"
    );
}

/// Without a server the Move import says it kept every source, and why.
#[test]
fn the_offline_line_says_every_source_was_kept_and_how_to_release_them() {
    let line = sources_kept_offline_line(3);
    assert!(line.starts_with("Kept 3 source files"), "{line}");
    assert!(line.contains("--push"), "{line}");
}

/// One server, however its endpoint is written, has one pin.
#[test]
fn the_pin_key_is_the_canonical_origin_of_the_endpoint() {
    for (endpoint, origin) in [
        ("https://capsule.example", "https://capsule.example"),
        ("https://Capsule.Example:443/", "https://capsule.example"),
        ("https://capsule.example/v1?x=1", "https://capsule.example"),
        ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
        ("http://host:80/", "http://host"),
    ] {
        assert_eq!(
            server_origin(endpoint).expect("an origin"),
            origin,
            "{endpoint}"
        );
    }
    assert_ne!(
        server_origin("http://capsule.example").expect("an origin"),
        server_origin("https://capsule.example").expect("an origin"),
        "a scheme change is another origin"
    );
}

#[test]
fn an_endpoint_with_no_network_origin_is_refused() {
    for endpoint in ["not a url", "file:///tmp/server"] {
        assert!(
            matches!(
                server_origin(endpoint),
                Err(RemoteError::ServerOrigin { .. })
            ),
            "{endpoint}"
        );
    }
}

// ── Album key escrow refusals (#741) ────────────────────────────────────

/// What the escrow mock saw, and which album it refuses.
#[derive(Clone)]
struct EscrowMock {
    /// The album whose `PUT` answers `409 error.escrow.album_keys_full`.
    full_for: Uuid,
    /// Every `METHOD path` the mock answered, in order.
    seen: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl EscrowMock {
    /// The escrow `PUT`s the mock answered, as album ids.
    fn stores(&self) -> Vec<String> {
        self.seen
            .lock()
            .unwrap()
            .iter()
            .filter_map(|line| line.strip_prefix("PUT /v1/auth/escrow/albums/"))
            .map(str::to_owned)
            .collect()
    }

    /// The one response for `request_line` (`METHOD path`).
    fn answer(&self, request_line: &str) -> (u16, &'static str, String) {
        self.seen.lock().unwrap().push(request_line.to_owned());
        if request_line == "GET /v1/auth/escrow/albums" {
            return (200, "application/json", r#"{"albums":[]}"#.to_owned());
        }
        // The server truth every push reads before it sends a byte: an empty feed, and the
        // attestation key the library pins on first contact.
        if request_line.starts_with("GET /v1/sync") {
            let page = serde_json::json!({
                "entries": [],
                "next_cursor": "",
                "has_more": false,
            });
            return (200, "application/json", page.to_string());
        }
        if request_line == "GET /.well-known/capsule/attestation-keys" {
            use base64::Engine as _;
            let public = base64::engine::general_purpose::STANDARD.encode(
                capsule_core::crypto::keys::HybridSigningKey::from_seed64(&[5u8; 64])
                    .verifying_key()
                    .to_bytes(),
            );
            let keys = serde_json::json!({
                "server_id": "capsule.example",
                "keys": [{
                    "key_id": "11",
                    "public": public,
                    "algorithm": "hybrid-ed25519-mldsa65",
                    "active_from": "2026-07-10T00:00:00Z",
                    "active_to": null,
                }],
            });
            return (200, "application/json", keys.to_string());
        }
        if let Some(album_id) = request_line.strip_prefix("PUT /v1/auth/escrow/albums/") {
            return if album_id == self.full_for.to_string() {
                let problem = serde_json::json!({
                    "type": "about:blank",
                    "title": "Conflict",
                    "status": 409,
                    "detail": "the account's album key escrows are full",
                    "code": "error.escrow.album_keys_full",
                });
                (409, "application/problem+json", problem.to_string())
            } else {
                let stored = serde_json::json!({
                    "album_id": album_id,
                    "stored_at": "2026-10-22T00:00:00Z",
                    "replaced": false,
                });
                (200, "application/json", stored.to_string())
            };
        }
        let problem = serde_json::json!({
            "type": "about:blank",
            "title": "Not Implemented",
            "status": 501,
            "detail": "the mock serves the escrow routes only",
            "code": "error.unexpected",
        });
        (501, "application/problem+json", problem.to_string())
    }
}

/// Serve `mock` on an ephemeral loopback port and return its origin. Each connection answers
/// one request and closes.
async fn serve_escrow_mock(mock: EscrowMock) -> String {
    serve_mock(move |line| mock.answer(line)).await
}

/// Serve `answer` on an ephemeral loopback port and return its origin: it is handed each
/// request's `METHOD path` and returns the status, content type and body. Each connection
/// answers one request and closes.
pub(crate) async fn serve_mock(
    answer: impl Fn(&str) -> (u16, &'static str, String) + Clone + Send + Sync + 'static,
) -> String {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let mock = answer;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("a loopback port");
    let origin = format!("http://{}", listener.local_addr().expect("an address"));
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let mock = mock.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_end, length) = loop {
                    let read = socket.read(&mut chunk).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..read]);
                    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&buf[..end]).to_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    break (end + 4, length);
                };
                while buf.len() < head_end + length {
                    let read = socket.read(&mut chunk).await.unwrap_or(0);
                    if read == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..read]);
                }
                let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                let mut parts = head.split_whitespace();
                let line = format!(
                    "{} {}",
                    parts.next().unwrap_or_default(),
                    parts.next().unwrap_or_default()
                );
                let (status, content_type, body) = mock(&line);
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: {content_type}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
                let _ = socket.shutdown().await;
            });
        }
    });
    origin
}

/// A server answering `409 error.escrow.album_keys_full` for one album's key escrow does not
/// stop the run: that album's assets are held back with `album_keys_not_escrowed` (and never
/// reach the album or upload surfaces), while every other album's escrow is still stored and
/// its assets are not held back by it. This is the branch that differs from a refused store
/// (`RemoteError::Recovery`), which fails the whole run before any asset is sent.
#[tokio::test]
async fn a_full_escrow_holds_back_only_that_albums_assets() {
    let EscrowRun {
        _scratch,
        mut ws,
        full_album,
        other_album,
        held_back,
        other,
        mock,
        remote,
        session,
    } = escrow_run().await;

    let summary = push_with(
        &remote,
        &mut ws,
        session,
        PushOptions::default(),
        PushRun::default(),
    )
    .await
    .expect("a full escrow holds assets back; it does not fail the run");

    let stores = mock.stores();
    assert_eq!(stores.len(), 2, "both albums were attempted: {stores:?}");
    assert!(stores.contains(&full_album.to_string()));
    assert!(stores.contains(&other_album.to_string()));
    assert_eq!(
        summary.album_keys_escrowed, 1,
        "only the album the server accepted counts as escrowed"
    );

    let failure_of = |asset_id: Uuid| {
        summary
            .outcomes
            .iter()
            .find(|outcome| outcome.asset_id == asset_id.to_string())
            .and_then(|outcome| outcome.error.as_ref())
            .map(|error| error.kind)
    };
    assert_eq!(failure_of(held_back), Some("album_keys_not_escrowed"));
    assert_ne!(
        failure_of(other),
        Some("album_keys_not_escrowed"),
        "an album whose escrow was stored is not held back by another's"
    );
    assert!(
        !mock
            .seen
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains(&full_album.to_string()) && !line.contains("/escrow/")),
        "nothing was sent for the held-back album"
    );
}

/// A push scoped to named assets (`capsule push --asset`, the TUI's retry) considers those
/// assets and no other, in the order given with repeats dropped, and tells its observer when
/// each starts — with every blob of its bundle — and how each finished, exactly as the
/// summary records it. An id the library does not hold fails as that asset, not silently.
#[tokio::test]
async fn a_scoped_push_considers_only_the_named_assets_and_reports_each_as_it_goes() {
    let EscrowRun {
        _scratch,
        mut ws,
        held_back,
        other,
        remote,
        session,
        ..
    } = escrow_run().await;
    let missing = Uuid::now_v7();

    let mut seen = Vec::new();
    let mut observe = |progress: AssetProgress| seen.push(progress);
    let summary = push_with(
        &remote,
        &mut ws,
        session,
        PushOptions::default(),
        PushRun {
            assets: Some(vec![held_back, missing, held_back]),
            observer: Some(&mut observe),
            ..PushRun::default()
        },
    )
    .await
    .expect("the run reaches its assets");

    assert_eq!(summary.assets, 2, "the repeat is dropped");
    let ids: Vec<&str> = summary
        .outcomes
        .iter()
        .map(|o| o.asset_id.as_str())
        .collect();
    assert_eq!(ids, vec![held_back.to_string(), missing.to_string()]);
    assert!(
        !ids.contains(&other.to_string().as_str()),
        "an asset not named is not pushed"
    );
    let missing_failure = summary.outcomes[1].error.as_ref().map(|e| e.kind);
    assert_eq!(missing_failure, Some("bundle"));

    let [
        AssetProgress::Started { asset_id, blobs },
        AssetProgress::Finished(first),
        AssetProgress::Finished(second),
    ] = seen.as_slice()
    else {
        panic!("one start and two finishes, in order: {seen:?}");
    };
    assert_eq!(*asset_id, held_back);
    let bundle = ws.upload_bundle(&held_back).unwrap();
    let expected: Vec<String> = push::bundle_blobs(&bundle)
        .into_iter()
        .map(|(_, hash)| hash)
        .collect();
    assert_eq!(
        blobs, &expected,
        "every blob of the bundle, in ladder order"
    );
    assert_eq!(first, &summary.outcomes[0]);
    assert_eq!(second, &summary.outcomes[1]);
}

/// An asset handed to another device and still inside its deadline is skipped: reported
/// `delegated`, counted apart from failures, and nothing of it reaches the album or upload
/// surfaces. Every other asset is pushed as before, and a dry run reports it the same way.
#[tokio::test]
async fn a_delegated_asset_is_skipped_and_reported_while_the_rest_is_pushed() {
    for dry_run in [false, true] {
        let EscrowRun {
            _scratch,
            mut ws,
            other_album,
            held_back,
            other,
            mock,
            remote,
            session,
            ..
        } = escrow_run().await;

        let summary = push_with(
            &remote,
            &mut ws,
            session,
            PushOptions {
                dry_run,
                ..PushOptions::default()
            },
            PushRun {
                delegated: BTreeSet::from([other]),
                ..PushRun::default()
            },
        )
        .await
        .expect("the run reaches its assets");

        let status_of = |asset_id: Uuid| {
            summary
                .outcomes
                .iter()
                .find(|o| o.asset_id == asset_id.to_string())
                .map(|o| o.status)
        };
        assert_eq!(status_of(other), Some(AssetPushStatus::Delegated));
        assert_ne!(status_of(held_back), Some(AssetPushStatus::Delegated));
        assert_eq!(summary.delegated_assets, 1, "{summary:#?}");
        assert_eq!(summary.assets, 2);
        assert!(
            summary.failures().all(|o| o.asset_id != other.to_string()),
            "a delegated asset is not a failure"
        );
        let json = serde_json::to_value(&summary).expect("serializes");
        assert_eq!(json["delegated_assets"], 1);
        assert!(
            json["outcomes"]
                .as_array()
                .expect("outcomes")
                .iter()
                .any(|o| o["status"] == "delegated" && o["asset_id"] == other.to_string().as_str()),
            "{json:#}"
        );
        assert!(
            !mock.seen.lock().unwrap().iter().any(|line| {
                (line.contains(&other_album.to_string()) || line.contains(&other.to_string()))
                    && !line.contains("/escrow/")
            }),
            "nothing of the delegated asset was sent (dry run: {dry_run})"
        );
    }
}

#[test]
fn the_custody_line_counts_the_assets_left_to_their_delegate() {
    let mut summary = PushSummary {
        assets: 3,
        verified_assets: 2,
        receipts: 4,
        ..PushSummary::default()
    };
    assert_eq!(
        custody_line(&summary),
        "Custody verified for 2 of 3 assets; 4 receipts stored in the library."
    );
    summary.delegated_assets = 1;
    assert_eq!(
        custody_line(&summary),
        "Custody verified for 2 of 3 assets; 4 receipts stored in the library. Skipped 1 \
         handed to another device; `capsule peer status` follows them."
    );
}

/// A deleted asset's lifecycle op carries the delete record's canonical CBOR, an envelope that
/// projects that record (action `delete`, its signed retention floor, the album the path names,
/// no derivative), and the sealed metadata blob exactly when the record binds one. An asset
/// whose head is not a delete, or one the library does not hold, has none.
#[tokio::test]
async fn a_delete_op_carries_the_signed_delete_record_and_its_projection() {
    use base64::Engine as _;
    let EscrowRun {
        _scratch,
        mut ws,
        other,
        ..
    } = escrow_run().await;
    assert_eq!(delete_op(&ws, &other).expect("encodes"), None);
    assert_eq!(delete_op(&ws, &Uuid::now_v7()).expect("encodes"), None);

    ws.soft_delete(&other, 30).expect("deletes");
    let op = delete_op(&ws, &other)
        .expect("encodes")
        .expect("the head is a delete");
    let request: capsule_sdk::rest::types::OpRequest =
        serde_json::from_slice(&op).expect("the held op is the request body");
    let asset = ws.asset(&other).expect("held");
    let head = asset.chain.records().last().expect("a head");
    let core = &head.manifest.core;
    let base64 = base64::engine::general_purpose::STANDARD;
    assert_eq!(
        base64.decode(&request.manifest_cbor).expect("base64"),
        capsule_core::cbor::to_canonical_vec(head).expect("encodes")
    );
    let envelope = &request.manifest_envelope;
    assert_eq!(envelope.action, "delete");
    assert_eq!(envelope.album_id, Some(core.album_id.to_string()));
    assert_eq!(envelope.file_id, other.to_string());
    assert_eq!(envelope.retention_until, core.retention_until);
    assert!(
        envelope.retention_until.is_some(),
        "the signed floor rides along"
    );
    assert_eq!(
        envelope.prior_provenance_hash,
        core.prior_provenance_hash.map(|h| h.to_hex())
    );
    assert_eq!(envelope.ciphertext_hash, core.ciphertext_hash.to_hex());
    assert_eq!(envelope.derivative_role, None);
    assert_eq!(
        envelope.metadata_blob_hash,
        core.metadata_blob_hash.map(|h| h.to_hex())
    );
    assert_eq!(
        request
            .metadata_blob
            .map(|blob| base64.decode(blob).expect("base64")),
        core.metadata_blob_hash.map(|_| asset.metadata_blob.clone())
    );
}

/// A library of two albums against the escrow mock, which refuses one album's escrow as
/// full: one asset in each album, and a resumed session against the mock.
struct EscrowRun {
    _scratch: Scratch,
    ws: Workspace,
    full_album: Uuid,
    other_album: Uuid,
    held_back: Uuid,
    other: Uuid,
    mock: EscrowMock,
    remote: RemoteConfig,
    session: Session,
}

async fn escrow_run() -> EscrowRun {
    let scratch = Scratch::new();
    let lib = scratch.0.join("library");
    std::fs::create_dir_all(&lib).unwrap();
    let mut ws = Workspace::create_with_params(
        &lib,
        b"passphrase",
        capsule_core::crypto::primitives::Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .unwrap();
    let full_album = ws.default_album_id();
    let other_album = Uuid::now_v7();
    ws.ensure_album(full_album, "Full").unwrap();
    ws.ensure_album(other_album, "Other").unwrap();
    let opts = capsule_library::lifecycle::SignedImportOptions {
        move_source: false,
        defer_source_release: false,
        stack: None,
        enrichment: None,
    };
    let mut import = |album: Uuid, name: &str, bytes: &[u8]| {
        let source = scratch.0.join(name);
        std::fs::write(&source, bytes).unwrap();
        ws.import_asset_with(album, &source, &opts)
            .unwrap()
            .asset_id
    };
    let held_back = import(full_album, "full.jpg", b"full album photo");
    let other = import(other_album, "other.jpg", b"other album photo");

    let mock = EscrowMock {
        full_for: full_album,
        seen: std::sync::Arc::default(),
    };
    let origin = serve_escrow_mock(mock.clone()).await;
    let (remote, session) = remote_at(&origin);
    EscrowRun {
        _scratch: scratch,
        ws,
        full_album,
        other_album,
        held_back,
        other,
        mock,
        remote,
        session,
    }
}

/// Every surface at `origin`, and a session resumed against it.
pub(crate) fn remote_at(origin: &str) -> (RemoteConfig, Session) {
    let remote = RemoteConfig {
        auth_endpoint: format!("{origin}/v1/auth"),
        sync_endpoint: origin.to_owned(),
        upload_endpoint: format!("{origin}/v1/upload"),
        albums_endpoint: format!("{origin}/v1/albums"),
        protocol_version: capsule_core::crypto::primitives::PROTOCOL_VERSION.to_owned(),
    };
    let session = AuthClient::new(&remote.auth_endpoint)
        .expect("a base url")
        .resume(capsule_sdk::auth::PersistedSession {
            access_token: "access".to_owned().into(),
            refresh_token: "refresh".to_owned().into(),
            access_expires_at_unix: jiff::Timestamp::now().as_second() + 3600,
        })
        .expect("a resumed session");
    (remote, session)
}

// ── Trash (`capsule trash delete|restore`, #875) ────────────────────────

/// A library holding one imported asset, in the default album.
fn trash_library(scratch: &Scratch) -> (Workspace, Uuid) {
    let lib = scratch.0.join("library");
    std::fs::create_dir_all(&lib).unwrap();
    let mut ws = Workspace::create_with_params(
        &lib,
        b"passphrase",
        capsule_core::crypto::primitives::Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        },
    )
    .unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Default").unwrap();
    let source = scratch.0.join("photo.jpg");
    std::fs::write(&source, b"a photo").unwrap();
    let opts = capsule_library::lifecycle::SignedImportOptions {
        move_source: false,
        defer_source_release: false,
        stack: None,
        enrichment: None,
    };
    let asset = ws
        .import_asset_with(album, &source, &opts)
        .unwrap()
        .asset_id;
    (ws, asset)
}

fn request(action: TrashAction, asset: Uuid) -> TrashRequest {
    TrashRequest {
        action,
        assets: vec![asset],
        retain_days: 30,
    }
}

fn chain_len(ws: &Workspace, asset: &Uuid) -> usize {
    ws.asset(asset).expect("held").chain.records().len()
}

/// The op body for `asset`'s chain head, as a delivery sends it.
fn head_op(ws: &Workspace, asset: &Uuid) -> Vec<u8> {
    let state = ws.asset(asset).expect("held");
    encode_op(
        asset,
        Some(&state.metadata_blob),
        state.chain.records().last().expect("a head"),
    )
    .expect("encodes")
}

fn head_action(ws: &Workspace, asset: &Uuid) -> capsule_core::crypto::provenance::Action {
    ws.asset(asset)
        .expect("held")
        .chain
        .records()
        .last()
        .expect("a head")
        .manifest
        .core
        .action
}

/// A delete signs one record carrying its retention floor, and its op is that record. Run again
/// (a send that did not land), it resends the same head rather than signing a second delete.
#[test]
fn a_trash_delete_signs_one_delete_and_resends_it_on_a_rerun() {
    use capsule_core::crypto::provenance::Action;
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    let before = chain_len(&ws, &asset);
    let now = jiff::Timestamp::now();

    let first = prepare_trash_op(&mut ws, &asset, &request(TrashAction::Delete, asset), now)
        .expect("prepared");
    assert_eq!(head_action(&ws, &asset), Action::Delete);
    assert_eq!(chain_len(&ws, &asset), before + 1);
    let first_op = head_op(&ws, &asset);
    let sent: capsule_sdk::rest::types::OpRequest =
        serde_json::from_slice(&first_op).expect("an op");
    assert_eq!(sent.manifest_envelope.action, "delete");
    assert!(sent.manifest_envelope.retention_until.is_some());
    assert_eq!(
        first.retention_until,
        sent.manifest_envelope.retention_until
    );

    let again = prepare_trash_op(&mut ws, &asset, &request(TrashAction::Delete, asset), now)
        .expect("prepared");
    assert_eq!(chain_len(&ws, &asset), before + 1, "no second delete");
    assert_eq!(again.retention_until, first.retention_until);
    assert_eq!(head_op(&ws, &asset), first_op, "the same head is resent");
}

/// A restore signs a `trash-restore` onto the delete it undoes.
#[test]
fn a_trash_restore_signs_a_restore_onto_the_delete() {
    use capsule_core::crypto::provenance::Action;
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    ws.soft_delete(&asset, 30).expect("deleted");
    let prepared = prepare_trash_op(
        &mut ws,
        &asset,
        &request(TrashAction::TrashRestore, asset),
        jiff::Timestamp::now(),
    )
    .expect("prepared");
    assert_eq!(head_action(&ws, &asset), Action::TrashRestore);
    let sent: capsule_sdk::rest::types::OpRequest =
        serde_json::from_slice(&head_op(&ws, &asset)).expect("an op");
    assert_eq!(sent.manifest_envelope.action, "trash-restore");
    assert!(prepared.retention_until.is_some(), "the delete's floor");
}

/// What the library already knows refuses before anything is signed: an asset it does not
/// hold, a restore of an asset not in the trash, and a restore after the delete's signed
/// retention floor — the last as `retention_elapsed` with the server's code and that floor.
#[test]
fn a_trash_run_refuses_locally_what_it_already_knows_without_signing() {
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    let now = jiff::Timestamp::now();
    let restore = request(TrashAction::TrashRestore, asset);

    let unknown = Uuid::now_v7();
    let failure = prepare_trash_op(&mut ws, &unknown, &restore, now).expect_err("unknown");
    assert_eq!(failure.kind, "not_found");

    let before = chain_len(&ws, &asset);
    let failure = prepare_trash_op(&mut ws, &asset, &restore, now).expect_err("not trashed");
    assert_eq!(failure.kind, "not_in_trash");
    assert!(failure.permanent);
    assert_eq!(chain_len(&ws, &asset), before, "nothing signed");

    ws.soft_delete(&asset, 1).expect("deleted");
    let before = chain_len(&ws, &asset);
    let later = now + jiff::SignedDuration::from_hours(49);
    let failure = prepare_trash_op(&mut ws, &asset, &restore, later).expect_err("too late");
    assert_eq!(failure.kind, "retention_elapsed");
    assert_eq!(
        failure.code.as_deref(),
        Some(capsule_i18n::error_codes::UPLOAD_RETENTION_ELAPSED)
    );
    assert!(failure.permanent);
    assert_eq!(
        failure.retention_until,
        trashed_until(ws.asset(&asset).expect("held"))
    );
    assert!(failure.retention_until.is_some());
    assert_eq!(chain_len(&ws, &asset), before, "nothing signed");
}

/// Every `METHOD path` a mock answered, in order, but the feed pulls.
type Seen = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// The feed entry naming `asset` indexed on the server, its head the record at `at` of the
/// library's chain.
fn listed(ws: &Workspace, asset: &Uuid, at: usize) -> serde_json::Value {
    use base64::Engine as _;
    let state = ws.asset(asset).expect("held");
    let record = &state.chain.records()[at];
    let cbor = capsule_core::cbor::to_canonical_vec(record).expect("encodes");
    serde_json::json!({
        "asset_id": asset.to_string(),
        "album_id": state.album_id.to_string(),
        "protocol_version": capsule_core::crypto::primitives::PROTOCOL_VERSION,
        "sync_seq": at + 1,
        "change": if at == 0 { "created" } else { "updated" },
        "manifest_cbor": base64::engine::general_purpose::STANDARD.encode(cbor),
        "metadata_blob": "metadata",
        "blobs": [],
        "original_held": true,
        "changed_at": "2026-10-01T00:00:00Z",
    })
}

/// The feed's answer to `line` when it is a feed pull: `entries` on the first page, then an
/// empty one. `None` for any other request.
fn feed_answer(line: &str, entries: &[serde_json::Value]) -> Option<(u16, &'static str, String)> {
    if !line.starts_with("GET /v1/sync") {
        return None;
    }
    let first = !line.contains("cursor=");
    let page = serde_json::json!({
        "entries": if first { entries.to_vec() } else { Vec::new() },
        "next_cursor": "after-the-listing",
        "has_more": false,
    });
    Some((200, "application/json", page.to_string()))
}

/// Serve a mock whose feed lists `feed`, and which answers every other request with `status`
/// and `body`; return the endpoints and a session against it, with the requests it answered.
async fn ops_mock(
    status: u16,
    body: serde_json::Value,
    feed: Vec<serde_json::Value>,
) -> (RemoteConfig, Session, Seen) {
    let seen = Seen::default();
    let sink = seen.clone();
    let answer = move |line: &str| {
        if let Some(page) = feed_answer(line, &feed) {
            return page;
        }
        sink.lock().unwrap().push(line.to_owned());
        let content_type = if status == 200 {
            "application/json"
        } else {
            "application/problem+json"
        };
        (status, content_type, body.to_string())
    };
    let (remote, session) = remote_at(&serve_mock(answer).await);
    (remote, session, seen)
}

/// **The refusal this command exists to surface.** The server answers a restore with
/// `409 error.upload.retention_elapsed` — its clock is past the floor this device's was not.
/// The asset is reported failed with the stable `retention_elapsed` kind, the server's code and
/// the delete's signed floor, permanent, and the op was sent exactly once.
#[tokio::test]
async fn a_restore_the_server_refuses_as_too_late_is_a_permanent_retention_elapsed() {
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    ws.soft_delete(&asset, 30).expect("deleted");
    let (remote, session, seen) = ops_mock(
        409,
        serde_json::json!({
            "type": "about:blank",
            "title": "Retention elapsed",
            "status": 409,
            "detail": "this asset's retention window has closed, so it can no longer be restored",
            "code": "error.upload.retention_elapsed",
            "retention_until": "2026-09-01T00:00:00Z",
        }),
        vec![listed(&ws, &asset, 1)],
    )
    .await;

    let summary = trash_with(
        &remote,
        &mut ws,
        session,
        &request(TrashAction::TrashRestore, asset),
    )
    .await;

    assert_eq!((summary.applied, summary.failed), (0, 1));
    let outcome = &summary.outcomes[0];
    assert_eq!(outcome.status, TrashStatus::Failed);
    let error = outcome.error.as_ref().expect("an error");
    assert_eq!(error.kind, "retention_elapsed");
    assert_eq!(
        error.code.as_deref(),
        Some("error.upload.retention_elapsed")
    );
    assert!(error.permanent, "not retried");
    assert_eq!(
        error.retention_until,
        trashed_until(ws.asset(&asset).expect("held"))
    );
    let album = ws.asset(&asset).expect("held").album_id;
    assert_eq!(
        *seen.lock().unwrap(),
        vec![format!("POST /v1/albums/{album}/ops")],
        "sent once, to the asset's album"
    );

    let json = serde_json::to_value(&summary).expect("serializes");
    assert_eq!(json["action"], "trash-restore");
    assert_eq!(json["outcomes"][0]["status"], "failed");
    assert_eq!(json["outcomes"][0]["error"]["kind"], "retention_elapsed");
    assert_eq!(
        json["outcomes"][0]["error"]["code"],
        "error.upload.retention_elapsed"
    );
}

/// An applied delete is reported with the feed position the server gave it.
#[tokio::test]
async fn an_applied_delete_reports_its_feed_position() {
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    let (remote, session, _) = ops_mock(
        200,
        serde_json::json!({
            "asset_id": asset.to_string(),
            "sync_seq": 7,
            "action": "delete",
            "replayed": false,
        }),
        vec![listed(&ws, &asset, 0)],
    )
    .await;

    let summary = trash_with(
        &remote,
        &mut ws,
        session,
        &request(TrashAction::Delete, asset),
    )
    .await;

    assert_eq!((summary.applied, summary.failed), (1, 0));
    assert_eq!(summary.outcomes[0].status, TrashStatus::Applied);
    assert_eq!(summary.outcomes[0].sync_seq, Some(7));
    assert!(
        summary.outcomes[0].retention_until.is_some(),
        "the sent delete's window is reported"
    );
    assert_eq!(
        trash_line(&summary),
        "1 asset moved to the trash on the server"
    );
}

/// An asset already deleted locally (as `capsule cull --sweep` does) is resent with the window
/// its delete was signed with, not `--retain-days`, and the outcome reports the window sent.
#[tokio::test]
async fn a_resent_delete_reports_the_window_it_carries_not_the_flag() {
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    ws.soft_delete(&asset, 5).expect("swept");
    let before = chain_len(&ws, &asset);
    let swept_until = ws
        .asset(&asset)
        .expect("held")
        .chain
        .records()
        .last()
        .expect("a head")
        .manifest
        .core
        .retention_until
        .clone()
        .expect("a delete carries its floor");
    let (remote, session, _) = ops_mock(
        200,
        serde_json::json!({
            "asset_id": asset.to_string(),
            "sync_seq": 8,
            "action": "delete",
            "replayed": false,
        }),
        vec![listed(&ws, &asset, 0)],
    )
    .await;

    // `request` asks for 30 days; the swept delete carries 5.
    let summary = trash_with(
        &remote,
        &mut ws,
        session,
        &request(TrashAction::Delete, asset),
    )
    .await;

    assert_eq!((summary.applied, summary.failed), (1, 0));
    assert_eq!(chain_len(&ws, &asset), before, "no second delete signed");
    assert_eq!(
        summary.outcomes[0].retention_until.as_deref(),
        Some(swept_until.as_str())
    );
    let json = serde_json::to_value(&summary).expect("serializes");
    assert_eq!(json["outcomes"][0]["retention_until"], swept_until);
}

/// Held bytes no resend can fix — not an op, or an op naming no album — are refused for good
/// before anything is sent.
#[tokio::test]
async fn bytes_that_are_not_a_sendable_op_are_refused_for_good() {
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    ws.soft_delete(&asset, 30).expect("deleted");
    let op = delete_op(&ws, &asset).expect("encodes").expect("a delete");
    let mut request: capsule_sdk::rest::types::OpRequest =
        serde_json::from_slice(&op).expect("an op");
    request.manifest_envelope.album_id = None;
    let albumless = serde_json::to_vec(&request).expect("encodes");
    let (remote, session, seen) = ops_mock(500, serde_json::json!({}), Vec::new()).await;

    for bytes in [b"not an op".to_vec(), albumless] {
        let failure = send_op(&remote, session.clone(), &bytes)
            .await
            .expect_err("refused");
        assert_eq!(failure.kind, "not_an_op");
        assert!(failure.permanent);
    }
    assert!(seen.lock().unwrap().is_empty(), "nothing was sent");
}

/// A server fault is not a judgement of the op: it fails the asset without being permanent,
/// so a re-run resends the same head.
#[tokio::test]
async fn a_server_fault_is_a_transient_failure() {
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    let (remote, session, _) = ops_mock(
        500,
        serde_json::json!({
            "type": "about:blank",
            "status": 500,
            "detail": "the lifecycle write could not be applied",
            "code": "error.internal",
        }),
        vec![listed(&ws, &asset, 0)],
    )
    .await;

    let summary = trash_with(
        &remote,
        &mut ws,
        session,
        &request(TrashAction::Delete, asset),
    )
    .await;

    let error = summary.outcomes[0].error.as_ref().expect("an error");
    assert_eq!(error.kind, "unavailable");
    assert!(!error.permanent);
    assert_eq!(error.retention_until, None);
}

// ── Redaction (#994) ────────────────────────────────────────────────────

/// A server holding three upload sessions — two of `asset`'s, one of another asset's — whose
/// cancel answers `204` for `s-open` and `409 session_not_active` for `s-final`, and which
/// applies every lifecycle op; its feed lists `feed`.
async fn redaction_mock(
    asset: Uuid,
    feed: Vec<serde_json::Value>,
) -> (RemoteConfig, Session, Seen) {
    let seen = Seen::default();
    let sink = seen.clone();
    let answer = move |line: &str| {
        if let Some(page) = feed_answer(line, &feed) {
            return page;
        }
        sink.lock().unwrap().push(line.to_owned());
        let session = |id: &str, asset_id: &str, status: &str| {
            serde_json::json!({
                "id": id, "asset_id": asset_id, "album_id": null, "blob_role": "original",
                "status": status, "received_bytes": 4096, "total_size": 8192,
                "created_at": "2026-10-01T00:00:00Z", "last_progress_at": "2026-10-01T00:00:00Z",
            })
        };
        match line {
            "GET /v1/upload/sessions" => {
                let body = serde_json::json!({ "sessions": [
                    session("s-open", &asset.to_string(), "uploading"),
                    session("s-elsewhere", &Uuid::now_v7().to_string(), "uploading"),
                    session("s-final", &asset.to_string(), "waiting_for_processing"),
                ]});
                (200, "application/json", body.to_string())
            }
            "DELETE /v1/upload/s-open" => (204, "application/json", String::new()),
            "DELETE /v1/upload/s-final" => {
                let problem = serde_json::json!({
                    "type": "about:blank", "status": 409, "detail": "Session not active",
                    "code": "error.upload.session_not_active",
                });
                (409, "application/problem+json", problem.to_string())
            }
            line if line.starts_with("POST /v1/albums/") => {
                let applied = serde_json::json!({
                    "asset_id": asset.to_string(), "sync_seq": 9, "action": "delete",
                    "replayed": false,
                });
                (200, "application/json", applied.to_string())
            }
            _ => (501, "application/problem+json", "{}".to_owned()),
        }
    };
    let (remote, session) = remote_at(&serve_mock(answer).await);
    (remote, session, seen)
}

/// **A delete redacts the asset's in-flight upload before the delete is sent.** Each of the
/// asset's open sessions is cancelled, another asset's is not touched, and a session that was
/// already finalizing (`409`) is reported as such rather than failing the delete, which is
/// sent after it so it supersedes the create that session completes.
#[tokio::test]
async fn a_delete_aborts_the_assets_open_upload_sessions_before_sending_the_delete() {
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    let (remote, session, seen) = redaction_mock(asset, vec![listed(&ws, &asset, 0)]).await;

    let summary = trash_with(
        &remote,
        &mut ws,
        session,
        &request(TrashAction::Delete, asset),
    )
    .await;

    assert_eq!((summary.applied, summary.failed), (1, 0), "{summary:#?}");
    assert_eq!(
        summary.outcomes[0].redaction,
        Some(TrashRedaction {
            cancelled: 1,
            finalizing: 1,
            error: None,
        })
    );
    let album = ws.asset(&asset).expect("held").album_id;
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            "GET /v1/upload/sessions".to_owned(),
            "DELETE /v1/upload/s-open".to_owned(),
            "DELETE /v1/upload/s-final".to_owned(),
            format!("POST /v1/albums/{album}/ops"),
        ],
        "the sessions are aborted first, then the delete is sent once"
    );
    let json = serde_json::to_value(&summary).expect("serializes");
    assert_eq!(json["outcomes"][0]["redaction"]["cancelled"], 1);
    assert_eq!(json["outcomes"][0]["redaction"]["finalizing"], 1);
}

/// A redaction that cannot reach the session listing is reported, and the delete is still
/// sent: this device uploads nothing more of a trashed asset either way. A restore redacts
/// nothing.
#[tokio::test]
async fn a_failed_redaction_is_reported_and_the_delete_still_sent() {
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    let (remote, session, seen) = ops_mock(
        200,
        serde_json::json!({
            "asset_id": asset.to_string(),
            "sync_seq": 7,
            "action": "delete",
            "replayed": false,
        }),
        vec![listed(&ws, &asset, 0)],
    )
    .await;

    let summary = trash_with(
        &remote,
        &mut ws,
        session.clone(),
        &request(TrashAction::Delete, asset),
    )
    .await;
    assert_eq!(summary.outcomes[0].status, TrashStatus::Applied);
    let redaction = summary.outcomes[0].redaction.as_ref().expect("a redaction");
    assert!(redaction.error.is_some(), "{redaction:?}");
    assert_eq!((redaction.cancelled, redaction.finalizing), (0, 0));
    assert!(
        seen.lock()
            .unwrap()
            .last()
            .is_some_and(|line| line.starts_with("POST /v1/albums/")),
        "the delete was sent"
    );

    let restored = trash_with(
        &remote,
        &mut ws,
        session,
        &request(TrashAction::TrashRestore, asset),
    )
    .await;
    assert_eq!(
        restored.outcomes[0].redaction, None,
        "a restore redacts nothing"
    );
}

/// **A push skips a trashed asset.** Its upload is redacted: nothing of it is planned or sent,
/// it has no outcome, and the summary counts it apart. A dry run reports it the same way.
#[tokio::test]
async fn a_trashed_asset_is_skipped_by_a_push() {
    for dry_run in [false, true] {
        let EscrowRun {
            _scratch,
            mut ws,
            other_album,
            held_back,
            other,
            mock,
            remote,
            session,
            ..
        } = escrow_run().await;
        ws.soft_delete(&other, 30).expect("deletes");

        let summary = push_with(
            &remote,
            &mut ws,
            session,
            PushOptions {
                dry_run,
                ..PushOptions::default()
            },
            PushRun::default(),
        )
        .await
        .expect("the run reaches its assets");

        assert_eq!(summary.trashed_assets, 1, "{summary:#?}");
        assert_eq!(summary.assets, 2);
        let ids: Vec<&str> = summary
            .outcomes
            .iter()
            .map(|o| o.asset_id.as_str())
            .collect();
        assert_eq!(ids, vec![held_back.to_string()], "dry run: {dry_run}");
        let json = serde_json::to_value(&summary).expect("serializes");
        assert_eq!(json["trashed_assets"], 1);
        assert!(
            custody_line(&summary).contains("Skipped 1 in the trash"),
            "{}",
            custody_line(&summary)
        );
        // The feed lists nothing, so the trashed asset's delete stays held; a dry run reads no
        // server truth for it at all.
        let held: Vec<_> = summary
            .held_records
            .iter()
            .map(|o| {
                (
                    o.asset_id.clone(),
                    o.status,
                    o.held.as_ref().map(|h| h.reason),
                )
            })
            .collect();
        let expected = if dry_run {
            Vec::new()
        } else {
            vec![(
                other.to_string(),
                TrashStatus::Held,
                Some(HoldReason::NotIndexed),
            )]
        };
        assert_eq!(held, expected, "dry run: {dry_run}");
        assert!(
            !mock.seen.lock().unwrap().iter().any(|line| {
                (line.contains(&other_album.to_string()) || line.contains(&other.to_string()))
                    && !line.contains("/escrow/")
            }),
            "nothing of the trashed asset was sent (dry run: {dry_run})"
        );
    }
}

// ── Held lifecycle records (#1029) ──────────────────────────────────────

fn applied_body(asset: Uuid, action: &str) -> serde_json::Value {
    serde_json::json!({
        "asset_id": asset.to_string(),
        "sync_seq": 11,
        "action": action,
        "replayed": false,
    })
}

fn posted_ops(seen: &Seen) -> usize {
    seen.lock()
        .unwrap()
        .iter()
        .filter(|line| line.starts_with("POST /v1/albums/"))
        .count()
}

/// **The server never sees a delete for an asset it has not indexed.** The feed does not list
/// the asset, so its delete is signed in the library and held there: the upload is still
/// redacted, nothing is posted to the ops surface, and the run neither applies nor fails it.
#[tokio::test]
async fn a_delete_of_an_asset_the_server_has_not_indexed_is_held() {
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    let (remote, session, seen) = ops_mock(200, applied_body(asset, "delete"), Vec::new()).await;

    let summary = trash_with(
        &remote,
        &mut ws,
        session,
        &request(TrashAction::Delete, asset),
    )
    .await;

    assert_eq!(
        (summary.applied, summary.held, summary.failed),
        (0, 1, 0),
        "{summary:#?}"
    );
    let outcome = &summary.outcomes[0];
    assert_eq!(outcome.status, TrashStatus::Held);
    assert_eq!(
        outcome.held,
        Some(TrashHold {
            reason: HoldReason::NotIndexed,
            retention_until: None,
        })
    );
    assert!(outcome.redaction.is_some(), "the upload is still redacted");
    assert!(outcome.retention_until.is_some(), "the signed window");
    assert_eq!(
        head_action(&ws, &asset),
        capsule_core::crypto::provenance::Action::Delete,
        "the delete is signed and kept in the library"
    );
    assert_eq!(posted_ops(&seen), 0, "nothing reached the ops surface");
    assert!(
        seen.lock()
            .unwrap()
            .iter()
            .any(|line| line == "GET /v1/upload/sessions"),
        "the redaction ran"
    );
    assert_eq!(
        trash_line(&summary),
        "0 assets moved to the trash on the server, 1 held in the library until the server may \
         take it"
    );
    let json = serde_json::to_value(&summary).expect("serializes");
    assert_eq!(json["held"], 1);
    assert_eq!(json["outcomes"][0]["status"], "held");
    assert_eq!(json["outcomes"][0]["held"]["reason"], "not_indexed");
}

/// A feed that cannot be read says nothing about what the server indexed, so nothing is sent:
/// the asset fails as `unavailable`, which a re-run can clear, and its record stays signed.
#[tokio::test]
async fn an_unreadable_feed_sends_nothing_and_fails_transiently() {
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    let seen = Seen::default();
    let sink = seen.clone();
    let answer = move |line: &str| {
        sink.lock().unwrap().push(line.to_owned());
        let problem = serde_json::json!({
            "type": "about:blank", "status": 500, "detail": "down", "code": "error.internal",
        });
        (500, "application/problem+json", problem.to_string())
    };
    let (remote, session) = remote_at(&serve_mock(answer).await);

    let summary = trash_with(
        &remote,
        &mut ws,
        session,
        &request(TrashAction::Delete, asset),
    )
    .await;

    assert_eq!((summary.applied, summary.held, summary.failed), (0, 0, 1));
    let error = summary.outcomes[0].error.as_ref().expect("an error");
    assert_eq!(error.kind, "unavailable");
    assert!(!error.permanent, "a re-run can send it");
    assert_eq!(posted_ops(&seen), 0, "nothing reached the ops surface");
    assert_eq!(
        head_action(&ws, &asset),
        capsule_core::crypto::provenance::Action::Delete
    );
}

/// Once the create is indexed, a restore made after a held delete sends both, the delete
/// first, so each chains onto the server's head in turn.
#[tokio::test]
async fn a_restore_after_a_held_delete_sends_the_delete_then_the_restore() {
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    let at_create = listed(&ws, &asset, 0);
    ws.soft_delete(&asset, 30).expect("deleted");
    let (remote, session, seen) =
        ops_mock(200, applied_body(asset, "trash-restore"), vec![at_create]).await;

    let summary = trash_with(
        &remote,
        &mut ws,
        session,
        &request(TrashAction::TrashRestore, asset),
    )
    .await;

    assert_eq!(
        (summary.applied, summary.held, summary.failed),
        (1, 0, 0),
        "{summary:#?}"
    );
    assert_eq!(posted_ops(&seen), 2, "the held delete, then the restore");
}

/// **A restore of a held delete is never sent too late.** The server's feed is past the
/// delete's window, so sending the pair would have the server apply the delete, refuse the
/// restore and purge an asset restored in time. Nothing is sent, the asset is reported held
/// with the closed window, and the library keeps both records.
#[tokio::test]
async fn a_restore_of_a_held_delete_past_its_window_is_withheld() {
    let scratch = Scratch::new();
    let (mut ws, asset) = trash_library(&scratch);
    let mut at_create = listed(&ws, &asset, 0);
    at_create["changed_at"] = serde_json::json!("2999-01-01T00:00:00Z");
    ws.soft_delete(&asset, 1).expect("deleted");
    let until = trashed_until(ws.asset(&asset).expect("held"));
    ws.restore(&asset).expect("restored in time");
    let before = chain_len(&ws, &asset);
    let (remote, session, seen) =
        ops_mock(200, applied_body(asset, "trash-restore"), vec![at_create]).await;

    let summary = trash_with(
        &remote,
        &mut ws,
        session,
        &request(TrashAction::TrashRestore, asset),
    )
    .await;

    assert_eq!((summary.applied, summary.held, summary.failed), (0, 1, 0));
    assert_eq!(
        summary.outcomes[0].held,
        Some(TrashHold {
            reason: HoldReason::RestoreTooLate,
            retention_until: until,
        })
    );
    assert_eq!(posted_ops(&seen), 0);
    assert_eq!(chain_len(&ws, &asset), before, "nothing signed or dropped");
}

/// **A push sends a held delete once another device's upload is indexed.** The trashed asset's
/// create is on the feed, so its delete goes to the ops surface, once; the asset is still not
/// uploaded. A second push finds the server level with the library and sends nothing.
#[tokio::test]
async fn a_push_sends_a_held_delete_once_the_create_is_indexed() {
    let EscrowRun {
        _scratch,
        mut ws,
        other,
        ..
    } = escrow_run().await;
    let at_create = listed(&ws, &other, 0);
    ws.soft_delete(&other, 30).expect("deleted");
    let at_delete = listed(&ws, &other, 1);
    let escrow = EscrowMock {
        full_for: Uuid::now_v7(),
        seen: std::sync::Arc::default(),
    };
    let feed = std::sync::Arc::new(std::sync::Mutex::new(vec![at_create]));
    let (seen, listing) = (escrow.seen.clone(), feed.clone());
    let answer = move |line: &str| {
        let entries = listing.lock().unwrap().clone();
        if let Some(page) = feed_answer(line, &entries) {
            return page;
        }
        if line.starts_with("POST /v1/albums/") {
            escrow.seen.lock().unwrap().push(line.to_owned());
            return (
                200,
                "application/json",
                applied_body(other, "delete").to_string(),
            );
        }
        escrow.answer(line)
    };
    let (remote, session) = remote_at(&serve_mock(answer).await);
    let only_other = || PushRun {
        assets: Some(vec![other]),
        ..PushRun::default()
    };

    let summary = push_with(
        &remote,
        &mut ws,
        session.clone(),
        PushOptions::default(),
        only_other(),
    )
    .await
    .expect("the run completes");

    assert_eq!(summary.trashed_assets, 1);
    assert!(summary.outcomes.is_empty(), "nothing of it is uploaded");
    let statuses: Vec<_> = summary.held_records.iter().map(|o| o.status).collect();
    assert_eq!(statuses, vec![TrashStatus::Applied], "{summary:#?}");
    assert_eq!(posted_ops(&seen), 1, "the delete, once");
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("/v1/upload")),
        "no upload session was opened"
    );
    assert!(
        custody_line(&summary).contains("Sent the held delete of 1"),
        "{}",
        custody_line(&summary)
    );

    *feed.lock().unwrap() = vec![at_delete];
    let again = push_with(
        &remote,
        &mut ws,
        session,
        PushOptions::default(),
        only_other(),
    )
    .await
    .expect("the run completes");
    assert!(again.held_records.is_empty(), "{again:#?}");
    assert_eq!(posted_ops(&seen), 1, "nothing sent again");
}

// ── Records after a live asset's create (#1053) ─────────────────────────

/// A mock whose feed lists `feed`, answering every lifecycle op as applied and everything else
/// as the escrow mock does (which refuses `full_for`'s escrow, so that album's assets are never
/// uploaded). Returns the endpoints, a session, and what the mock saw.
async fn live_records_mock(
    asset: Uuid,
    full_for: Uuid,
    feed: Vec<serde_json::Value>,
) -> (RemoteConfig, Session, Seen) {
    let escrow = EscrowMock {
        full_for,
        seen: std::sync::Arc::default(),
    };
    let seen = escrow.seen.clone();
    let answer = move |line: &str| {
        if let Some(page) = feed_answer(line, &feed) {
            return page;
        }
        if line.starts_with("POST /v1/albums/") {
            escrow.seen.lock().unwrap().push(line.to_owned());
            return (
                200,
                "application/json",
                applied_body(asset, "trash-restore").to_string(),
            );
        }
        escrow.answer(line)
    };
    let (remote, session) = remote_at(&serve_mock(answer).await);
    (remote, session, seen)
}

/// **A live asset's chain past its create reaches the server as lifecycle ops** (issue #1053).
/// The asset was deleted and restored after its create; the server indexes the create, so the
/// push sends the delete and then the restore, each chaining onto the one before, and reports
/// them applied. A second push against a server level with the library sends nothing more.
#[tokio::test]
async fn a_push_sends_a_live_assets_records_after_its_indexed_create() {
    use capsule_core::crypto::provenance::Action;
    let EscrowRun {
        _scratch,
        mut ws,
        full_album,
        held_back,
        ..
    } = escrow_run().await;
    let at_create = listed(&ws, &held_back, 0);
    ws.soft_delete(&held_back, 30).expect("deleted");
    ws.restore(&held_back).expect("restored");
    assert_eq!(chain_len(&ws, &held_back), 3);
    let bundle = ws.upload_bundle(&held_back).expect("the create's bundle");
    assert_eq!(
        bundle.action,
        Action::Create,
        "an upload carries the create"
    );
    let (remote, session, seen) = live_records_mock(held_back, full_album, vec![at_create]).await;
    let only = || PushRun {
        assets: Some(vec![held_back]),
        ..PushRun::default()
    };

    let summary = push_with(&remote, &mut ws, session, PushOptions::default(), only())
        .await
        .expect("the run completes");

    assert_eq!(summary.trashed_assets, 0);
    let live: Vec<_> = summary
        .live_records
        .iter()
        .map(|o| (o.asset_id.clone(), o.status, o.retention_until.clone()))
        .collect();
    assert_eq!(
        live,
        vec![(held_back.to_string(), TrashStatus::Applied, None)],
        "{summary:#?}"
    );
    assert_eq!(posted_ops(&seen), 2, "the delete, then the restore");
    assert!(
        custody_line(&summary).contains("Sent the records after the create of 1 assets"),
        "{}",
        custody_line(&summary)
    );
    let json = serde_json::to_value(&summary).expect("serializes");
    assert_eq!(json["live_records"][0]["status"], "applied");

    let at_restore = listed(&ws, &held_back, 2);
    let (remote, session, seen) = live_records_mock(held_back, full_album, vec![at_restore]).await;
    let again = push_with(&remote, &mut ws, session, PushOptions::default(), only())
        .await
        .expect("the run completes");
    assert!(again.live_records.is_empty(), "{again:#?}");
    assert_eq!(posted_ops(&seen), 0, "level with the server: nothing sent");
}

/// Until the server indexes the create, a live asset's later records are held in the library:
/// nothing reaches the ops surface, the asset is reported held as `not_indexed`, and a lone
/// create, which the upload itself delivers, has no entry at all.
#[tokio::test]
async fn a_live_assets_records_wait_for_its_create_to_be_indexed() {
    let EscrowRun {
        _scratch,
        mut ws,
        full_album,
        held_back,
        other,
        ..
    } = escrow_run().await;
    ws.soft_delete(&held_back, 30).expect("deleted");
    ws.restore(&held_back).expect("restored");
    let (remote, session, seen) = live_records_mock(held_back, full_album, Vec::new()).await;

    let summary = push_with(
        &remote,
        &mut ws,
        session,
        PushOptions::default(),
        PushRun {
            assets: Some(vec![held_back, other]),
            ..PushRun::default()
        },
    )
    .await
    .expect("the run completes");

    let live: Vec<_> = summary
        .live_records
        .iter()
        .map(|o| {
            (
                o.asset_id.clone(),
                o.status,
                o.held.as_ref().map(|h| h.reason),
            )
        })
        .collect();
    assert_eq!(
        live,
        vec![(
            held_back.to_string(),
            TrashStatus::Held,
            Some(HoldReason::NotIndexed)
        )],
        "{summary:#?}"
    );
    assert_eq!(posted_ops(&seen), 0, "nothing reached the ops surface");
    assert!(
        custody_line(&summary)
            .contains("1 assets keep the records after their create held in the library"),
        "{}",
        custody_line(&summary)
    );
}

/// `listed`, with the feed also holding the original the create pins.
fn listed_with_original(ws: &Workspace, asset: &Uuid, at: usize) -> serde_json::Value {
    let state = ws.asset(asset).expect("held");
    let create = &state.chain.records()[0].manifest.core;
    let mut entry = listed(ws, asset, at);
    entry["blobs"] = serde_json::json!([{
        "hash": create.ciphertext_hash.to_hex(),
        "role": "original",
        "size": capsule_core::crypto::encryption::ciphertext_len(create.plaintext_size),
    }]);
    entry
}

/// **An asset edited since the server indexed its create is not a failure** (D1 of #1092's
/// review), whether the library kept the create's metadata blob (#1091) or was edited before it
/// kept one. The feed lists the asset and holds its original, so nothing of the create is left
/// to upload. The push reports it `indexed`, sends the edit as a lifecycle op, and exits zero; a
/// dry run plans it the same way.
#[tokio::test]
async fn a_push_skips_an_indexed_create_an_edit_superseded_and_sends_the_edit() {
    indexed_create_an_edit_superseded(false).await;
}

/// [`a_push_skips_an_indexed_create_an_edit_superseded_and_sends_the_edit`] for a library edited
/// before the create's blob was kept: still `indexed` while listed, and, not listed, a failure
/// of kind `bundle`, because its create has to be uploaded and cannot be.
#[tokio::test]
async fn an_indexed_create_with_no_kept_blob_is_skipped_and_fails_once_unlisted() {
    indexed_create_an_edit_superseded(true).await;
}

/// The shared body of the two tests above; `legacy` removes the kept copy of the create's blob.
async fn indexed_create_an_edit_superseded(legacy: bool) {
    let EscrowRun {
        _scratch,
        mut ws,
        full_album,
        held_back,
        ..
    } = escrow_run().await;
    let at_create = listed_with_original(&ws, &held_back, 0);
    ws.tag_add(&held_back, "beach").expect("tagged");
    let kept = ws.create_metadata_blob_path(&held_back).expect("held");
    if legacy {
        std::fs::remove_file(&kept).expect("the edit kept the create's blob");
        assert!(matches!(
            ws.upload_bundle(&held_back),
            Err(LifecycleError::CreateMetadataSuperseded(_))
        ));
    } else {
        let bundle = ws.upload_bundle(&held_back).expect("the create's bundle");
        assert_eq!(
            bundle.metadata_blob_hash,
            ws.asset(&held_back).unwrap().chain.records()[0]
                .manifest
                .core
                .metadata_blob_hash,
            "the bundle carries the blob the create binds, kept by the edit"
        );
    }
    let only = || PushRun {
        assets: Some(vec![held_back]),
        ..PushRun::default()
    };

    let (remote, session, seen) =
        live_records_mock(held_back, full_album, vec![at_create.clone()]).await;
    let dry = push_with(
        &remote,
        &mut ws,
        session,
        PushOptions {
            dry_run: true,
            ..PushOptions::default()
        },
        only(),
    )
    .await
    .expect("the dry run completes");
    assert_eq!(dry.outcomes[0].status, AssetPushStatus::Indexed, "{dry:#?}");
    assert_eq!(dry.failed_assets, 0);
    assert_eq!(posted_ops(&seen), 0, "a dry run sends nothing");

    let (remote, session, seen) = live_records_mock(held_back, full_album, vec![at_create]).await;
    let summary = push_with(&remote, &mut ws, session, PushOptions::default(), only())
        .await
        .expect("the run completes");

    assert_eq!(
        summary.outcomes[0].status,
        AssetPushStatus::Indexed,
        "{summary:#?}"
    );
    assert_eq!((summary.indexed_assets, summary.failed_assets), (1, 0));
    assert_eq!(failed_run_reason(&summary), None);
    let live: Vec<_> = summary
        .live_records
        .iter()
        .map(|o| (o.asset_id.clone(), o.status))
        .collect();
    assert_eq!(live, vec![(held_back.to_string(), TrashStatus::Applied)]);
    assert_eq!(posted_ops(&seen), 1, "the edit");
    assert!(
        custody_line(&summary).contains("Skipped 1 the server already holds"),
        "{}",
        custody_line(&summary)
    );
    let json = serde_json::to_value(&summary).expect("serializes");
    assert_eq!(json["outcomes"][0]["status"], "indexed");

    let (remote, session, _seen) = live_records_mock(held_back, full_album, Vec::new()).await;
    let unlisted = push_with(&remote, &mut ws, session, PushOptions::default(), only())
        .await
        .expect("the run completes");
    let kind = unlisted.outcomes[0].error.as_ref().map(|e| e.kind);
    if legacy {
        assert_eq!(unlisted.failed_assets, 1, "{unlisted:#?}");
        assert_eq!(kind, Some("bundle"));
    } else {
        assert_ne!(
            kind,
            Some("bundle"),
            "the kept blob builds the create's bundle"
        );
    }
}

/// **A feed that cannot be read after the uploads sends nothing** (`deliver_live_records`):
/// whether the server indexed the create is unknown, so each live asset with records past its
/// create is reported `unavailable`, not permanent, and a re-run clears it.
#[tokio::test]
async fn a_live_assets_records_are_unavailable_when_the_redrain_fails() {
    let EscrowRun {
        _scratch,
        mut ws,
        full_album,
        held_back,
        ..
    } = escrow_run().await;
    let at_create = listed(&ws, &held_back, 0);
    ws.soft_delete(&held_back, 30).expect("deleted");
    ws.restore(&held_back).expect("restored");
    let escrow = EscrowMock {
        full_for: full_album,
        seen: std::sync::Arc::default(),
    };
    let seen = escrow.seen.clone();
    let drains = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let answer = move |line: &str| {
        if line.starts_with("GET /v1/sync") && !line.contains("cursor=") {
            // The drain before the uploads reads the listing; the one after them fails.
            let drain = drains.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if drain > 0 {
                let problem = serde_json::json!({
                    "type": "about:blank",
                    "title": "Bad Request",
                    "status": 400,
                    "detail": "the feed is unavailable",
                });
                return (400, "application/problem+json", problem.to_string());
            }
        }
        if let Some(page) = feed_answer(line, std::slice::from_ref(&at_create)) {
            return page;
        }
        escrow.answer(line)
    };
    let (remote, session) = remote_at(&serve_mock(answer).await);

    let summary = push_with(
        &remote,
        &mut ws,
        session,
        PushOptions::default(),
        PushRun {
            assets: Some(vec![held_back]),
            ..PushRun::default()
        },
    )
    .await
    .expect("the run completes");

    let live: Vec<_> = summary
        .live_records
        .iter()
        .map(|o| {
            (
                o.asset_id.clone(),
                o.status,
                o.error.as_ref().map(|e| (e.kind, e.permanent)),
            )
        })
        .collect();
    assert_eq!(
        live,
        vec![(
            held_back.to_string(),
            TrashStatus::Failed,
            Some(("unavailable", false))
        )],
        "{summary:#?}"
    );
    assert_eq!(posted_ops(&seen), 0, "nothing reached the ops surface");
}

/// An asset handed to another device is left to its hand-off: its records after the create are
/// neither sent nor reported, even when the server indexes the create.
#[tokio::test]
async fn a_delegated_assets_records_are_left_to_its_hand_off() {
    let EscrowRun {
        _scratch,
        mut ws,
        full_album,
        held_back,
        ..
    } = escrow_run().await;
    let at_create = listed(&ws, &held_back, 0);
    ws.soft_delete(&held_back, 30).expect("deleted");
    ws.restore(&held_back).expect("restored");
    let (remote, session, seen) = live_records_mock(held_back, full_album, vec![at_create]).await;

    let summary = push_with(
        &remote,
        &mut ws,
        session,
        PushOptions::default(),
        PushRun {
            assets: Some(vec![held_back]),
            delegated: BTreeSet::from([held_back]),
            ..PushRun::default()
        },
    )
    .await
    .expect("the run completes");

    assert_eq!(summary.outcomes[0].status, AssetPushStatus::Delegated);
    assert!(summary.live_records.is_empty(), "{summary:#?}");
    assert_eq!(posted_ops(&seen), 0, "nothing reached the ops surface");
}

/// A feed entry naming `asset` at `record`, a record of its chain read before a purge.
fn listed_record(
    album: Uuid,
    asset: Uuid,
    record: &capsule_core::crypto::provenance::ProvenanceRecord,
    seq: usize,
) -> serde_json::Value {
    use base64::Engine as _;
    let cbor = capsule_core::cbor::to_canonical_vec(record).expect("encodes");
    serde_json::json!({
        "asset_id": asset.to_string(),
        "album_id": album.to_string(),
        "protocol_version": capsule_core::crypto::primitives::PROTOCOL_VERSION,
        "sync_seq": seq,
        "change": "updated",
        "manifest_cbor": base64::engine::general_purpose::STANDARD.encode(cbor),
        "metadata_blob": "metadata",
        "blobs": [],
        "original_held": true,
        "changed_at": "2026-10-01T00:00:00Z",
    })
}

/// **A purged asset's held records are sent from its tombstone.** The origin deleted and then
/// purged the asset before any upload of it was indexed, so the delete and the early purge's
/// shorter-retention re-issue were both held. Once another device's upload of the create is on
/// the feed, a push sends both from the kept chain, in chain order, and reports the asset sent;
/// the server then purges by the re-issued window. A second push finds the server level with
/// the tombstone and sends nothing. While the feed does not list the asset, nothing is sent and
/// nothing is reported.
#[tokio::test]
async fn a_push_sends_a_purged_assets_held_records_from_its_tombstone() {
    use capsule_core::crypto::provenance::Action;
    let EscrowRun {
        _scratch,
        mut ws,
        other_album,
        held_back,
        other,
        ..
    } = escrow_run().await;
    // In the trash too, so a whole-library run uploads nothing and reads only held records.
    ws.soft_delete(&held_back, 30).expect("deleted");
    let at_create = listed(&ws, &other, 0);
    ws.soft_delete(&other, 30).expect("deleted");
    let reissue = ws
        .purge(&other)
        .expect("purged early")
        .reissue
        .expect("a shorter-retention re-issue");
    assert!(ws.asset(&other).is_none(), "the asset left the library");
    let kept = ws.purged_chain(&other).expect("the tombstone").to_vec();
    let actions: Vec<_> = kept.iter().map(|r| r.manifest.core.action).collect();
    assert_eq!(
        actions,
        vec![Action::Create, Action::Delete, Action::Delete]
    );
    let at_reissue = listed_record(other_album, other, &reissue, 3);

    let escrow = EscrowMock {
        full_for: Uuid::now_v7(),
        seen: std::sync::Arc::default(),
    };
    let feed = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let (seen, listing) = (escrow.seen.clone(), feed.clone());
    let answer = move |line: &str| {
        let entries = listing.lock().unwrap().clone();
        if let Some(page) = feed_answer(line, &entries) {
            return page;
        }
        if line.starts_with("POST /v1/albums/") {
            escrow.seen.lock().unwrap().push(line.to_owned());
            return (
                200,
                "application/json",
                applied_body(other, "delete").to_string(),
            );
        }
        escrow.answer(line)
    };
    let (remote, session) = remote_at(&serve_mock(answer).await);

    // Not indexed: nothing to send, nothing reported.
    let unlisted = push_with(
        &remote,
        &mut ws,
        session.clone(),
        PushOptions::default(),
        PushRun::default(),
    )
    .await
    .expect("the run completes");
    assert!(
        unlisted
            .held_records
            .iter()
            .all(|o| o.asset_id != other.to_string()),
        "{unlisted:#?}"
    );
    assert_eq!(posted_ops(&seen), 0);

    *feed.lock().unwrap() = vec![at_create];
    let summary = push_with(
        &remote,
        &mut ws,
        session.clone(),
        PushOptions::default(),
        PushRun {
            assets: Some(vec![other]),
            ..PushRun::default()
        },
    )
    .await
    .expect("the run completes");
    assert_eq!(summary.assets, 0, "a purged asset has nothing to upload");
    assert!(summary.outcomes.is_empty(), "{summary:#?}");
    let held: Vec<_> = summary
        .held_records
        .iter()
        .map(|o| (o.asset_id.clone(), o.status, o.retention_until.clone()))
        .collect();
    assert_eq!(
        held,
        vec![(
            other.to_string(),
            TrashStatus::Applied,
            reissue.manifest.core.retention_until.clone()
        )],
        "sent, reporting the re-issued window"
    );
    assert_eq!(posted_ops(&seen), 2, "the held delete, then the re-issue");
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("/v1/upload")),
        "no upload session was opened"
    );

    *feed.lock().unwrap() = vec![at_reissue];
    let again = push_with(
        &remote,
        &mut ws,
        session,
        PushOptions::default(),
        PushRun::default(),
    )
    .await
    .expect("the run completes");
    assert!(
        again
            .held_records
            .iter()
            .all(|o| o.asset_id != other.to_string()),
        "{again:#?}"
    );
    assert_eq!(posted_ops(&seen), 2, "nothing sent again");
}
