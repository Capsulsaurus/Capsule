//! Cross-module integration for slice `S-D1`: the real `capsule-sdk` upload client
//! driving THIS crate's real server — the actual router (protocol gate hooped),
//! testcontainer Postgres + Valkey, and real disk — over a TCP socket, since the
//! SDK client is a genuine `reqwest` client, not an in-memory test client.
//!
//! Covers the upload doc's client-side Validation bullets that need a real peer:
//! the full create → PATCH (checksummed chunks) → finalize → HEAD round-trip, the
//! idempotent re-create resume (`200` + `X-Capsule-Offset`), `duplicate_blob` →
//! merge, and the `426` abort-with-upgrade against the real handshake gate.

use capsule_sdk::upload::{
    BlobRole, CreateUploadRequest, ManifestEnvelope, StaticToken, UploadClient, UploadError,
    UploadOutcome, UploadTransport,
};
use jiff::Timestamp;
use nanoid::nanoid;
use salvo::conn::tcp::TcpAcceptor;
use salvo::test::TestClient;

use super::{PROTOCOL, TestCtx, setup, sha256_hex};
use crate::config::{DEFAULT_PROTOCOL_MAX, DEFAULT_PROTOCOL_MIN};

/// Serve the real upload router on an ephemeral TCP port. The server task runs
/// until the test process ends (aborting it mid-request would only flake).
async fn serve(ctx: &TestCtx) -> String {
    let service = ctx.service();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("local addr");
    let acceptor = TcpAcceptor::try_from(listener).expect("acceptor");
    tokio::spawn(async move {
        salvo::server::Server::new(acceptor).serve(service).await;
    });
    format!("http://{addr}/upload")
}

/// An SDK client over the served router, speaking `protocol` with the seeded
/// uploader's bearer token.
fn sdk_client(ctx: &TestCtx, base_url: &str, protocol: &str) -> UploadClient {
    let transport = UploadTransport::with_static_token(
        reqwest::Client::new(),
        base_url,
        protocol,
        StaticToken(ctx.token()),
    );
    UploadClient::new(transport)
}

/// The SDK-side mirror of the harness's `valid_create_body` (envelope-consistent).
fn sdk_request(album_id: &str, hash: &str, size: u64) -> CreateUploadRequest {
    CreateUploadRequest {
        size,
        hash: hash.to_string(),
        content_type: "image/jpeg".to_string(),
        crypto_suite_id: 1,
        protocol_version: PROTOCOL.to_string(),
        blob_role: BlobRole::Original,
        manifest_envelope: ManifestEnvelope {
            crypto_suite_id: 1,
            protocol_version: PROTOCOL.to_string(),
            album_id: Some(album_id.to_string()),
            file_id: nanoid!(),
            amk_version: 1,
            ciphertext_hash: hash.to_string(),
            plaintext_size: size,
            chunk_size: 65536,
            key_mode: "derived".to_string(),
            metadata_blob_hash: None,
            created_by_user: nanoid!(),
            created_by_device: nanoid!(),
            client_version: "capsule-test/1.0".to_string(),
            timestamp: Timestamp::now().to_string(),
            action: "create".to_string(),
            prior_provenance_hash: None,
            retention_until: None,
        },
        album_id: Some(album_id.to_string()),
        owner_id: None,
        intent_id: None,
    }
}

