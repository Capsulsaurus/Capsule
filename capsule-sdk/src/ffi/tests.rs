//! Host-verifiable smoke for the S-D9 FFI surface and the `S-P1` workspace verbs.
//!
//! The native (Swift + Kotlin) harnesses drive the same flows against a dev server
//! *through the generated bindings* — that is the platform CI's half and needs native
//! toolchains. This is the Rust-side half of both Done-whens: it exercises the
//! FFI-exposed functions directly (the exact `fn`s and `async fn`s uniffi wraps)
//! against the crate's shared in-process mock server ([`crate::testmock`], the same
//! one the upload, push, album, and directory client tests drive), so the surface is
//! proven wired end to end without a native toolchain.
//!
//! `S-D9` (the networked half): login → upload → status, plus the `duplicate_blob`
//! merge and the typed-error paths.
//!
//! `S-P1` (the workspace half): the whole critical-path loop —
//! **enroll → album → seal+import → upload → sync-apply** — driven through
//! [`FfiWorkspace`] and [`FfiSession`], plus escrow put/get and device-directory
//! publish. Every verb reaches `capsule-core` for its crypto; what is under test here
//! is the wiring, the shapes, and the verdicts.
//!
//! `sync_pull` itself rides the generated `GET /v1/sync` operation (`S-D28` retired the gRPC
//! feed) and is exercised over a socket in `capsule-server/tests/sdk_client.rs` against the
//! real router; its Rust-side shape is compiled here (the surface builds) but not behaviorally
//! driven — the sync-apply test below feeds `apply_sync_entry` the exact three byte strings a
//! feed entry carries, which is the half `S-P1` owns.

use std::sync::Arc;

use super::*;
// One mock server for the whole crate, not one per module: `testmock` replays the real
// wire — statuses, headers, and the `ApiError` JSON with its stable `error.*` codes — and is
// shared with the upload, push, album, and directory client tests.
use crate::testmock::{MockRequest, MockResponse, MockServer};

/// The session handle from a login that finished.
///
/// Panics rather than returning a `Result`: a fixture that answers a second-factor challenge
/// when it meant to answer a token pair has nothing left for the case to assert.
fn finished(outcome: FfiLoginOutcome) -> Arc<FfiSession> {
    match outcome {
        FfiLoginOutcome::Session { session } => session,
        FfiLoginOutcome::SecondFactorRequired { .. } => {
            panic!("the fixture answered a second-factor challenge")
        }
    }
}

const PROTOCOL: &str = "2026-07-10";

// ─── Fixtures ────────────────────────────────────────────────────────────────

fn far_future() -> i64 {
    jiff::Timestamp::now().as_second() + 3600
}

fn token_json() -> String {
    serde_json::json!({
        "access_token": "access-1",
        "refresh_token": "refresh-1",
        "token_type": "Bearer",
        "expires_by": far_future(),
    })
    .to_string()
}

fn ffi_request(size: u64) -> FfiUploadRequest {
    FfiUploadRequest {
        size,
        hash: "0".repeat(64),
        content_type: "image/jpeg".into(),
        crypto_suite_id: 1,
        protocol_version: PROTOCOL.into(),
        blob_role: FfiBlobRole::Original,
        manifest_envelope: FfiManifestEnvelope {
            crypto_suite_id: 1,
            protocol_version: PROTOCOL.into(),
            album_id: Some("album-1".into()),
            file_id: "0192f000-0000-7000-8000-000000000001".into(),
            amk_version: 1,
            ciphertext_hash: "0".repeat(64),
            plaintext_size: size,
            chunk_size: 65536,
            key_mode: "derived".into(),
            metadata_blob_hash: None,
            derivative_role: None,
            derivative_format: None,
            created_by_user: "user-1".into(),
            created_by_device: "device-1".into(),
            client_version: "capsule-test/0".into(),
            timestamp: "2026-07-10T00:00:00Z".into(),
            action: "create".into(),
            prior_provenance_hash: None,
            retention_until: None,
        },
        album_id: Some("album-1".into()),
        owner_id: None,
        intent_id: None,
    }
}

// ─── The FFI-exposed flow: login → upload → status ───────────────────────────

