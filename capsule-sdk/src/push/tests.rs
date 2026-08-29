//! `S-D18` — the bundle → wire mapping and the ladder a push drives.
//!
//! The envelope mapping is the highest-risk part of the slice, so it is asserted against a
//! manifest a **real** `capsule_core::Workspace` produced (import a file, read the bundle back)
//! rather than a hand-built fixture that could agree with a wrong mapping. The wire behaviour
//! rides the shared in-process mock HTTP server, following `upload/tests.rs`.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::lifecycle::{UploadBundle, Workspace};
use capsule_i18n::error_codes;

use super::*;
use crate::net::ConnectionClass;
use crate::testmock::{MockRequest, MockResponse, MockServer};

/// The fast Argon2 cost; the production tier would dominate the suite's runtime.
const FAST: Argon2Params = Argon2Params {
    mem_kib: 64,
    t_cost: 1,
    p_cost: 1,
};

/// A real workspace with one imported asset, and its bundle. The temp dir is returned so the
/// library outlives the bundle's borrow-free contents.
fn real_bundle() -> (tempfile::TempDir, UploadBundle) {
    let dir = tempfile::TempDir::new().unwrap();
    let lib = dir.path().join("library");
    std::fs::create_dir_all(&lib).unwrap();
    let src = dir.path().join("photo.jpg");
    std::fs::write(
        &src,
        b"\xFF\xD8\xFF real bundle bytes for the envelope mapping test",
    )
    .unwrap();

    let mut ws = Workspace::create_with_params(&lib, b"passphrase", FAST).unwrap();
    let album = ws.default_album_id();
    ws.ensure_album(album, "Imports").unwrap();
    let asset_id = ws.import_asset(album, &src).unwrap();
    let bundle = ws.upload_bundle(&asset_id).unwrap();
    (dir, bundle)
}

/// **The mapping.** Every `ManifestEnvelope` field is the head manifest's, verbatim — except
/// `ciphertext_hash`, which names the blob actually being transferred (the server's
/// invariant-15 rule: `manifest_envelope.ciphertext_hash == hash`). For the original blob the
/// two coincide, so the envelope is a byte-for-byte mirror of the signed manifest.
#[test]
fn manifest_envelope_mirrors_the_signed_manifest() {
    let (_dir, bundle) = real_bundle();
    let hash = bundle.ciphertext_hash.to_hex();
    let envelope = envelope_for(&bundle, &hash);

    assert_eq!(envelope.crypto_suite_id, bundle.crypto_suite_id);
    assert_eq!(envelope.protocol_version, bundle.protocol_version);
    assert_eq!(envelope.album_id, Some(bundle.album_id.to_string()));
    assert_eq!(envelope.file_id, bundle.asset_id.to_string());
    assert_eq!(envelope.amk_version, bundle.amk_version);
    assert_eq!(envelope.ciphertext_hash, hash);
    assert_eq!(envelope.plaintext_size, bundle.plaintext_size);
    assert_eq!(envelope.chunk_size, bundle.chunk_size);
    assert_eq!(
        envelope.key_mode, "derived",
        "a plain import is key-derived"
    );
    assert_eq!(
        envelope.metadata_blob_hash,
        bundle.metadata_blob_hash.map(|h| h.to_hex())
    );
    assert_eq!(envelope.created_by_user, bundle.created_by_user.to_string());
    assert_eq!(
        envelope.created_by_device,
        bundle.created_by_device.to_string()
    );
    assert_eq!(envelope.client_version, bundle.client_version);
    assert_eq!(envelope.timestamp, bundle.timestamp);
    assert_eq!(envelope.action, "create", "a first import is a create");
    assert_eq!(envelope.prior_provenance_hash, None, "create has no prior");
    assert_eq!(envelope.retention_until, None);

    // The consistency the server enforces at invariant 15, for every blob of the bundle.
    for (blob, blob_hash) in bundle_blobs(&bundle) {
        let request = create_request(&bundle, &blob, &blob_hash);
        assert_eq!(request.hash, request.manifest_envelope.ciphertext_hash);
        assert_eq!(request.album_id, request.manifest_envelope.album_id);
        assert_eq!(
            request.crypto_suite_id,
            request.manifest_envelope.crypto_suite_id
        );
        assert_eq!(
            request.protocol_version,
            request.manifest_envelope.protocol_version
        );
        assert_eq!(request.size, blob.bytes.len() as u64);
    }
}

/// **The ladder.** A bundle's blobs come out strictly T0 (metadata index) → T1 (derivatives) →
/// T2 (original), and each blob's declared size and role match what it actually carries.
#[test]
fn tier_blobs_are_ladder_ordered() {
    let (_dir, bundle) = real_bundle();
    let asset = staged_asset(&bundle);

    let tiers: Vec<_> = asset.ladder_ordered().iter().map(|b| b.tier).collect();
    assert_eq!(
        tiers,
        vec![UploadTier::Index, UploadTier::Original],
        "a CLI-shaped import has a metadata index and an original, no derivatives"
    );
    assert!(
        tiers.windows(2).all(|w| w[0] <= w[1]),
        "blobs are emitted in ladder order"
    );

    let blobs = bundle_blobs(&bundle);
    assert_eq!(blobs[0].0.role, BlobRole::Metadata);
    assert_eq!(blobs[0].0.content_type, "application/octet-stream");
    assert_eq!(blobs[0].1, bundle.metadata_blob_hash.unwrap().to_hex());
    assert_eq!(blobs.last().unwrap().0.role, BlobRole::Original);
    assert_eq!(blobs.last().unwrap().1, bundle.ciphertext_hash.to_hex());

    // Server truth prunes the ladder: a held original leaves only the index outstanding.
    let held: HashSet<String> = [bundle.ciphertext_hash.to_hex()].into_iter().collect();
    let remaining = remaining_tiers(&asset, &held);
    assert_eq!(
        remaining.blobs.iter().map(|b| b.tier).collect::<Vec<_>>(),
        vec![UploadTier::Index]
    );
}

