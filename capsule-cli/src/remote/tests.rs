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
/// for an asset the library cannot build a bundle for.
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
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
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
                let (status, content_type, body) = mock.answer(&line);
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
    let remote = RemoteConfig {
        auth_endpoint: format!("{origin}/v1/auth"),
        sync_endpoint: origin.clone(),
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