/// The whole Done-when host-verifiable half: build the FFI client, `login` through
/// the exported entry point to get a session **handle**, `upload` a blob through the
/// handle, then `upload_status` (HEAD) — all against one mock server replaying the
/// real wire. No token ever leaves the handle.
#[tokio::test]
async fn ffi_login_upload_status_round_trip() {
    const SIZE: u64 = 4096;
    let server =
        MockServer::start(
            |req: &MockRequest| match (req.method.as_str(), req.path.as_str()) {
                ("POST", "/auth/login") => MockResponse::new(200, "OK").json_body(token_json()),
                ("POST", "/auth/logout") => {
                    MockResponse::new(200, "OK").json_body(r#"{"error":"ok"}"#)
                }
                // Fresh upload session (offset 0).
                ("POST", "/upload") => MockResponse::new(201, "Created")
                    .header("X-Capsule-Offset", "0")
                    .json_body(
                        serde_json::json!({
                            "id": "sess-1",
                            "upload_url": "/upload/sess-1",
                            "suggested_chunk_size": 4096,
                        })
                        .to_string(),
                    ),
                // Chunk accepted; no offset header → the client advances by chunk length.
                ("PATCH", "/upload/sess-1") => MockResponse::new(200, "OK"),
                // Status (HEAD): fully received, finalizing.
                ("HEAD", "/upload/sess-1") => MockResponse::new(200, "OK")
                    .header("X-Capsule-Offset", SIZE.to_string())
                    .header("X-Capsule-Content-Length", SIZE.to_string())
                    .header("X-Capsule-Upload-Status", "finalizing"),
                _ => MockResponse::new(404, "Not Found").json_body(r#"{"error":"no route"}"#),
            },
        )
        .await;

    let client = FfiCapsuleClient::new(
        format!("{}/auth", server.base_url()),
        format!("{}/upload", server.base_url()),
        PROTOCOL.to_string(),
        None,
    )
    .unwrap();

    // login → session handle (tokens stay inside the handle).
    let session = finished(
        client
            .login("a@example.com".into(), "pw".into())
            .await
            .unwrap(),
    );
    assert!(session.is_authenticated().await);

    // upload file through the handle.
    let outcome = session
        .upload(ffi_request(SIZE), vec![7u8; SIZE as usize])
        .await
        .unwrap();
    let session_id = match outcome {
        FfiUploadOutcome::Completed { session_id } => session_id,
        FfiUploadOutcome::AlreadyStored { asset_ref } => panic!("unexpected merge: {asset_ref}"),
    };
    assert_eq!(session_id, "sess-1");

    // upload status (HEAD) through the handle.
    let status = session
        .upload_status(session_id)
        .await
        .unwrap()
        .expect("session present");
    assert_eq!(status.offset, SIZE);
    assert_eq!(status.total_size, Some(SIZE));
    assert_eq!(status.status, "finalizing");

    // logout clears the handle.
    session.logout().await.unwrap();
    assert!(!session.is_authenticated().await);
}

/// A `duplicate_blob` on create resolves through the FFI as the merge outcome, not a
/// transfer — the exact `error.*`-code-switched path the upload client owns.
#[tokio::test]
async fn ffi_upload_surfaces_duplicate_blob_merge() {
    let server = MockServer::start(|req: &MockRequest| match (req.method.as_str(), req.path.as_str())
    {
        ("POST", "/auth/login") => MockResponse::new(200, "OK").json_body(token_json()),
        ("POST", "/upload") => MockResponse::new(409, "Conflict").json_body(
            r#"{"error":"This content is already stored as asset asset-xyz","code":"error.upload.duplicate_blob"}"#,
        ),
        _ => MockResponse::new(404, "Not Found").json_body(r#"{"error":"no route"}"#),
    })
    .await;

    let client = FfiCapsuleClient::new(
        format!("{}/auth", server.base_url()),
        format!("{}/upload", server.base_url()),
        PROTOCOL.to_string(),
        None,
    )
    .unwrap();
    let session = finished(
        client
            .login("a@example.com".into(), "pw".into())
            .await
            .unwrap(),
    );

    let outcome = session
        .upload(ffi_request(4096), vec![7u8; 4096])
        .await
        .unwrap();
    match outcome {
        FfiUploadOutcome::AlreadyStored { asset_ref } => assert_eq!(asset_ref, "asset-xyz"),
        FfiUploadOutcome::Completed { .. } => panic!("expected a merge, not a transfer"),
    }
}

/// Login failure maps to a typed `FfiError::Auth` carrying the stable catalog code
/// (which clients localize) — never a bare status.
#[tokio::test]
async fn ffi_login_invalid_credentials_carries_catalog_code() {
    let server =
        MockServer::start(
            |req: &MockRequest| match (req.method.as_str(), req.path.as_str()) {
                ("POST", "/auth/login") => MockResponse::new(401, "Unauthorized")
                    .json_body(r#"{"error":"Invalid credentials"}"#),
                _ => MockResponse::new(404, "Not Found").json_body(r#"{"error":"no route"}"#),
            },
        )
        .await;

    let client = FfiCapsuleClient::new(
        format!("{}/auth", server.base_url()),
        format!("{}/upload", server.base_url()),
        PROTOCOL.to_string(),
        None,
    )
    .unwrap();

    // The Ok type (`Arc<FfiSession>`) is not Debug, so match rather than `unwrap_err`.
    match client.login("a@example.com".into(), "bad".into()).await {
        Err(FfiError::Auth { code, .. }) => assert_eq!(
            code.as_deref(),
            Some(capsule_i18n::error_codes::AUTH_INVALID_CREDENTIALS)
        ),
        Err(other) => panic!("expected FfiError::Auth, got {other:?}"),
        Ok(_) => panic!("expected invalid-credentials to fail"),
    }
}

/// A bad base URL is rejected at construction as `InvalidArgument`, before any call.
#[test]
fn ffi_client_rejects_invalid_base_url() {
    // `Arc<FfiCapsuleClient>` (the Ok type) is not Debug, so match rather than
    // `unwrap_err`.
    match FfiCapsuleClient::new(
        "not a url".into(),
        "also bad".into(),
        PROTOCOL.to_string(),
        None,
    ) {
        Err(FfiError::InvalidArgument { .. }) => {}
        Err(other) => panic!("expected InvalidArgument, got {other:?}"),
        Ok(_) => panic!("expected an invalid-base-url rejection"),
    }
}

/// A derivative's role/format pair survives both `FfiManifestEnvelope` conversions, so a
/// derivative upload that crosses the FFI still declares what the server's gate requires.
#[test]
fn ffi_manifest_envelope_carries_the_derivative_pair_both_ways() {
    let mut ffi = ffi_request(4096).manifest_envelope;
    ffi.derivative_role = Some("thumbnail".into());
    ffi.derivative_format = Some("image/jxl".into());

    let sdk = ManifestEnvelope::from(ffi);
    assert_eq!(sdk.derivative_role.as_deref(), Some("thumbnail"));
    assert_eq!(sdk.derivative_format.as_deref(), Some("image/jxl"));

    let back = FfiManifestEnvelope::from(sdk);
    assert_eq!(back.derivative_role.as_deref(), Some("thumbnail"));
    assert_eq!(back.derivative_format.as_deref(), Some("image/jxl"));
}

/// A sidecar with an as-imported capture time and, when `corrected` is given, a user correction
/// in `capture_timestamp_user`.
fn sidecar_with_capture(corrected: Option<&str>) -> capsule_core::sidecar::SidecarV1 {
    use capsule_core::metadata::crdt::{Lww, OrSet};
    use capsule_core::sidecar::{Regions, SIDECAR_SCHEMA_V1, SidecarV1};

    let mut capture_timestamp_user = Lww::new();
    if let Some(corrected) = corrected {
        capture_timestamp_user.set(
            corrected.to_string(),
            "2026-06-01T00:00:00Z",
            uuid::Uuid::from_u128(0xD1),
        );
    }
    SidecarV1 {
        sidecar_schema: SIDECAR_SCHEMA_V1,
        crypto_suite_id: capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
        uuid: uuid::Uuid::from_u128(0x7777),
        hash: capsule_core::crypto::hash::Hash32([0xAB; 32]),
        capture_timestamp: "2021-06-01T12:00:00Z".into(),
        capture_timestamp_user,
        import_timestamp: "2026-05-31T11:00:00Z".into(),
        content_type: "image/jpeg".into(),
        dimensions: None,
        lqip: None,
        tags_user: OrSet::new(),
        tags_ai: OrSet::new(),
        caption: Lww::new(),
        rating: Lww::new(),
        stack_membership: Lww::new(),
        regions: Regions::new(),
        cull: Lww::new(),
        hidden: Lww::new(),
        camera_id: None,
        device_id: uuid::Uuid::from_u128(0xD1),
        session_id: uuid::Uuid::from_u128(0x5E),
        gps: None,
        gps_user: Lww::new(),
        provenance_chain_hash: None,
        unknown: std::collections::BTreeMap::new(),
        signature: None,
    }
}

/// The record an app upserts dates the asset by the user's correction when the sidecar carries
/// one, and by the as-imported capture time otherwise.
#[test]
fn ffi_asset_metadata_reports_the_effective_capture_time() {
    let corrected = FfiAssetMetadata::from(sidecar_with_capture(Some("2001-09-09T01:46:40Z")));
    assert_eq!(corrected.capture_timestamp, "2001-09-09T01:46:40Z");

    let imported = FfiAssetMetadata::from(sidecar_with_capture(None));
    assert_eq!(imported.capture_timestamp, "2021-06-01T12:00:00Z");
}

// ─── S-P1: the workspace verbs ───────────────────────────────────────────────

/// The app's build identity every manifest this workspace authors reports (S-D15).
fn client_build() -> FfiClientBuild {
    FfiClientBuild {
        client_id: "capsule-ffi-test".into(),
        semver: "0.0.1".into(),
    }
}

/// A workspace enrolled at a fresh temp root. `LowRam` is the cheapest sanctioned
/// Argon2id tier; the FFI constructors deliberately expose only real tiers, so the test
/// pays one real password hash rather than reaching behind the surface.
fn enroll(root: &std::path::Path) -> Arc<FfiWorkspace> {
    FfiWorkspace::create(
        root.to_string_lossy().into_owned(),
        b"test-passphrase".to_vec(),
        FfiDeviceTier::LowRam,
        client_build(),
    )
    .expect("enrollment succeeds")
}

/// The mock the flow tests drive: login, an upload session per blob (the ladder opens
/// one `POST /upload` for the metadata blob and one for the original), plus the escrow
/// and device-directory endpoints.
///
/// The escrow endpoints are **stateful** — a `PUT` stores the bytes verbatim and a `GET`
/// serves them back, exactly as the single-active-escrow contract says — so `escrow_put`
/// and `escrow_get` can be asserted as a real round trip rather than two isolated calls.
///
/// They are served on `/v1/auth/escrow` under the API root, which is the path the committed
/// document declares and the generated client therefore requests. The `PUT` answers a JSON
/// `StoreEscrowResponse` and the empty-escrow `GET` answers an RFC 9457 problem, because a
/// generated operation decodes both — a bare `204` or a body-less `404` would arrive as a
/// decode failure rather than as the typed outcome this test is asserting.
async fn flow_server() -> MockServer {
    let escrow: Arc<std::sync::Mutex<Vec<u8>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    MockServer::start(
        move |req: &MockRequest| match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/auth/login") => MockResponse::new(200, "OK").json_body(token_json()),
            ("POST", "/upload") => MockResponse::new(201, "Created")
                .header("X-Capsule-Offset", "0")
                .json_body(
                    serde_json::json!({
                        "id": "sess-1",
                        "upload_url": "/upload/sess-1",
                        "suggested_chunk_size": 262_144,
                    })
                    .to_string(),
                ),
            ("PATCH", "/upload/sess-1") => MockResponse::new(200, "OK"),
            ("PUT", "/api/v1/auth/escrow") => {
                let replaced = if let Ok(mut stored) = escrow.lock() {
                    let replaced = !stored.is_empty();
                    stored.clone_from(&req.body);
                    replaced
                } else {
                    false
                };
                MockResponse::new(200, "OK").json_body(
                    serde_json::json!({
                        "stored_at": "2026-01-01T00:00:00Z",
                        "replaced": replaced,
                    })
                    .to_string(),
                )
            }
            ("GET", "/api/v1/auth/escrow") => {
                let stored = escrow.lock().map(|s| s.clone()).unwrap_or_default();
                if stored.is_empty() {
                    // Nothing enrolled yet — the typed `NotEnrolled` path.
                    MockResponse::new(404, "Not Found").json_body(
                        serde_json::json!({
                            "type": "about:blank",
                            "title": "Not found",
                            "status": 404,
                            "detail": "no escrow has been stored for this account",
                            "code": "error.escrow.not_stored",
                        })
                        .to_string(),
                    )
                } else {
                    let mut response = MockResponse::new(200, "OK")
                        .header("Content-Type", "application/octet-stream");
                    response.body = stored;
                    response
                }
            }
            // As the real server does, a publish without the identity key is a `400`: the
            // document carries the key's signature but not the key.
            ("POST", "/api/devices/directory") => {
                if req.header("X-Capsule-Identity-Key").is_some() {
                    MockResponse::new(200, "OK").json_body(r#"{"directory_version":1}"#)
                } else {
                    MockResponse::new(400, "Bad Request")
                }
            }
            _ => MockResponse::new(404, "Not Found").json_body(r#"{"error":"no route"}"#),
        },
    )
    .await
}

/// **The `S-P1` Done-when.** One flow, through the FFI types only:
///
/// 1. **enroll** — `FfiWorkspace::create` mints the account, device key, signed device
///    directory, and durable album keystore at a fresh root;
/// 2. **album** — `ensure_album` mints AMK_v1, its write-tier and admin keys, and an
///    attested authority;
/// 3. **seal + import** — `seal_asset` takes bytes (as PhotoKit would hand them over),
///    STREAM-encrypts, signs the create manifest, seals the signed sidecar into its
///    metadata blob, and self-verifies through the `verify_asset` chokepoint;
/// 4. **upload** — `upload_blobs` projects that asset into `POST /upload` bodies in
///    ladder order and `FfiSession::upload` drives each against the mock server;
/// 5. **sync-apply** — the very bytes a feed entry carries for this asset are fed back
///    through `apply_sync_entry`, which re-runs the chokepoint and returns the
///    decrypted, signature-checked facts a catalog upserts.
///
/// Step 5 passes `local_chain_head: None` because it models the *receiving* catalog — a
/// device that has never seen this asset. Feeding the head back makes the same entry a
/// replay, which the last assertion pins.
#[tokio::test]
async fn ffi_enroll_album_seal_upload_sync_apply_round_trip() {
    const PLAINTEXT: &[u8] = b"\xFF\xD8\xFF\xE0 photo bytes handed over by the platform";

    let lib = tempfile::TempDir::new().unwrap();
    let server = flow_server().await;

    // 1. Enroll.
    let workspace = enroll(lib.path());
    assert!(!workspace.user_id().unwrap().is_empty());
    assert!(!workspace.device_id().unwrap().is_empty());
    // The signed device directory exists from enrollment — it is what makes this
    // device's signatures resolvable to anyone else.
    assert!(!workspace.signed_device_directory().unwrap().is_empty());

    // 2. Album. `ensure_album` over the derived default id is the first-run verb, and it
    //    is idempotent across relaunches where `create_album` would refuse.
    let album = workspace
        .ensure_album(workspace.default_album_id().unwrap(), "Camera Roll".into())
        .unwrap();
    assert_eq!(workspace.albums().unwrap().len(), 1);

    // 3. Seal + import.
    let asset = workspace
        .seal_asset(album.clone(), "IMG_0042.JPG".into(), PLAINTEXT.to_vec())
        .unwrap();
    assert!(matches!(
        workspace.verify_asset(asset.clone()).unwrap(),
        FfiVerifyOutcome::Accept
    ));
    assert_eq!(workspace.read_plaintext(asset.clone()).unwrap(), PLAINTEXT);

    // 4. Upload, in ladder order, through the session handle.
    let session = FfiCapsuleClient::new(
        format!("{}/auth", server.base_url()),
        format!("{}/upload", server.base_url()),
        PROTOCOL.to_string(),
        None,
    )
    .unwrap()
    .login("a@example.com".into(), "pw".into())
    .await
    .map(finished)
    .unwrap();

    let blobs = workspace.upload_blobs(asset.clone()).unwrap();
    assert_eq!(
        blobs.iter().map(|b| b.tier.as_str()).collect::<Vec<_>>(),
        vec!["index", "index", "original"],
        "T0 is two blobs — provenance then metadata, the pair the server needs before it may \
         publish the asset — and precedes T2; no derivatives without a codec"
    );
    assert!(
        matches!(
            (
                &blobs[0].request.blob_role,
                &blobs[1].request.blob_role,
                &blobs[2].request.blob_role,
            ),
            (
                FfiBlobRole::Provenance,
                FfiBlobRole::Metadata,
                FfiBlobRole::Original
            )
        ),
        "the index tier is provenance then metadata; the original is T2"
    );
    // Keep the wire bytes the feed would carry for this asset before consuming the blobs.
    let metadata_blob = blobs[1].bytes.clone();
    let ciphertext = blobs[2].bytes.clone();
    for blob in blobs {
        // Every envelope names *this* blob's content address (the server's invariant-15
        // consistency rule) while carrying the head manifest's fields verbatim.
        assert_eq!(blob.request.manifest_envelope.ciphertext_hash, blob.hash);
        assert_eq!(blob.request.manifest_envelope.file_id, asset);
        assert_eq!(blob.request.album_id.as_deref(), Some(album.as_str()));
        match session.upload(blob.request, blob.bytes).await.unwrap() {
            FfiUploadOutcome::Completed { session_id } => assert_eq!(session_id, "sess-1"),
            FfiUploadOutcome::AlreadyStored { asset_ref } => {
                panic!("unexpected merge: {asset_ref}")
            }
        }
    }

    // 5. Sync-apply: exactly the three byte strings a feed entry carries.
    let album_bytes = uuid::Uuid::parse_str(&album).unwrap().as_bytes().to_vec();
    // The feed carries the provenance record, not the bare manifest — the record is what the
    // server's chain head hashes and what `apply_sync_entry` decodes.
    let manifest_cbor = workspace.provenance_head(asset.clone()).unwrap();
    let entry = || FfiSyncEntry {
        album_id: album_bytes.clone(),
        manifest_cbor: manifest_cbor.clone(),
        metadata_blob: metadata_blob.clone(),
        original_ciphertext: ciphertext.clone(),
        local_chain_head: None,
    };

    let facts = match workspace.apply_sync_entry(entry()).unwrap() {
        FfiSyncApplyOutcome::Applied { facts } => facts,
        other => panic!("expected the entry to apply, got {other:?}"),
    };
    assert_eq!(facts.asset_id, asset);
    assert_eq!(facts.album_id, album);
    assert_eq!(facts.action, "create");
    assert_eq!(facts.amk_version, 1);
    assert_eq!(facts.plaintext_size, PLAINTEXT.len() as u64);
    // The metadata blob really was opened: these fields exist only inside it.
    let metadata = facts
        .metadata
        .as_ref()
        .expect("a create carries decrypted metadata");
    assert_eq!(metadata.content_type, "image/jpeg");
    assert_eq!(metadata.sidecar_schema, 1);
    assert!(!metadata.capture_timestamp.is_empty());
    assert_eq!(facts.provenance_head.len(), 32);

    // Replaying the same entry against a catalog that now holds it is quarantined by the
    // chokepoint — never silently re-applied.
    let replay = FfiSyncEntry {
        local_chain_head: Some(facts.provenance_head.clone()),
        ..entry()
    };
    match workspace.apply_sync_entry(replay).unwrap() {
        FfiSyncApplyOutcome::Quarantined { reason, .. } => {
            assert_eq!(reason, "Rejected.Replayed");
        }
        other => panic!("expected a replay quarantine, got {other:?}"),
    }
}

/// A tampered original quarantines with the chokepoint's own reason code rather than
/// erroring — one hostile feed row must not abort a sync page.
#[tokio::test]
async fn ffi_sync_apply_quarantines_a_tampered_entry() {
    let lib = tempfile::TempDir::new().unwrap();
    let workspace = enroll(lib.path());
    let album = workspace.create_album("Trip".into()).unwrap();
    let asset = workspace
        .seal_asset(
            album.clone(),
            "shot.jpg".into(),
            b"\xFF\xD8\xFF bytes".to_vec(),
        )
        .unwrap();

    let blobs = workspace.upload_blobs(asset.clone()).unwrap();
    let mut ciphertext = blobs[1].bytes.clone();
    ciphertext[0] ^= 0xFF;

    let outcome = workspace
        .apply_sync_entry(FfiSyncEntry {
            album_id: uuid::Uuid::parse_str(&album).unwrap().as_bytes().to_vec(),
            manifest_cbor: workspace.provenance_head(asset).unwrap(),
            metadata_blob: blobs[0].bytes.clone(),
            original_ciphertext: ciphertext,
            local_chain_head: None,
        })
        .unwrap();
    match outcome {
        FfiSyncApplyOutcome::Quarantined { reason, .. } => {
            assert_eq!(reason, "Rejected.CiphertextHashMismatch");
        }
        other => panic!("expected a quarantine, got {other:?}"),
    }
}

/// A record from a newer writer reaches the app as the stable `UnsupportedRecordVersion`
/// code with both versions in the detail, never as `MalformedManifest` and never applied.
#[tokio::test]
async fn ffi_sync_apply_quarantines_a_newer_record_version() {
    let lib = tempfile::TempDir::new().unwrap();
    let workspace = enroll(lib.path());
    let album = workspace.create_album("Trip".into()).unwrap();
    let asset = workspace
        .seal_asset(
            album.clone(),
            "shot.jpg".into(),
            b"\xFF\xD8\xFF bytes".to_vec(),
        )
        .unwrap();
    let blobs = workspace.upload_blobs(asset.clone()).unwrap();

    // Canonical CBOR sorts `version` first, so the record's leading key is the text
    // `version` followed by its current value; bump that one byte to the next version.
    let current = u8::try_from(capsule_core::crypto::provenance::PROVENANCE_RECORD_VERSION)
        .expect("the current record version fits a one-byte CBOR integer");
    let mut leading = vec![0x67];
    leading.extend_from_slice(b"version");
    leading.push(current);
    let mut manifest_cbor = workspace.provenance_head(asset).unwrap();
    let at = manifest_cbor
        .windows(leading.len())
        .position(|w| w == leading.as_slice())
        .expect("the record carries a leading version key");
    assert_eq!(at, 1, "version is the record's first key");
    manifest_cbor[at + leading.len() - 1] = current + 1;

    let outcome = workspace
        .apply_sync_entry(FfiSyncEntry {
            album_id: uuid::Uuid::parse_str(&album).unwrap().as_bytes().to_vec(),
            manifest_cbor,
            metadata_blob: blobs[0].bytes.clone(),
            original_ciphertext: blobs[1].bytes.clone(),
            local_chain_head: None,
        })
        .unwrap();
    match outcome {
        FfiSyncApplyOutcome::Quarantined { reason, detail } => {
            assert_eq!(reason, "UnsupportedRecordVersion");
            assert_eq!(
                detail.as_deref(),
                Some(
                    format!(
                        "record version {} is newer than this build reads ({current})",
                        current + 1
                    )
                    .as_str()
                )
            );
        }
        other => panic!("expected an unsupported-version quarantine, got {other:?}"),
    }
}

// ─── Bounded-memory push and fetch of originals ─────────────────────────────

/// One chunk the mock upload server received: the session it belongs to, the offset it
/// declared, and its body.
type ReceivedChunk = (String, u64, Vec<u8>);

/// An upload mock that opens a distinct session per `POST /upload` and records every chunk,
/// so a test can reassemble one blob and measure the largest window the client sent.
async fn recording_upload_server(chunks: Arc<std::sync::Mutex<Vec<ReceivedChunk>>>) -> MockServer {
    let sessions = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    MockServer::start(
        move |req: &MockRequest| match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/auth/login") => MockResponse::new(200, "OK").json_body(token_json()),
            ("POST", "/upload") => {
                let id = format!(
                    "sess-{}",
                    sessions.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                );
                MockResponse::new(201, "Created")
                    .header("X-Capsule-Offset", "0")
                    .json_body(
                        serde_json::json!({
                            "id": id,
                            "upload_url": format!("/upload/{id}"),
                            "suggested_chunk_size": 262_144,
                        })
                        .to_string(),
                    )
            }
            ("PATCH", path) if path.starts_with("/upload/sess-") => {
                let offset = req
                    .header("X-Capsule-Offset")
                    .and_then(|o| o.parse().ok())
                    .unwrap_or(u64::MAX);
                let session = path.trim_start_matches("/upload/").to_string();
                if let Ok(mut chunks) = chunks.lock() {
                    chunks.push((session, offset, req.body.clone()));
                }
                MockResponse::new(200, "OK")
            }
            _ => MockResponse::new(404, "Not Found").json_body(r#"{"error":"no route"}"#),
        },
    )
    .await
}

async fn login(server: &MockServer) -> Arc<FfiSession> {
    FfiCapsuleClient::new(
        format!("{}/auth", server.base_url()),
        format!("{}/upload", server.base_url()),
        PROTOCOL.to_string(),
        None,
    )
    .unwrap()
    .login("a@example.com".into(), "pw".into())
    .await
    .map(finished)
    .unwrap()
}

/// A JPEG-tagged plaintext of `len` bytes no codec decodes, so the bundle is the index tier
/// and the original alone.
fn original_plaintext(len: usize) -> Vec<u8> {
    let mut bytes: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    bytes[..3].copy_from_slice(b"\xFF\xD8\xFF");
    bytes
}

/// **The bounded-memory push.** `push_asset` drives a sealed asset's whole ladder inside the
/// SDK, and the original reaches the wire in windows: every chunk the server receives is at
/// most the upload strategy's 1 MiB ceiling for an original of this size class, the same
/// ceiling for a 2 MiB and a 6 MiB original, and each original is several times larger than
/// the largest chunk. The reassembled chunks hash to the original's content address, so the
/// windows are the signed ciphertext, in order, with nothing missing.
#[tokio::test]
async fn ffi_push_asset_streams_the_original_in_bounded_windows() {
    const WINDOW_CEILING: usize = 1024 * 1024;

    let lib = tempfile::TempDir::new().unwrap();
    let workspace = enroll(lib.path());
    let album = workspace
        .ensure_album(workspace.default_album_id().unwrap(), "Camera Roll".into())
        .unwrap();
    let chunks = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = recording_upload_server(chunks.clone()).await;
    let session = login(&server).await;

    for len in [2 * 1024 * 1024, 6 * 1024 * 1024] {
        let asset = workspace
            .seal_asset(
                album.clone(),
                "IMG_0042.JPG".into(),
                original_plaintext(len),
            )
            .unwrap();
        chunks.lock().unwrap().clear();

        let report = session
            .push_asset(workspace.clone(), asset.clone())
            .await
            .unwrap();
        assert_eq!(report.asset_id, asset);
        assert_eq!(
            report
                .pushed
                .iter()
                .map(|b| b.tier.as_str())
                .collect::<Vec<_>>(),
            vec!["index", "index", "original"],
            "the ladder is provenance and metadata, then the original"
        );
        let original = &report.pushed[2];
        let FfiUploadOutcome::Completed { session_id } = &original.outcome else {
            panic!("the original transferred rather than merged");
        };

        let received = chunks.lock().unwrap().clone();
        let original_chunks: Vec<&ReceivedChunk> = received
            .iter()
            .filter(|(s, _, _)| s == session_id)
            .collect();
        let mut reassembled = Vec::new();
        for (_, offset, body) in &original_chunks {
            assert_eq!(*offset, reassembled.len() as u64, "chunks arrive in order");
            reassembled.extend_from_slice(body);
        }
        let largest = original_chunks
            .iter()
            .map(|(_, _, body)| body.len())
            .max()
            .unwrap();
        assert!(
            reassembled.len() >= len,
            "the ciphertext covers the plaintext"
        );
        assert_eq!(
            capsule_core::crypto::hash::hash_bytes(&reassembled).to_hex(),
            original.hash,
            "the windows reassemble to the signed ciphertext"
        );
        assert!(
            largest <= WINDOW_CEILING,
            "a {len}-byte original went out in a {largest}-byte chunk"
        );
        assert!(
            reassembled.len() > 2 * largest,
            "the original ({} bytes) is several times its largest chunk ({largest})",
            reassembled.len()
        );
        assert!(report.bytes >= reassembled.len() as u64);
    }
}

/// An unknown asset is refused before any request, and a malformed id is the caller's.
#[tokio::test]
async fn ffi_push_asset_refuses_an_unknown_asset() {
    let lib = tempfile::TempDir::new().unwrap();
    let workspace = enroll(lib.path());
    let chunks = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = recording_upload_server(chunks.clone()).await;
    let session = login(&server).await;

    match session
        .push_asset(workspace.clone(), "not-a-uuid".into())
        .await
    {
        Err(FfiError::InvalidArgument { .. }) => {}
        other => panic!("expected InvalidArgument, got {:?}", other.err()),
    }
    match session
        .push_asset(workspace, uuid::Uuid::now_v7().to_string())
        .await
    {
        Err(FfiError::Workspace { .. }) => {}
        other => panic!("expected a workspace error, got {:?}", other.err()),
    }
    assert!(
        chunks.lock().unwrap().is_empty(),
        "nothing reached the wire"
    );
}

/// A blob mock serving `blob` at `/v1/blob/{hash}` by `Range`, recording each requested
/// window's length (`None` for an open-ended range).
async fn blob_server(
    blob: Vec<u8>,
    hash: String,
    ranges: Arc<std::sync::Mutex<Vec<Option<u64>>>>,
) -> MockServer {
    MockServer::start(
        move |req: &MockRequest| match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/auth/login") => MockResponse::new(200, "OK").json_body(token_json()),
            ("GET", path) if path == format!("/v1/blob/{hash}") => {
                let range = req
                    .header("Range")
                    .and_then(|r| r.strip_prefix("bytes="))
                    .and_then(|r| r.split_once('-'))
                    .unwrap_or(("0", ""));
                let start: usize = range.0.parse().unwrap_or(0);
                let end: Option<usize> = range.1.parse().ok();
                if let Ok(mut ranges) = ranges.lock() {
                    ranges.push(end.map(|e| (e + 1 - start) as u64));
                }
                let end = end.map_or(blob.len(), |e| (e + 1).min(blob.len()));
                let mut response = MockResponse::new(206, "Partial Content")
                    .header("Content-Type", "application/octet-stream");
                response.body = blob[start..end].to_vec();
                response
            }
            _ => MockResponse::new(404, "Not Found").json_body(r#"{"error":"no route"}"#),
        },
    )
    .await
}

/// **The bounded-memory receive.** `fetch_blob_to_path` lands a blob on disk in `Range`
/// windows no larger than the streamed-fetch window, and the file holds exactly the bytes
/// whose content address was asked for.
#[tokio::test]
async fn ffi_fetch_blob_to_path_streams_a_verified_blob_to_disk() {
    let blob = original_plaintext(3 * 1024 * 1024);
    let hash = capsule_core::crypto::hash::hash_bytes(&blob).to_hex();
    let ranges = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = blob_server(blob.clone(), hash.clone(), ranges.clone()).await;
    let session = login(&server).await;
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("original.bin");

    session
        .fetch_blob_to_path(
            format!("{}/v1", server.base_url()),
            hash,
            blob.len() as u64,
            path.to_string_lossy().into_owned(),
        )
        .await
        .unwrap();

    assert_eq!(std::fs::read(&path).unwrap(), blob);
    let ranges = ranges.lock().unwrap().clone();
    assert!(!ranges.is_empty());
    assert!(
        ranges
            .iter()
            .all(|len| len.is_some_and(|l| l <= crate::fetch::SINK_WINDOW)),
        "every request is a bounded window: {ranges:?}"
    );
}

/// Bytes that do not hash to the requested address are refused and leave no file behind;
/// a missing blob likewise; a malformed hash never reaches the network.
#[tokio::test]
async fn ffi_fetch_blob_to_path_leaves_nothing_on_failure() {
    let blob = original_plaintext(64 * 1024);
    // Served under this address, but holding other bytes.
    let claimed = capsule_core::crypto::hash::hash_bytes(b"something else").to_hex();
    let ranges = Arc::new(std::sync::Mutex::new(Vec::new()));
    let server = blob_server(blob.clone(), claimed.clone(), ranges.clone()).await;
    let session = login(&server).await;
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("original.bin");
    let base = format!("{}/v1", server.base_url());
    let target = path.to_string_lossy().into_owned();

    match session
        .fetch_blob_to_path(base.clone(), claimed, blob.len() as u64, target.clone())
        .await
    {
        Err(FfiError::Fetch { message, .. }) => assert!(message.contains("integrity")),
        other => panic!("expected an integrity failure, got {other:?}"),
    }
    assert!(!path.exists(), "an unverified blob is not left on disk");

    let absent = capsule_core::crypto::hash::hash_bytes(b"absent").to_hex();
    match session
        .fetch_blob_to_path(base.clone(), absent, 10, target.clone())
        .await
    {
        Err(FfiError::Fetch { .. }) => {}
        other => panic!("expected a fetch failure, got {other:?}"),
    }
    assert!(!path.exists());

    let requests = ranges.lock().unwrap().len();
    match session
        .fetch_blob_to_path(base, "../../etc/passwd".into(), 10, target)
        .await
    {
        Err(FfiError::InvalidArgument { .. }) => {}
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
    assert_eq!(
        ranges.lock().unwrap().len(),
        requests,
        "nothing was requested"
    );
}

/// A fetch whose session cannot refresh surfaces as `FfiError::Auth` carrying the refusal,
/// as an upload or a sync pull does — the app signs in again rather than retrying — and
/// leaves no file behind.
#[tokio::test]
async fn ffi_fetch_blob_to_path_keeps_a_refused_refresh_an_auth_error() {
    let server =
        MockServer::start(
            |req: &MockRequest| match (req.method.as_str(), req.path.as_str()) {
                ("POST", "/auth/login") => MockResponse::new(200, "OK").json_body(token_json()),
                ("POST", "/auth/refresh") => MockResponse::api_error(
                    401,
                    "Unauthorized",
                    "error.auth.session_expired",
                    "refresh token revoked",
                ),
                ("GET", path) if path.starts_with("/v1/blob/") => {
                    MockResponse::new(401, "Unauthorized")
                }
                _ => MockResponse::new(404, "Not Found").json_body(r#"{"error":"no route"}"#),
            },
        )
        .await;
    let session = login(&server).await;
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("original.bin");

    match session
        .fetch_blob_to_path(
            format!("{}/v1", server.base_url()),
            capsule_core::crypto::hash::hash_bytes(b"blob").to_hex(),
            4,
            path.to_string_lossy().into_owned(),
        )
        .await
    {
        Err(FfiError::Auth { .. }) => {}
        other => panic!("expected an auth error, got {other:?}"),
    }
    assert!(!path.exists());
}

/// A refresh that fails for a temporary reason (here a `503`) is a transient
/// `FfiError::Fetch`, not a sign-in prompt, and leaves no file behind.
#[tokio::test]
async fn ffi_fetch_blob_to_path_keeps_a_failing_refresh_transient() {
    let server =
        MockServer::start(
            |req: &MockRequest| match (req.method.as_str(), req.path.as_str()) {
                ("POST", "/auth/login") => MockResponse::new(200, "OK").json_body(token_json()),
                ("POST", "/auth/refresh") => MockResponse::api_error(
                    503,
                    "Service Unavailable",
                    "error.server.unavailable",
                    "maintenance",
                ),
                ("GET", path) if path.starts_with("/v1/blob/") => {
                    MockResponse::new(401, "Unauthorized")
                }
                _ => MockResponse::new(404, "Not Found").json_body(r#"{"error":"no route"}"#),
            },
        )
        .await;
    let session = login(&server).await;
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("original.bin");

    match session
        .fetch_blob_to_path(
            format!("{}/v1", server.base_url()),
            capsule_core::crypto::hash::hash_bytes(b"blob").to_hex(),
            4,
            path.to_string_lossy().into_owned(),
        )
        .await
    {
        Err(FfiError::Fetch { message, .. }) => assert!(message.contains("transient")),
        other => panic!("expected a transient fetch error, got {other:?}"),
    }
    assert!(!path.exists());
}

// ─── Bounded-memory apply and open of a synced original ─────────────────────

/// Heap accounting for the bounded-memory tests: every allocation and free made **on the
/// calling thread** is tallied, so a test reads its own peak without interference from the
/// tests running beside it. It wraps the system allocator and changes nothing else.
mod heap {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;

    struct Counting;

    thread_local! {
        static LIVE: Cell<isize> = const { Cell::new(0) };
        static PEAK: Cell<isize> = const { Cell::new(0) };
    }

    fn track(delta: isize) {
        // `try_with`: an allocation during thread teardown must not panic.
        let _ = LIVE.try_with(|live| {
            let now = live.get() + delta;
            live.set(now);
            let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
        });
    }

    // SAFETY: every method delegates to `System` with the caller's own arguments and only adds
    // thread-local bookkeeping, so `System`'s guarantees pass through unchanged.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            // SAFETY: forwarded verbatim; the caller upholds `alloc`'s contract.
            let ptr = unsafe { System.alloc(layout) };
            if !ptr.is_null() {
                track(layout.size() as isize);
            }
            ptr
        }

        unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
            // SAFETY: forwarded verbatim; the caller upholds `dealloc`'s contract.
            unsafe { System.dealloc(ptr, layout) };
            track(-(layout.size() as isize));
        }

        unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
            // SAFETY: forwarded verbatim; the caller upholds `realloc`'s contract.
            let new = unsafe { System.realloc(ptr, layout, new_size) };
            if !new.is_null() {
                track(new_size as isize - layout.size() as isize);
            }
            new
        }
    }

    #[global_allocator]
    static COUNTING: Counting = Counting;

    /// Run `f` and return its result with the peak heap it grew this thread by, in bytes.
    pub(super) fn peak_during<T>(f: impl FnOnce() -> T) -> (T, usize) {
        let base = LIVE.with(Cell::get);
        PEAK.with(|peak| peak.set(base));
        let out = f();
        let peak = PEAK.with(Cell::get);
        (out, usize::try_from(peak - base).unwrap_or(0))
    }
}

/// A sealed asset's feed head and the path of its original ciphertext, written to `dir` as
/// `fetch_blob_to_path` would have left it. The in-memory copy is dropped before returning.
struct SyncedOriginal {
    asset: String,
    head: FfiSyncHead,
    ciphertext_path: std::path::PathBuf,
    ciphertext_len: usize,
}

fn synced_original(
    workspace: &FfiWorkspace,
    album: &str,
    plaintext: Vec<u8>,
    dir: &std::path::Path,
) -> SyncedOriginal {
    let asset = workspace
        .seal_asset(album.to_string(), "VID_0042.JPG".into(), plaintext)
        .unwrap();
    let mut blobs = workspace.upload_blobs(asset.clone()).unwrap();
    assert_eq!(
        blobs.iter().map(|b| b.tier.as_str()).collect::<Vec<_>>(),
        vec!["index", "index", "original"]
    );
    let original = blobs.pop().unwrap();
    let ciphertext_path = dir.join(format!("{}.blob", original.hash));
    std::fs::write(&ciphertext_path, &original.bytes).unwrap();
    SyncedOriginal {
        head: FfiSyncHead {
            album_id: uuid::Uuid::parse_str(album).unwrap().as_bytes().to_vec(),
            manifest_cbor: workspace.provenance_head(asset.clone()).unwrap(),
            metadata_blob: blobs[1].bytes.clone(),
            local_chain_head: None,
        },
        asset,
        ciphertext_path,
        ciphertext_len: original.bytes.len(),
    }
}

fn path_string(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The names in `dir`, sorted.
fn dir_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// **The bounded-memory apply and open.** An original many STREAM chunks and read windows long
/// is applied and opened from its ciphertext file holding a few chunks of it: the peak heap of
/// each verb stays under a ceiling that is a fraction of the smaller original, and the larger
/// original's peak is the smaller's within one chunk, so buffering does not grow with the
/// original. The opened file is the original byte for byte, the apply's facts are
/// `apply_sync_entry`'s, and no staging file is left behind.
#[test]
fn ffi_apply_and_open_a_synced_original_from_disk_in_bounded_memory() {
    use capsule_core::crypto::encryption::CIPHERTEXT_CHUNK;
    const CEILING: usize = 512 * 1024;
    const SMALL: usize = 2 * 1024 * 1024;
    const LARGE: usize = 6 * 1024 * 1024;

    let lib = tempfile::TempDir::new().unwrap();
    let blobs = tempfile::TempDir::new().unwrap();
    let out = tempfile::TempDir::new().unwrap();
    let workspace = enroll(lib.path());
    let album = workspace
        .ensure_album(workspace.default_album_id().unwrap(), "Camera Roll".into())
        .unwrap();

    let mut peaks = Vec::new();
    let mut opened_names = Vec::new();
    for len in [SMALL, LARGE] {
        let synced = synced_original(&workspace, &album, original_plaintext(len), blobs.path());
        assert!(
            synced.ciphertext_len > 4 * CEILING,
            "the original dwarfs the ceiling, which is itself several STREAM chunks and read \
             windows"
        );
        const { assert!(CEILING > 4 * CIPHERTEXT_CHUNK) };
        let destination = out.path().join(format!("{}.jpg", synced.asset));
        // The control: the meter sees a whole-original buffer, so a verb's low peak is real.
        let (_, whole) = heap::peak_during(|| std::fs::read(&synced.ciphertext_path).unwrap());
        assert!(whole >= synced.ciphertext_len, "the heap meter counts");

        let (applied, apply_peak) = heap::peak_during(|| {
            workspace.apply_sync_entry_from_file(
                synced.head.clone(),
                path_string(&synced.ciphertext_path),
            )
        });
        let facts = match applied.unwrap() {
            FfiSyncApplyOutcome::Applied { facts } => facts,
            other => panic!("expected the entry to apply, got {other:?}"),
        };
        assert_eq!(facts.asset_id, synced.asset);
        assert_eq!(facts.plaintext_size, len as u64);

        let (opened, open_peak) = heap::peak_during(|| {
            workspace.open_synced_original(
                synced.head.clone(),
                path_string(&synced.ciphertext_path),
                path_string(&destination),
            )
        });
        match opened.unwrap() {
            FfiOpenedOriginal::Opened { facts: opened } => {
                assert_eq!(opened.asset_id, synced.asset);
                assert_eq!(opened.provenance_head, facts.provenance_head);
            }
            other => panic!("expected the original to open, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(&destination).unwrap(),
            workspace.read_plaintext(synced.asset.clone()).unwrap(),
            "the destination holds the original byte for byte"
        );

        for (verb, peak) in [("apply", apply_peak), ("open", open_peak)] {
            assert!(
                peak < CEILING,
                "{verb} peaked at {peak} B, over the {CEILING} B ceiling, for a {} B original",
                synced.ciphertext_len
            );
        }
        peaks.push((apply_peak, open_peak));
        opened_names.push(format!("{}.jpg", synced.asset));
    }
    let [(small_apply, small_open), (large_apply, large_open)] = peaks[..] else {
        panic!("two sizes measured");
    };
    assert!(
        large_apply <= small_apply + CIPHERTEXT_CHUNK
            && large_open <= small_open + CIPHERTEXT_CHUNK,
        "buffering grew with the original: apply {small_apply} -> {large_apply} B, open \
         {small_open} -> {large_open} B"
    );
    opened_names.sort();
    assert_eq!(
        dir_names(out.path()),
        opened_names,
        "only the opened originals are left"
    );
}

/// A tampered ciphertext file quarantines with the code `apply_sync_entry` gives the same
/// bytes, through both verbs, and the open leaves nothing at the destination — not a partial
/// original, not a staging file, and not a change to a file already there.
#[test]
fn ffi_a_tampered_ciphertext_file_quarantines_and_places_nothing() {
    let lib = tempfile::TempDir::new().unwrap();
    let blobs = tempfile::TempDir::new().unwrap();
    let out = tempfile::TempDir::new().unwrap();
    let workspace = enroll(lib.path());
    let album = workspace.create_album("Trip".into()).unwrap();
    let synced = synced_original(
        &workspace,
        &album,
        original_plaintext(300 * 1024),
        blobs.path(),
    );
    let mut ciphertext = std::fs::read(&synced.ciphertext_path).unwrap();
    // Past the first chunk, so the decryptor has already written plaintext when it fails.
    let at = ciphertext.len() - 10;
    ciphertext[at] ^= 0xFF;
    std::fs::write(&synced.ciphertext_path, &ciphertext).unwrap();

    let in_memory = workspace
        .apply_sync_entry(FfiSyncEntry {
            album_id: synced.head.album_id.clone(),
            manifest_cbor: synced.head.manifest_cbor.clone(),
            metadata_blob: synced.head.metadata_blob.clone(),
            original_ciphertext: ciphertext,
            local_chain_head: None,
        })
        .unwrap();
    let FfiSyncApplyOutcome::Quarantined {
        reason: expected, ..
    } = in_memory
    else {
        panic!("expected apply_sync_entry to quarantine, got {in_memory:?}");
    };
    assert_eq!(expected, "Rejected.CiphertextHashMismatch");

    match workspace
        .apply_sync_entry_from_file(synced.head.clone(), path_string(&synced.ciphertext_path))
        .unwrap()
    {
        FfiSyncApplyOutcome::Quarantined { reason, .. } => assert_eq!(reason, expected),
        other => panic!("expected a quarantine, got {other:?}"),
    }

    let fresh = out.path().join("fresh.jpg");
    let existing = out.path().join("existing.jpg");
    std::fs::write(&existing, b"what the app already had").unwrap();
    for destination in [&fresh, &existing] {
        match workspace
            .open_synced_original(
                synced.head.clone(),
                path_string(&synced.ciphertext_path),
                path_string(destination),
            )
            .unwrap()
        {
            FfiOpenedOriginal::Refused {
                outcome: FfiSyncApplyOutcome::Quarantined { reason, .. },
            } => assert_eq!(reason, expected),
            other => panic!("expected a quarantine, got {other:?}"),
        }
    }
    assert!(!fresh.exists(), "nothing is placed at the destination");
    assert_eq!(
        std::fs::read(&existing).unwrap(),
        b"what the app already had",
        "a refused original replaces nothing"
    );
    assert_eq!(
        dir_names(out.path()),
        vec!["existing.jpg".to_string()],
        "no staging file is left behind"
    );
}

/// The verbs refuse what they cannot act on as typed errors: a missing ciphertext file, a
/// destination that names no file, and a malformed provenance head.
#[test]
fn ffi_apply_and_open_from_disk_surface_bad_arguments_as_errors() {
    let lib = tempfile::TempDir::new().unwrap();
    let blobs = tempfile::TempDir::new().unwrap();
    let workspace = enroll(lib.path());
    let album = workspace.create_album("Trip".into()).unwrap();
    let synced = synced_original(&workspace, &album, original_plaintext(1024), blobs.path());
    let missing = path_string(&blobs.path().join("missing.blob"));
    let missing_path = missing.clone();
    let ciphertext = path_string(&synced.ciphertext_path);

    match workspace.apply_sync_entry_from_file(synced.head.clone(), missing.clone()) {
        Err(FfiError::Workspace { message }) => assert!(message.contains("missing.blob")),
        other => panic!("expected a workspace error, got {other:?}"),
    }
    match workspace.open_synced_original(
        synced.head.clone(),
        missing,
        path_string(&blobs.path().join("out.jpg")),
    ) {
        Err(FfiError::Workspace { .. }) => {}
        other => panic!("expected a workspace error, got {other:?}"),
    }
    match workspace.open_synced_original(synced.head.clone(), ciphertext.clone(), "/".into()) {
        Err(FfiError::InvalidArgument { .. }) => {}
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
    let bad_head = FfiSyncHead {
        local_chain_head: Some(vec![0; 31]),
        ..synced.head.clone()
    };
    match workspace.apply_sync_entry_from_file(bad_head.clone(), ciphertext) {
        Err(FfiError::InvalidArgument { .. }) => {}
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
    // A malformed argument is refused before any file is touched: paired with a ciphertext
    // path that does not exist, it is still InvalidArgument, never the file's error.
    let bad_album = FfiSyncHead {
        album_id: vec![0; 3],
        ..synced.head
    };
    for head in [bad_head, bad_album] {
        match workspace.apply_sync_entry_from_file(head.clone(), missing_path.clone()) {
            Err(FfiError::InvalidArgument { .. }) => {}
            other => panic!("expected InvalidArgument before the file is read, got {other:?}"),
        }
        match workspace.open_synced_original(
            head,
            missing_path.clone(),
            path_string(&blobs.path().join("out.jpg")),
        ) {
            Err(FfiError::InvalidArgument { .. }) => {}
            other => panic!("expected InvalidArgument before the file is read, got {other:?}"),
        }
    }
    assert!(!blobs.path().join("out.jpg").exists());
}

/// Escrow put/get and device-directory publish, through the session handle. The
/// workspace mints both documents (the master key and the identity key never cross the
/// boundary); the session only ever carries opaque bytes.
#[tokio::test]
async fn ffi_escrow_and_device_directory_round_trip() {
    let lib = tempfile::TempDir::new().unwrap();
    let workspace = enroll(lib.path());
    let recovery_secret = vec![0x5Au8; 32];

    // The escrow blob is minted inside core; what comes out is the wrapped document.
    let blob = workspace
        .escrow_blob(recovery_secret.clone(), FfiDeviceTier::LowRam)
        .unwrap();
    assert!(!blob.is_empty());
    // It opens under the secret it was minted with, and under nothing else.
    assert!(
        workspace
            .verify_escrow_blob(blob.clone(), recovery_secret.clone())
            .unwrap()
    );
    assert!(
        !workspace
            .verify_escrow_blob(blob.clone(), b"the wrong secret".to_vec())
            .unwrap()
    );

    let server = flow_server().await;
    let session = FfiCapsuleClient::new(
        format!("{}/auth", server.base_url()),
        format!("{}/upload", server.base_url()),
        PROTOCOL.to_string(),
        None,
    )
    .unwrap()
    .login("a@example.com".into(), "pw".into())
    .await
    .map(finished)
    .unwrap();
    let api_base = format!("{}/api", server.base_url());

    // Nothing enrolled yet: the fetch is a typed escrow failure, not a panic or an empty blob.
    assert!(matches!(
        session.escrow_get(api_base.clone()).await,
        Err(FfiError::Escrow { .. })
    ));

    // put → get round-trips the opaque document byte-for-byte, and what comes back still
    // opens under the recovery secret — which is the whole point of storing it.
    let first = session
        .escrow_put(api_base.clone(), blob.clone())
        .await
        .unwrap();
    assert_eq!(
        first,
        FfiEscrowStored {
            stored_at: "2026-01-01T00:00:00Z".to_owned(),
            replaced: false,
        },
        "a first escrow is recovery set up, with the server's stored_at as RFC 3339"
    );
    // Storing again over it is a replacement — the event that retires the old secret.
    let second = session
        .escrow_put(api_base.clone(), blob.clone())
        .await
        .unwrap();
    assert!(second.replaced, "a second escrow replaces the first");
    let fetched = session.escrow_get(api_base.clone()).await.unwrap();
    assert_eq!(fetched, blob, "the escrow blob is stored verbatim");
    assert!(
        workspace
            .verify_escrow_blob(fetched, recovery_secret)
            .unwrap()
    );

    // The identity key crosses the boundary as the workspace's public bytes; anything that is
    // not a hybrid verifying key is refused before a request is made.
    assert!(matches!(
        session
            .publish_device_directory(
                api_base.clone(),
                workspace.signed_device_directory().unwrap(),
                b"not a verifying key".to_vec(),
            )
            .await,
        Err(FfiError::InvalidArgument { .. })
    ));
    let version = session
        .publish_device_directory(
            api_base,
            workspace.signed_device_directory().unwrap(),
            workspace.user_ik_public().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(version, 1);
}

// ─── S-P1: hardware-signer constructor parity ────────────────────────────────

/// A software stand-in for a P-256 secure element, implementing **this namespace's**
/// [`FfiHardwareSigner`] exactly as a Swift `SecureEnclaveSigner` or a Kotlin
/// `StrongBoxSigner` does: `enroll`/`classical_public_key` return an uncompressed SEC1
/// public key (`0x04‖x‖y`), and `sign_classical` returns a DER-encoded ECDSA signature
/// over `SHA-256(msg)`.
struct MockElement {
    sk: p256::ecdsa::SigningKey,
    /// A conforming element refuses to reveal its private bytes; `true` simulates one
    /// that does not, which is a failure by contract.
    exportable: bool,
    /// `Some` makes every call fail, standing in for a locked/absent element.
    fails_with: Option<&'static str>,
}

impl MockElement {
    fn new() -> Self {
        Self {
            sk: p256::ecdsa::SigningKey::from_slice(&[7u8; 32]).expect("valid P-256 scalar"),
            exportable: false,
            fails_with: None,
        }
    }

    fn unavailable() -> Self {
        Self {
            fails_with: Some("no secure element"),
            ..Self::new()
        }
    }

    fn guard(&self) -> Result<(), FfiHardwareSignerError> {
        match self.fails_with {
            Some(_) => Err(FfiHardwareSignerError::Unavailable),
            None => Ok(()),
        }
    }
}

impl FfiHardwareSigner for MockElement {
    fn enroll(&self, key_alias: String) -> Result<Vec<u8>, FfiHardwareSignerError> {
        self.classical_public_key(key_alias)
    }

    fn classical_public_key(&self, _key_alias: String) -> Result<Vec<u8>, FfiHardwareSignerError> {
        self.guard()?;
        Ok(self
            .sk
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec())
    }

    fn sign_classical(
        &self,
        _key_alias: String,
        msg: Vec<u8>,
    ) -> Result<Vec<u8>, FfiHardwareSignerError> {
        self.guard()?;
        use p256::ecdsa::signature::Signer as _;
        let sig: p256::ecdsa::Signature = self.sk.sign(&msg);
        Ok(sig.to_der().as_bytes().to_vec())
    }

    fn assert_non_exportable(&self, _key_alias: String) -> Result<(), FfiHardwareSignerError> {
        self.guard()?;
        if self.exportable {
            Err(FfiHardwareSignerError::Exportable)
        } else {
            Ok(())
        }
    }
}

/// **Constructor parity.** A workspace whose device signing key is hardware-bound over
/// P-256 — reached through *this* namespace's foreign-trait seam rather than
/// `capsule-core`'s — drives the same loop and its assets verify through the chokepoint.
/// That is the whole point of the seam: the classical signature is produced by the
/// element, composed with a software ML-DSA-65 half, and `verify_asset` dispatches on
/// the published key's P-256 tag.
///
/// If the adapter mis-delegated (wrong alias, swapped arguments, dropped bytes) the
/// signature would not verify and this would fail — which is exactly what an
/// FFI-namespace duplicate of the trait needs pinned.
#[tokio::test]
async fn ffi_p256_hardware_signer_constructor_reaches_the_same_flow() {
    let lib = tempfile::TempDir::new().unwrap();
    let workspace = FfiWorkspace::create_with_p256_hardware_signer(
        lib.path().to_string_lossy().into_owned(),
        b"test-passphrase".to_vec(),
        FfiDeviceTier::LowRam,
        Arc::new(MockElement::new()),
        "capsule-device-dsk".into(),
        vec![9u8; 32],
        client_build(),
    )
    .expect("a hardware-bound workspace enrolls");

    let album = workspace.create_album("Trip".into()).unwrap();
    let asset = workspace
        .seal_asset(
            album.clone(),
            "hw.jpg".into(),
            b"\xFF\xD8\xFF hw bytes".to_vec(),
        )
        .unwrap();

    // The manifest was signed by the hardware-composed hybrid key and verifies.
    assert!(matches!(
        workspace.verify_asset(asset.clone()).unwrap(),
        FfiVerifyOutcome::Accept
    ));

    // And so does the same asset as a remote feed entry — the device the directory
    // publishes is the hardware-composed one, so the chokepoint resolves it.
    let blobs = workspace.upload_blobs(asset.clone()).unwrap();
    let outcome = workspace
        .apply_sync_entry(FfiSyncEntry {
            album_id: uuid::Uuid::parse_str(&album).unwrap().as_bytes().to_vec(),
            manifest_cbor: workspace.provenance_head(asset).unwrap(),
            metadata_blob: blobs[1].bytes.clone(),
            original_ciphertext: blobs[2].bytes.clone(),
            local_chain_head: None,
        })
        .unwrap();
    assert!(matches!(outcome, FfiSyncApplyOutcome::Applied { .. }));
}

/// An element that refuses (locked, absent, biometric cancelled) fails enrollment as a
/// typed error rather than panicking across the boundary, and a wrong-length ML seed is
/// refused before the element is touched at all.
#[test]
fn ffi_hardware_enrollment_failures_are_typed() {
    let lib = tempfile::TempDir::new().unwrap();
    let root = lib.path().to_string_lossy().into_owned();

    // `Arc<FfiWorkspace>` (the Ok type) is not Debug, so match rather than `unwrap_err`.
    match FfiWorkspace::create_with_p256_hardware_signer(
        root.clone(),
        b"pw".to_vec(),
        FfiDeviceTier::LowRam,
        Arc::new(MockElement::unavailable()),
        "capsule-device-dsk".into(),
        vec![9u8; 32],
        client_build(),
    ) {
        Err(FfiError::Workspace { .. }) => {}
        Err(other) => panic!("expected a typed workspace error, got {other:?}"),
        Ok(_) => panic!("an unavailable element must not yield a workspace"),
    }

    match FfiWorkspace::create_with_p256_hardware_signer(
        root,
        b"pw".to_vec(),
        FfiDeviceTier::LowRam,
        Arc::new(MockElement::new()),
        "capsule-device-dsk".into(),
        vec![9u8; 16],
        client_build(),
    ) {
        Err(FfiError::InvalidArgument { .. }) => {}
        Err(other) => panic!("expected InvalidArgument for a short ml_seed, got {other:?}"),
        Ok(_) => panic!("a short ml_seed must not yield a workspace"),
    }
}

// ─── Ed25519 hardware signer + hardware DEK (the harness surfaces, #424) ─────

/// A software stand-in for an **Ed25519** element behind [`FfiHardwareSigner`]: a 32-byte
/// public key and a 64-byte signature over `msg` — the shape
/// [`FfiWorkspace::create_with_hardware_signer`] composes, and the one the native
/// `SoftwareSigner` harnesses implement.
struct MockEd25519Element(ed25519_dalek::SigningKey);

impl FfiHardwareSigner for MockEd25519Element {
    fn enroll(&self, key_alias: String) -> Result<Vec<u8>, FfiHardwareSignerError> {
        self.classical_public_key(key_alias)
    }

    fn classical_public_key(&self, _key_alias: String) -> Result<Vec<u8>, FfiHardwareSignerError> {
        Ok(self.0.verifying_key().to_bytes().to_vec())
    }

    fn sign_classical(
        &self,
        _key_alias: String,
        msg: Vec<u8>,
    ) -> Result<Vec<u8>, FfiHardwareSignerError> {
        use ed25519_dalek::Signer as _;
        Ok(self.0.sign(&msg).to_bytes().to_vec())
    }

    fn assert_non_exportable(&self, _key_alias: String) -> Result<(), FfiHardwareSignerError> {
        Ok(())
    }
}

/// The Ed25519 constructor composes a foreign element into a workspace whose assets verify
/// through the chokepoint and read back — so a mis-delegating adapter (wrong alias, dropped
/// bytes, swapped arguments) fails here rather than on a device.
#[test]
fn ffi_ed25519_hardware_signer_constructor_round_trips() {
    let lib = tempfile::TempDir::new().unwrap();
    let workspace = FfiWorkspace::create_with_hardware_signer(
        lib.path().to_string_lossy().into_owned(),
        b"test-passphrase".to_vec(),
        FfiDeviceTier::LowRam,
        Arc::new(MockEd25519Element(ed25519_dalek::SigningKey::from_bytes(
            &[5u8; 32],
        ))),
        "capsule-device-dsk".into(),
        vec![6u8; 32],
        client_build(),
    )
    .expect("an Ed25519 hardware-bound workspace enrolls");

    let album = workspace.create_album("Trip".into()).unwrap();
    let asset = workspace
        .seal_asset(album, "ed.jpg".into(), b"\xFF\xD8\xFF ed bytes".to_vec())
        .unwrap();
    assert!(matches!(
        workspace.verify_asset(asset.clone()).unwrap(),
        FfiVerifyOutcome::Accept
    ));
    assert_eq!(
        workspace.read_plaintext(asset).unwrap(),
        b"\xFF\xD8\xFF ed bytes"
    );
}

/// The Ed25519 constructor refuses a P-256 element (a 65-byte SEC1 key is not a 32-byte
/// Ed25519 key) and a short ML seed as typed errors, never a panic or a half-built workspace.
#[test]
fn ffi_ed25519_hardware_enrollment_failures_are_typed() {
    let lib = tempfile::TempDir::new().unwrap();
    let root = lib.path().to_string_lossy().into_owned();

    match FfiWorkspace::create_with_hardware_signer(
        root.clone(),
        b"pw".to_vec(),
        FfiDeviceTier::LowRam,
        Arc::new(MockElement::new()),
        "capsule-device-dsk".into(),
        vec![9u8; 32],
        client_build(),
    ) {
        Err(FfiError::Workspace { .. }) => {}
        Err(other) => panic!("expected a typed workspace error, got {other:?}"),
        Ok(_) => panic!("a P-256 element must not enroll as Ed25519"),
    }

    match FfiWorkspace::create_with_hardware_signer(
        root,
        b"pw".to_vec(),
        FfiDeviceTier::LowRam,
        Arc::new(MockEd25519Element(ed25519_dalek::SigningKey::from_bytes(
            &[5u8; 32],
        ))),
        "capsule-device-dsk".into(),
        vec![6u8; 16],
        client_build(),
    ) {
        Err(FfiError::InvalidArgument { .. }) => {}
        Err(other) => panic!("expected InvalidArgument for a short ml_seed, got {other:?}"),
        Ok(_) => panic!("a short ml_seed must not yield a workspace"),
    }
}

/// A software stand-in for a P-256 **key-agreement** element behind
/// [`FfiHardwareKeyAgreement`]: uncompressed SEC1 public key, raw 32-byte ECDH x-coordinate —
/// the shapes Secure Enclave `P256.KeyAgreement` and StrongBox ECDH emit. `corrupt` flips the
/// shared secret, standing in for an element whose ECDH disagrees with its published key;
/// `unavailable` makes enrollment fail, standing in for a device with no key-agreement element.
struct MockKeyAgreement {
    sk: p256::SecretKey,
    corrupt: bool,
    unavailable: bool,
}

impl MockKeyAgreement {
    fn new(corrupt: bool) -> Self {
        Self {
            sk: p256::SecretKey::from_slice(&[7u8; 32]).expect("valid P-256 scalar"),
            corrupt,
            unavailable: false,
        }
    }

    fn unavailable() -> Self {
        Self {
            unavailable: true,
            ..Self::new(false)
        }
    }
}

impl FfiHardwareKeyAgreement for MockKeyAgreement {
    fn enroll(&self, key_alias: String) -> Result<Vec<u8>, FfiHardwareSignerError> {
        self.public_key(key_alias)
    }

    fn public_key(&self, _key_alias: String) -> Result<Vec<u8>, FfiHardwareSignerError> {
        use p256::elliptic_curve::sec1::ToEncodedPoint as _;
        if self.unavailable {
            return Err(FfiHardwareSignerError::Unavailable);
        }
        Ok(self
            .sk
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec())
    }

    fn key_agreement(
        &self,
        _key_alias: String,
        peer_public: Vec<u8>,
    ) -> Result<Vec<u8>, FfiHardwareSignerError> {
        let peer = p256::PublicKey::from_sec1_bytes(&peer_public)
            .map_err(|e| FfiHardwareSignerError::Backend(e.to_string()))?;
        let shared = p256::ecdh::diffie_hellman(self.sk.to_nonzero_scalar(), peer.as_affine());
        let mut secret = shared.raw_secret_bytes().to_vec();
        if self.corrupt {
            secret[0] ^= 0xff;
        }
        Ok(secret)
    }

    fn assert_non_exportable(&self, _key_alias: String) -> Result<(), FfiHardwareSignerError> {
        Ok(())
    }
}

/// The hardware DEK round-trip reports `true` only when the element's ECDH composes into the
/// hybrid KEM: an honest element recovers the sender's secret, a disagreeing one does not, and
/// a short ML seed is a typed error rather than a panic, as is an element that fails enrollment.
#[test]
fn ffi_p256_hardware_dek_round_trip_tracks_the_element() {
    assert!(
        p256_hardware_dek_round_trip(
            Arc::new(MockKeyAgreement::new(false)),
            "device-dek".into(),
            vec![9u8; 32],
        )
        .unwrap()
    );
    assert!(
        !p256_hardware_dek_round_trip(
            Arc::new(MockKeyAgreement::new(true)),
            "device-dek".into(),
            vec![9u8; 32],
        )
        .unwrap()
    );
    match p256_hardware_dek_round_trip(
        Arc::new(MockKeyAgreement::new(false)),
        "device-dek".into(),
        vec![9u8; 16],
    ) {
        Err(FfiError::InvalidArgument { .. }) => {}
        other => panic!("expected InvalidArgument for a short ml_seed, got {other:?}"),
    }
    match p256_hardware_dek_round_trip(
        Arc::new(MockKeyAgreement::unavailable()),
        "device-dek".into(),
        vec![9u8; 32],
    ) {
        Err(FfiError::Workspace { message }) => assert!(
            message.contains("hardware P-256 DEK enrollment failed"),
            "unexpected enrollment error message: {message}"
        ),
        other => panic!("expected Workspace for a failing element enrollment, got {other:?}"),
    }
}

/// The workspace surfaces typed errors instead of panicking across the boundary.
#[test]
fn ffi_workspace_surfaces_errors_instead_of_panicking() {
    let lib = tempfile::TempDir::new().unwrap();
    let workspace = enroll(lib.path());

    // A malformed UUID is an InvalidArgument, not a panic.
    match workspace.verify_asset("not-a-uuid".into()) {
        Err(FfiError::InvalidArgument { .. }) => {}
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
    // An unknown asset is a typed workspace error.
    let missing = uuid::Uuid::now_v7().to_string();
    assert!(workspace.read_plaintext(missing.clone()).is_err());
    assert!(workspace.signed_manifest(missing.clone()).is_err());
    assert!(workspace.provenance_head(missing).is_err());
    // A short chain head is refused before any verification runs.
    match workspace.apply_sync_entry(FfiSyncEntry {
        album_id: uuid::Uuid::now_v7().as_bytes().to_vec(),
        manifest_cbor: vec![0xa0],
        metadata_blob: Vec::new(),
        original_ciphertext: Vec::new(),
        local_chain_head: Some(vec![0u8; 4]),
    }) {
        Err(FfiError::InvalidArgument { .. }) => {}
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}