/// **`duplicate_blob` is a merge, not a failure.** The server answers `409` +
/// `error.upload.duplicate_blob` for a blob it already holds; the push resolves it as an
/// `AlreadyStored` outcome and carries on with the rest of the ladder.
#[tokio::test]
async fn duplicate_blob_resolves_as_merge_not_error() {
    let (_dir, bundle) = real_bundle();
    let original_hash = bundle.ciphertext_hash.to_hex();
    let dup = original_hash.clone();

    let creates = Arc::new(AtomicUsize::new(0));
    let seen = creates.clone();
    let server = MockServer::start(move |req: &MockRequest| {
        if req.method == "POST" {
            seen.fetch_add(1, Ordering::SeqCst);
            let body = String::from_utf8_lossy(&req.body);
            if body.contains(&dup) {
                return MockResponse::api_error(
                    409,
                    "Conflict",
                    error_codes::UPLOAD_DUPLICATE_BLOB,
                    "This content is already stored as asset asset-77",
                );
            }
            return MockResponse::new(201, "Created")
                .header("X-Capsule-Offset", "0")
                .json_body(
                    r#"{"id":"sess-1","upload_url":"/upload/sess-1","suggested_chunk_size":262144}"#,
                );
        }
        // Every PATCH acknowledges the whole chunk.
        let offset: u64 = req
            .header("X-Capsule-Offset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        MockResponse::new(204, "No Content").header(
            "X-Capsule-Offset",
            (offset + req.body.len() as u64).to_string(),
        )
    })
    .await;

    let client = server.client(&bundle.protocol_version);
    let scheduler = StagedScheduler::new(
        capsule_core::import::upload::UploadPolicy::Full,
        ConnectionClass::Unmetered,
    );
    let report = push_bundle(&client, &scheduler, &bundle, &HashSet::new(), false)
        .await
        .expect("a duplicate blob must not fail the push");

    assert_eq!(
        report.tier_sequence(),
        vec![UploadTier::Index, UploadTier::Original],
        "the whole ladder still runs"
    );
    assert!(matches!(
        report.pushed[0].outcome,
        TierSessionOutcome::Uploaded { .. }
    ));
    match &report.pushed[1].outcome {
        TierSessionOutcome::AlreadyStored { asset_ref } => {
            assert_eq!(asset_ref, "asset-77", "the merge carries the existing ref");
        }
        other @ TierSessionOutcome::Uploaded { .. } => panic!("expected AlreadyStored (merge), got {other:?}"),
    }
    assert_eq!(creates.load(Ordering::SeqCst), 2, "one create per blob");
}

/// **Re-running a push is a no-op.** With every blob in the server-truth `held` set, nothing is
/// planned and no request is made — which is what makes `capsule push` re-runnable.
#[tokio::test]
async fn a_fully_held_bundle_pushes_nothing() {
    let (_dir, bundle) = real_bundle();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen = requests.clone();
    let server = MockServer::start(move |_req: &MockRequest| {
        seen.fetch_add(1, Ordering::SeqCst);
        MockResponse::new(500, "Internal Server Error")
    })
    .await;

    let held: HashSet<String> = bundle_blobs(&bundle)
        .into_iter()
        .map(|(_, hash)| hash)
        .collect();
    let client = server.client(&bundle.protocol_version);
    let scheduler = StagedScheduler::new(
        capsule_core::import::upload::UploadPolicy::Full,
        ConnectionClass::Unmetered,
    );

    let report = push_bundle(&client, &scheduler, &bundle, &held, false)
        .await
        .expect("a fully-held bundle is a no-op");
    assert!(report.is_no_op());
    assert_eq!(report.already_held, held.len());
    assert_eq!(requests.load(Ordering::SeqCst), 0, "no request was made");

    // `--force` ignores server truth and re-drives the ladder anyway.
    let forced = push_bundle(&client, &scheduler, &bundle, &held, true).await;
    assert!(forced.is_err(), "force re-drives and surfaces the 500");
    assert!(requests.load(Ordering::SeqCst) > 0);
}

/// A staged policy on a metered link opens the index tier only; the original waits for a
/// window that permits it, and carries no client-side state in the meantime.
#[test]
fn a_staged_policy_defers_the_original_on_a_metered_link() {
    let (_dir, bundle) = real_bundle();
    let asset = staged_asset(&bundle);
    let metered = StagedScheduler::new(
        capsule_core::import::upload::UploadPolicy::Staged,
        ConnectionClass::Metered,
    );
    assert_eq!(
        metered
            .plan_sessions(&asset)
            .iter()
            .map(|b| b.tier)
            .collect::<Vec<_>>(),
        vec![UploadTier::Index],
        "a metered link escapes the index only"
    );

    let unmetered = StagedScheduler::new(
        capsule_core::import::upload::UploadPolicy::Staged,
        ConnectionClass::Unmetered,
    );
    assert_eq!(
        unmetered.plan_sessions(&asset).len(),
        asset.blobs.len(),
        "unmetered Wi-Fi opens the whole ladder"
    );
}
