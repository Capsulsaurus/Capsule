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
            ("POST", "/api/devices/directory") => {
                MockResponse::new(200, "OK").json_body(r#"{"directory_version":1}"#)
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
        vec!["index", "original"],
        "T0 (metadata) precedes T2 (original); no derivatives without a codec"
    );
    // Keep the wire bytes the feed would carry for this asset before consuming the blobs.
    let metadata_blob = blobs[0].bytes.clone();
    let ciphertext = blobs[1].bytes.clone();
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
    let manifest_cbor = workspace.signed_manifest(asset.clone()).unwrap();
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
            manifest_cbor: workspace.signed_manifest(asset).unwrap(),
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
    session
        .escrow_put(api_base.clone(), blob.clone())
        .await
        .unwrap();
    let fetched = session.escrow_get(api_base.clone()).await.unwrap();
    assert_eq!(fetched, blob, "the escrow blob is stored verbatim");
    assert!(
        workspace
            .verify_escrow_blob(fetched, recovery_secret)
            .unwrap()
    );

    let version = session
        .publish_device_directory(api_base, workspace.signed_device_directory().unwrap())
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
            manifest_cbor: workspace.signed_manifest(asset).unwrap(),
            metadata_blob: blobs[0].bytes.clone(),
            original_ciphertext: blobs[1].bytes.clone(),
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
    assert!(workspace.signed_manifest(missing).is_err());
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