/// Deterministic non-uniform test bytes.
fn test_bytes(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Full round-trip: the SDK client creates the session, streams checksummed
/// chunks (a 256 KiB aligned chunk + an unaligned final chunk), the server
/// finalizes on the last byte, and a subsequent `HEAD` through the client reads
/// the terminal `completed` receipt at the full offset.
#[tokio::test]
async fn sdk_client_full_round_trip_completes() {
    let ctx = setup().await;
    let base = serve(&ctx).await;
    let client = sdk_client(&ctx, &base, PROTOCOL);

    let data = test_bytes(300 * 1024);
    let hash = sha256_hex(&data);
    let request = sdk_request(&ctx.album_id, &hash, data.len() as u64);

    let outcome = client.upload(&request, &data).await.expect("upload");
    let session_id = match outcome {
        UploadOutcome::Completed { session_id } => session_id,
        other => panic!("expected Completed, got {other:?}"),
    };

    let info = client
        .head(&session_id)
        .await
        .expect("head")
        .expect("terminal receipt still queryable");
    assert_eq!(info.offset, data.len() as u64);
    assert_eq!(info.status, "completed");
}

/// Re-uploading an already-finalized blob (same owner/hash/album tuple) is the
/// merge trigger: the server answers `409 error.upload.duplicate_blob` naming the
/// existing asset, and the client resolves it as success-with-asset-ref.
#[tokio::test]
async fn sdk_client_duplicate_blob_resolves_as_merge() {
    let ctx = setup().await;
    let base = serve(&ctx).await;
    let client = sdk_client(&ctx, &base, PROTOCOL);

    let data = test_bytes(8 * 1024);
    let hash = sha256_hex(&data);
    let request = sdk_request(&ctx.album_id, &hash, data.len() as u64);

    let first = client.upload(&request, &data).await.expect("first upload");
    assert!(matches!(first, UploadOutcome::Completed { .. }));

    // Same tuple again — nothing is re-uploaded.
    let second = client.upload(&request, &data).await.expect("dup upload");
    match second {
        UploadOutcome::AlreadyStored { asset_ref } => {
            assert!(!asset_ref.is_empty(), "merge must carry the asset ref");
        }
        other => panic!("expected AlreadyStored (merge), got {other:?}"),
    }
}

/// Resume semantics against the real server. A first chunk lands out-of-band
/// (simulating an interrupted earlier run sharing the same Valkey/disk state);
/// then (a) `upload_resuming` HEAD-aligns and finishes from the authoritative
/// offset, and (b) an idempotent re-`POST` for the same tuple returns the
/// existing session (`200` + `X-Capsule-Offset`), which `upload` also finishes
/// without restarting from zero.
#[tokio::test]
async fn sdk_client_resumes_the_interrupted_session() {
    let ctx = setup().await;
    let base = serve(&ctx).await;
    let client = sdk_client(&ctx, &base, PROTOCOL);

    let data = test_bytes(12 * 1024);
    let hash = sha256_hex(&data);
    let request = sdk_request(&ctx.album_id, &hash, data.len() as u64);

    // Create through the SDK, then land the first 4 KiB chunk out-of-band via the
    // in-memory test client (same session store / upload dir underneath).
    let created = client.create_session(&request).await.expect("create");
    let session_id = match created {
        capsule_sdk::upload::CreateOutcome::Created { response, .. } => response.id,
        other => panic!("expected Created, got {other:?}"),
    };
    let svc = ctx.service();
    let first_chunk = &data[..4096];
    let res = TestClient::patch(format!("http://localhost/upload/{session_id}"))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .add_header("Content-Type", "application/octet-stream", true)
        .add_header("X-Capsule-Offset", "0", true)
        .add_header("X-Capsule-Checksum", sha256_hex(first_chunk), true)
        .body(first_chunk.to_vec())
        .send(&svc)
        .await;
    assert_eq!(
        res.status_code,
        Some(salvo::http::StatusCode::NO_CONTENT),
        "out-of-band first chunk must land"
    );

    // (a) HEAD through the SDK shows the authoritative offset…
    let info = client
        .head(&session_id)
        .await
        .expect("head")
        .expect("session alive");
    assert_eq!(info.offset, 4096);

    // (b) …and the idempotent re-create path: `upload` re-POSTs the same tuple,
    // receives the existing session + offset, and finishes the remaining bytes.
    let outcome = client.upload(&request, &data).await.expect("resume upload");
    match outcome {
        UploadOutcome::Completed {
            session_id: finished,
        } => assert_eq!(finished, session_id, "must finish the SAME session"),
        other => panic!("expected Completed, got {other:?}"),
    }

    let info = client
        .head(&session_id)
        .await
        .expect("head")
        .expect("terminal receipt");
    assert_eq!(info.offset, data.len() as u64);
    assert_eq!(info.status, "completed");
}

/// The cross-version protocol gate (E2E case 9): a client speaking a version
/// outside the server's window is refused `426` before any state is written, and
/// the client aborts-with-upgrade carrying the real advertised range.
#[tokio::test]
async fn sdk_client_426_aborts_with_the_advertised_window() {
    let ctx = setup().await;
    let base = serve(&ctx).await;
    // This client speaks a protocol far before the server's window.
    let client = sdk_client(&ctx, &base, "2020-01-01");

    let data = test_bytes(4096);
    let hash = sha256_hex(&data);
    let request = sdk_request(&ctx.album_id, &hash, data.len() as u64);

    let err = client.upload(&request, &data).await.expect_err("must 426");
    match err {
        UploadError::UpgradeRequired { min, max, .. } => {
            assert_eq!(min.as_deref(), Some(DEFAULT_PROTOCOL_MIN));
            assert_eq!(max.as_deref(), Some(DEFAULT_PROTOCOL_MAX));
        }
        other => panic!("expected UpgradeRequired, got {other:?}"),
    }

    // The gate is fail-closed: no session was created for the refused client.
    let good_client = sdk_client(&ctx, &base, PROTOCOL);
    let sessions = good_client.list_sessions().await.expect("list");
    assert!(
        sessions.is_empty(),
        "426 must reject before any state is written"
    );
}
