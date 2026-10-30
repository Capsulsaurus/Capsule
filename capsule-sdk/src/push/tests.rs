//! The bundle → wire mapping and the ladder a push drives.
//!
//! The envelope mapping is the highest-risk part of the push client, so it is asserted against a
//! manifest a **real** `capsule_core::Workspace` produced (import a file, read the bundle back)
//! rather than a hand-built fixture that could agree with a wrong mapping. The wire behaviour
//! rides the shared in-process mock HTTP server, following `upload/tests.rs`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::keys::HybridSigningKey;
use capsule_core::crypto::primitives::Argon2Params;
use capsule_i18n::error_codes;
use capsule_library::library::CustodyReceiptCore;
use capsule_library::lifecycle::{UploadBundle, Workspace};

use super::*;
use crate::net::ConnectionClass;
use crate::testmock::{MockRequest, MockResponse, MockServer};
use crate::verify::{StaticToken as VerifyToken, VerifyTransport};

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
        assert_eq!(request.size, blob.bytes.size());
    }
}

/// **The derivative pair.** A derivative's envelope declares its signed `(role, format)` —
/// the pair the server checks against the closed still-derivative format set on receipt — and
/// the format is the same value as the request's `content_type`. No other blob declares it,
/// because the server refuses the pair on anything but a derivative.
#[test]
fn only_a_derivative_declares_its_role_and_format() {
    use capsule_core::crypto::hash::Hash32;
    use capsule_core::crypto::provenance::DerivativeRole;
    use capsule_library::lifecycle::DerivativeBlob;

    let (_dir, mut bundle) = real_bundle();
    bundle.derivatives.push(DerivativeBlob {
        role: DerivativeRole::Preview,
        format: "image/jxl".to_owned(),
        amk_version: Some(bundle.amk_version),
        bytes: b"derivative ciphertext".to_vec(),
        ciphertext_hash: Hash32([7u8; 32]),
    });

    let mut derivatives = 0;
    for (blob, blob_hash) in bundle_blobs(&bundle) {
        let request = create_request(&bundle, &blob, &blob_hash);
        let envelope = &request.manifest_envelope;
        if blob.role == BlobRole::Derivative {
            derivatives += 1;
            assert_eq!(blob.derivative_role, Some(DerivativeRole::Preview));
            assert_eq!(envelope.derivative_role.as_deref(), Some("preview"));
            assert_eq!(envelope.derivative_format.as_deref(), Some("image/jxl"));
            assert_eq!(
                envelope.derivative_format.as_deref(),
                Some(request.content_type.as_str())
            );
        } else {
            assert_eq!(blob.derivative_role, None);
            assert_eq!(envelope.derivative_role, None, "{:?}", blob.role);
            assert_eq!(envelope.derivative_format, None, "{:?}", blob.role);
        }
    }
    assert_eq!(derivatives, 1);

    // Absent on the wire for a non-derivative blob, so an envelope stays the shape it was.
    let original = bundle_blobs(&bundle).pop().expect("the original");
    let json = serde_json::to_value(create_request(&bundle, &original.0, &original.1))
        .expect("the request serializes");
    assert!(json["manifest_envelope"].get("derivative_role").is_none());
    assert!(json["manifest_envelope"].get("derivative_format").is_none());
}

/// **The derivative manifests (#556).** Each signed manifest ships in T1 as an opaque
/// `derivative_manifest` blob addressed by its own bytes, **ahead of** the derivatives it opens,
/// declaring no derivative pair — the server refuses the pair on anything but a derivative.
#[test]
fn derivative_manifests_ship_in_the_preview_tier_ahead_of_their_derivatives() {
    use capsule_core::crypto::hash::Hash32;
    use capsule_core::crypto::provenance::DerivativeRole;
    use capsule_library::lifecycle::DerivativeBlob;

    let (_dir, mut bundle) = real_bundle();
    bundle.derivatives.push(DerivativeBlob {
        role: DerivativeRole::Thumbnail,
        format: "image/jxl".to_owned(),
        amk_version: Some(bundle.amk_version),
        bytes: b"derivative ciphertext".to_vec(),
        ciphertext_hash: Hash32([7u8; 32]),
    });
    bundle.derivative_manifests = vec![
        b"signed thumbnail manifest".to_vec(),
        b"signed sentinel manifest".to_vec(),
    ];

    let blobs = bundle_blobs(&bundle);
    assert_eq!(
        blobs.iter().map(|(blob, _)| blob.role).collect::<Vec<_>>(),
        vec![
            BlobRole::Provenance,
            BlobRole::Metadata,
            BlobRole::DerivativeManifest,
            BlobRole::DerivativeManifest,
            BlobRole::Derivative,
            BlobRole::Original,
        ],
        "a receiver that sees a derivative has already been handed its manifest"
    );
    for (manifest, (blob, hash)) in bundle.derivative_manifests.iter().zip(&blobs[2..4]) {
        assert_eq!(blob.tier, UploadTier::Preview);
        assert_eq!(
            blob.bytes.inline(),
            Some(manifest.as_slice()),
            "the exact bytes ship"
        );
        assert_eq!(*hash, hash_bytes(manifest).to_hex());
        assert_eq!(blob.content_type, "application/octet-stream");

        let request = create_request(&bundle, blob, hash);
        let json = serde_json::to_value(&request).expect("the request serializes");
        assert_eq!(json["blob_role"], "derivative_manifest");
        assert_eq!(request.manifest_envelope.ciphertext_hash, *hash);
        assert!(json["manifest_envelope"].get("derivative_role").is_none());
        assert!(json["manifest_envelope"].get("derivative_format").is_none());
    }

    // The staged view agrees, so resume and scheduling see the same rungs the push drives.
    let staged = staged_asset(&bundle);
    assert_eq!(
        staged
            .ladder_ordered()
            .iter()
            .map(|blob| blob.hash.clone())
            .collect::<Vec<_>>(),
        blobs
            .iter()
            .map(|(_, hash)| hash.clone())
            .collect::<Vec<_>>()
    );
}

/// **The ladder.** A bundle's blobs come out strictly T0 (the index tier: provenance, then the
/// metadata blob) → T1 (derivatives) → T2 (original), and each blob's declared size and role
/// match what it actually carries.
#[test]
fn tier_blobs_are_ladder_ordered() {
    let (_dir, bundle) = real_bundle();
    let asset = staged_asset(&bundle);

    let tiers: Vec<_> = asset.ladder_ordered().iter().map(|b| b.tier).collect();
    assert_eq!(
        tiers,
        vec![UploadTier::Index, UploadTier::Index, UploadTier::Original],
        "a CLI-shaped import has a two-blob index tier and an original, no derivatives"
    );
    assert!(
        tiers.windows(2).all(|w| w[0] <= w[1]),
        "blobs are emitted in ladder order"
    );

    let blobs = bundle_blobs(&bundle);
    assert_eq!(
        blobs.iter().map(|(b, _)| b.role).collect::<Vec<_>>(),
        vec![BlobRole::Provenance, BlobRole::Metadata, BlobRole::Original],
        "provenance leads the index tier: it is the blob the server reads the asset's own \
         claims out of"
    );
    assert_eq!(blobs[0].0.content_type, "application/octet-stream");
    assert_eq!(
        blobs[0].0.bytes.inline(),
        Some(bundle.provenance_blob.as_slice())
    );
    assert_eq!(blobs[1].0.content_type, "application/octet-stream");
    assert_eq!(blobs[1].1, bundle.metadata_blob_hash.unwrap().to_hex());
    assert_eq!(blobs.last().unwrap().1, bundle.ciphertext_hash.to_hex());

    // Server truth prunes the ladder: a held original leaves the index tier outstanding.
    let held: HashSet<String> = [bundle.ciphertext_hash.to_hex()].into_iter().collect();
    let remaining = remaining_tiers(&asset, &held);
    assert_eq!(
        remaining.blobs.iter().map(|b| b.tier).collect::<Vec<_>>(),
        vec![UploadTier::Index, UploadTier::Index]
    );
}

/// **The rung the ladder used to omit** (issue #464). The provenance blob is uploaded under
/// the `provenance` role, its bytes are the canonical CBOR of the chain head, and its content
/// address is `record_hash()` — the value a later lifecycle op's `prior_provenance_hash` must
/// equal.
///
/// Without it the server never holds both index-tier roles, so a pushed asset is invisible on
/// the feed to every device including the pusher's, and its row has no chain head for an op to
/// build on. Both are silent: the upload succeeds.
#[test]
fn the_index_tier_carries_the_provenance_rung() {
    use capsule_core::crypto::hash::hash_bytes;
    use capsule_core::crypto::provenance::ProvenanceRecord;

    let (_dir, bundle) = real_bundle();
    let blobs = bundle_blobs(&bundle);
    let (rung, hash) = &blobs[0];

    assert_eq!(rung.role, BlobRole::Provenance);
    assert_eq!(rung.tier, UploadTier::Index);

    let record: ProvenanceRecord = capsule_core::cbor::from_slice(rung.bytes.inline().unwrap())
        .expect("the rung is a provenance record");
    assert_eq!(record.asset_id, bundle.asset_id);
    assert_eq!(
        *hash,
        record.record_hash().to_hex(),
        "the rung's content address is record_hash(), which is what makes the server's chain \
         head and a client's next prior_provenance_hash the same number"
    );
    assert_eq!(hash_bytes(rung.bytes.inline().unwrap()).to_hex(), *hash);

    // The envelope the server checks against invariant 15 names *this* blob.
    let request = create_request(&bundle, rung, hash);
    assert_eq!(request.hash, request.manifest_envelope.ciphertext_hash);
    assert_eq!(request.blob_role, BlobRole::Provenance);
    assert_eq!(request.size, bundle.provenance_blob.len() as u64);
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
        capsule_library::import::UploadPolicy::Full,
        ConnectionClass::Unmetered,
    );
    let report = push_bundle(&client, &scheduler, &bundle, &HashSet::new(), false)
        .await
        .expect("a duplicate blob must not fail the push");

    assert_eq!(
        report.tier_sequence(),
        vec![UploadTier::Index, UploadTier::Index, UploadTier::Original],
        "the whole ladder still runs"
    );
    assert!(matches!(
        report.pushed[0].outcome,
        TierSessionOutcome::Uploaded { .. }
    ));
    match &report.pushed[2].outcome {
        TierSessionOutcome::AlreadyStored { asset_ref } => {
            assert_eq!(asset_ref, "asset-77", "the merge carries the existing ref");
        }
        other @ TierSessionOutcome::Uploaded { .. } => {
            panic!("expected AlreadyStored (merge), got {other:?}")
        }
    }
    assert_eq!(creates.load(Ordering::SeqCst), 3, "one create per blob");
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
        // A refusal the client never retries: a `5xx` would be waited out.
        MockResponse::new(403, "Forbidden")
    })
    .await;

    let held: HashSet<String> = bundle_blobs(&bundle)
        .into_iter()
        .map(|(_, hash)| hash)
        .collect();
    let client = server.client(&bundle.protocol_version);
    let scheduler = StagedScheduler::new(
        capsule_library::import::UploadPolicy::Full,
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
    assert!(forced.is_err(), "force re-drives and surfaces the 403");
    assert!(requests.load(Ordering::SeqCst) > 0);
}

/// A staged policy on a metered link opens the index tier only; the original waits for a
/// window that permits it, and carries no client-side state in the meantime.
#[test]
fn a_staged_policy_defers_the_original_on_a_metered_link() {
    let (_dir, bundle) = real_bundle();
    let asset = staged_asset(&bundle);
    let metered = StagedScheduler::new(
        capsule_library::import::UploadPolicy::Staged,
        ConnectionClass::Metered,
    );
    assert_eq!(
        metered
            .plan_sessions(&asset)
            .iter()
            .map(|b| b.tier)
            .collect::<Vec<_>>(),
        vec![UploadTier::Index, UploadTier::Index],
        "a metered link escapes the index tier only — both of its blobs, since an asset the \
         server cannot publish is not worth the metered bytes either half costs"
    );

    let unmetered = StagedScheduler::new(
        capsule_library::import::UploadPolicy::Staged,
        ConnectionClass::Unmetered,
    );
    assert_eq!(
        unmetered.plan_sessions(&asset).len(),
        asset.blobs.len(),
        "unmetered Wi-Fi opens the whole ladder"
    );
}

// ─── Custody: a transfer is not custody ──────────────────────────────────────

/// The `received_at` every mock receipt carries, and the client clock the checks run at.
const RECEIVED_AT: &str = "2026-07-10T00:00:00Z";

fn now() -> i64 {
    RECEIVED_AT.parse::<jiff::Timestamp>().unwrap().as_second()
}

/// The server attestation key the client pins.
fn attestation_key() -> HybridSigningKey {
    HybridSigningKey::from_seed64(&[9u8; 64])
}

/// A key the server never published.
fn foreign_key() -> HybridSigningKey {
    HybridSigningKey::from_seed64(&[3u8; 64])
}

/// A receipt for one blob of `bundle`, issued the way finalization issues it: the server's own
/// address and size, the blob's wire role, and the chain position on the provenance blob only.
fn receipt_for(
    key: &HybridSigningKey,
    bundle: &UploadBundle,
    seq: u64,
    blob: &BundleBlob<'_>,
    hash: &str,
) -> CustodyReceipt {
    let core = CustodyReceiptCore {
        version: "custody-receipt/v1".into(),
        crypto_suite_id: bundle.crypto_suite_id,
        protocol_version: bundle.protocol_version.clone(),
        server_id: "capsule.example".into(),
        server_key_id: Hash32::from_bytes([0x11; 32]),
        receipt_seq: seq,
        prior_receipt_hash: None,
        upload_id: format!("sess-{seq}"),
        asset_id: bundle.asset_id.to_string(),
        blob_role: wire_enum(&blob.role),
        ciphertext_hash: Hash32::from_hex(hash).unwrap(),
        size: blob.bytes.size(),
        envelope_hash: (blob.role == BlobRole::Provenance)
            .then(|| hash_bytes(blob.bytes.inline().unwrap())),
        uploaded_by_user: bundle.created_by_user.to_string(),
        uploaded_by_device: None,
        received_at: RECEIVED_AT.into(),
        unknown: Default::default(),
    };
    let server_sig = key.sign(&core.signing_bytes());
    CustodyReceipt { core, server_sig }
}

/// The ways the custody mock departs from an honest server.
#[derive(Clone, Copy, Default)]
struct Faults {
    /// The last chunk is answered `204` but the finalize it raced never lands — the lost-claim
    /// answer the server gives, read naively as success.
    finalize_lost: bool,
    /// Finalization fails terminally after the bytes are committed (the stage and the session's
    /// byte count disagree): the completing chunk is answered `500 storage_inconsistent`, which
    /// the client waits out, and the `HEAD` it resumes from reports the session
    /// `failed_processing` with that code.
    finalize_fails: bool,
    /// The verdict is honest per blob and says nothing is held.
    not_durable: bool,
    /// The verdict's aggregate says `durable: true` whatever the per-blob detail says.
    lying_aggregate: bool,
    /// The receipt log omits this role's receipt.
    drop_receipt: Option<BlobRole>,
    /// Receipts are signed by a key the client never pinned.
    foreign_key: bool,
    /// This role's receipt attests a size other than the one sent (validly signed).
    wrong_size: Option<BlobRole>,
}

/// What a custody run produced: the push report, and the custody check where the push succeeded.
struct CustodyRun {
    pushed: Result<AssetPushReport, PushError>,
    custody: Option<Result<VerifiedCustody, PushError>>,
    /// Every transfer event the push and the custody check published, in order.
    events: Vec<TransferEvent>,
}

/// Every receipt the mock server would issue for `bundle`, keyed by content address.
fn issued_receipts(bundle: &UploadBundle, faults: Faults) -> HashMap<String, CustodyReceipt> {
    let signer = if faults.foreign_key {
        foreign_key()
    } else {
        attestation_key()
    };
    let mut issued = HashMap::new();
    for (seq, (blob, hash)) in bundle_blobs(bundle).iter().enumerate() {
        let mut receipt = receipt_for(&signer, bundle, seq as u64 + 1, blob, hash);
        if faults.wrong_size == Some(blob.role) {
            receipt.core.size += 1;
            receipt.server_sig = signer.sign(&receipt.core.signing_bytes());
        }
        if faults.drop_receipt != Some(blob.role) {
            issued.insert(hash.clone(), receipt);
        }
    }
    issued
}

/// The in-process custody server: it stages, finalizes, verifies and serves receipts, with
/// `faults` applied. Per-session state is `(declared hash, declared size)`; `finalized` holds the
/// addresses whose finalize landed, in order.
struct CustodyServer {
    faults: Faults,
    asset_id: String,
    issued: HashMap<String, CustodyReceipt>,
    sessions: Mutex<HashMap<String, (String, u64)>>,
    finalized: Mutex<Vec<String>>,
}

impl CustodyServer {
    fn answer(&self, req: &MockRequest) -> MockResponse {
        let body: serde_json::Value =
            serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
        match (req.method.as_str(), req.path.as_str()) {
            ("POST", "/storage/verify") => self.verdict(&body),
            ("POST", _) => self.create(&body),
            ("PATCH", path) => self.chunk(req, path),
            ("HEAD", path) if self.faults.finalize_fails => self.failed_head(path),
            ("GET", path) if path.ends_with("/receipts") => self.receipts(),
            other => panic!("unexpected request {other:?}"),
        }
    }

    fn verdict(&self, body: &serde_json::Value) -> MockResponse {
        let finalized = self.finalized.lock().unwrap();
        let declared = body["assets"][0]["blob_hashes"].as_array().unwrap();
        let mut all_held = true;
        let blobs: Vec<String> = declared
            .iter()
            .map(|hash| {
                let hash = hash.as_str().unwrap();
                let held = !self.faults.not_durable && finalized.iter().any(|h| h == hash);
                all_held &= held;
                format!(
                    r#"{{"hash":{hash:?},"role":"original","stored":{held},"indexed":{held},"retrievable":{held}}}"#
                )
            })
            .collect();
        let durable = self.faults.lying_aggregate || all_held;
        MockResponse::new(200, "OK").json_body(format!(
            r#"{{"verdicts":[{{"asset_id":{aid:?},"durable":{durable},"blobs":[{blobs}],"checked_at":{RECEIVED_AT:?}}}]}}"#,
            aid = self.asset_id,
            blobs = blobs.join(","),
        ))
    }

    fn create(&self, body: &serde_json::Value) -> MockResponse {
        let hash = body["hash"].as_str().unwrap().to_owned();
        let size = body["size"].as_u64().unwrap();
        let id = format!("sess-{}", &hash[..16]);
        self.sessions
            .lock()
            .unwrap()
            .insert(id.clone(), (hash, size));
        MockResponse::new(201, "Created")
            .header("X-Capsule-Offset", "0")
            .json_body(format!(
                r#"{{"id":{id:?},"upload_url":"/upload/{id}","suggested_chunk_size":262144}}"#
            ))
    }

    fn chunk(&self, req: &MockRequest, path: &str) -> MockResponse {
        let id = path.rsplit('/').next().unwrap();
        let (hash, size) = self.sessions.lock().unwrap()[id].clone();
        let offset: u64 = req
            .header("X-Capsule-Offset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let next = offset + req.body.len() as u64;
        if next == size {
            if self.faults.finalize_fails {
                return MockResponse::api_error(
                    500,
                    "Internal Server Error",
                    error_codes::UPLOAD_STORAGE_INCONSISTENT,
                    "the staged upload and the session's byte count disagree",
                );
            }
            if !self.faults.finalize_lost {
                self.finalized.lock().unwrap().push(hash);
            }
        }
        MockResponse::new(204, "No Content").header("X-Capsule-Offset", next.to_string())
    }

    /// The `HEAD` a client resumes from after a failed finalize: every byte held, and the
    /// session refused at finalization with the code the completing chunk was answered.
    fn failed_head(&self, path: &str) -> MockResponse {
        let id = path.rsplit('/').next().unwrap();
        let (_, size) = self.sessions.lock().unwrap()[id].clone();
        MockResponse::new(200, "OK")
            .header("X-Capsule-Offset", size.to_string())
            .header("X-Capsule-Content-Length", size.to_string())
            .header("X-Capsule-Upload-Status", "failed_processing")
            .header(
                "X-Capsule-Upload-Error",
                error_codes::UPLOAD_STORAGE_INCONSISTENT,
            )
    }

    fn receipts(&self) -> MockResponse {
        let finalized = self.finalized.lock().unwrap();
        let receipts: Vec<String> = finalized
            .iter()
            .filter_map(|hash| self.issued.get(hash))
            .map(|receipt| {
                let b64 = BASE64.encode(receipt.to_canonical_cbor());
                format!(
                    r#"{{"receipt_seq":{},"receipt_cbor":{b64:?}}}"#,
                    receipt.core.receipt_seq
                )
            })
            .collect();
        MockResponse::new(200, "OK").json_body(format!(
            r#"{{"asset_id":{aid:?},"receipts":[{receipts}]}}"#,
            aid = self.asset_id,
            receipts = receipts.join(","),
        ))
    }
}

/// Push a real bundle through the [`CustodyServer`] with `faults` applied, then run
/// [`verify_custody`] over what the push reported.
async fn custody_run(faults: Faults) -> (tempfile::TempDir, UploadBundle, CustodyRun) {
    let (dir, bundle) = real_bundle();
    let state = Arc::new(CustodyServer {
        faults,
        asset_id: bundle.asset_id.to_string(),
        issued: issued_receipts(&bundle, faults),
        sessions: Mutex::default(),
        finalized: Mutex::default(),
    });
    let server = MockServer::start(move |req: &MockRequest| state.answer(req)).await;

    let events = TransferEvents::channel(4096);
    let mut subscriber = events.subscribe().unwrap();
    let uploads = server
        .client(&bundle.protocol_version)
        .with_events(events.clone());
    let verify = StorageVerifyClient::new(VerifyTransport::with_static_token(
        reqwest::Client::new(),
        server.base_url(),
        VerifyToken("test-token".into()),
    ));
    let scheduler = StagedScheduler::new(
        capsule_library::import::UploadPolicy::Full,
        ConnectionClass::Unmetered,
    );
    let pushed = push_bundle(&uploads, &scheduler, &bundle, &HashSet::new(), false).await;
    let custody = match &pushed {
        Ok(report) => Some(
            verify_custody_with_events(
                &verify,
                &[attestation_key().verifying_key()],
                &bundle,
                report,
                now(),
                &events,
            )
            .await,
        ),
        Err(_) => None,
    };
    let mut published = Vec::new();
    while let Ok(event) = subscriber.try_recv() {
        published.push(event);
    }
    (
        dir,
        bundle,
        CustodyRun {
            pushed,
            custody,
            events: published,
        },
    )
}

/// **The honest path.** A pushed asset is confirmed durable and comes back with one verified
/// receipt per blob, in ladder order — the provenance receipt carrying the chain position.
#[tokio::test]
async fn custody_is_verified_with_one_receipt_per_blob() {
    let (_dir, bundle, run) = custody_run(Faults::default()).await;
    let report = run.pushed.expect("the push transfers");
    let custody = run.custody.unwrap().expect("custody verifies");

    let hashes: Vec<String> = bundle_blobs(&bundle).into_iter().map(|(_, h)| h).collect();
    assert_eq!(report.custody_hashes, hashes, "every blob is relied on");
    assert_eq!(custody.checked_at, RECEIVED_AT);
    assert_eq!(
        custody
            .receipts
            .iter()
            .map(|r| r.core.ciphertext_hash.to_hex())
            .collect::<Vec<_>>(),
        hashes,
        "one receipt per blob, in ladder order"
    );
    assert_eq!(custody.receipts[0].core.blob_role, "provenance");
    assert_eq!(
        custody.receipts[0].core.envelope_hash,
        Some(hash_bytes(&bundle.provenance_blob))
    );
    assert!(
        custody.receipts[1..]
            .iter()
            .all(|r| r.core.envelope_hash.is_none())
    );
}

/// **The event stream of a push.** Each blob, in ladder order, opens a session and ends in a
/// finalize; then the custody check publishes its verdict and one receipt per blob, in the same
/// order. A progress UI can therefore follow a push to custody from the stream alone.
#[tokio::test]
async fn a_push_publishes_each_blob_then_its_custody() {
    let (_dir, bundle, run) = custody_run(Faults::default()).await;
    run.custody.unwrap().expect("custody verifies");
    let hashes: Vec<String> = bundle_blobs(&bundle).into_iter().map(|(_, h)| h).collect();
    let asset_id = bundle.asset_id.to_string();

    let created: Vec<&str> = run
        .events
        .iter()
        .filter_map(|event| match event {
            TransferEvent::SessionCreated { blob, .. } => Some(blob.as_str()),
            _ => None,
        })
        .collect();
    let finalized: Vec<&str> = run
        .events
        .iter()
        .filter_map(|event| match event {
            TransferEvent::Finalize { blob, .. } => Some(blob.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(created, hashes, "one session per blob, in ladder order");
    assert_eq!(finalized, hashes, "every blob finalizes, in ladder order");

    let verify_at = run
        .events
        .iter()
        .position(|event| matches!(event, TransferEvent::Verify { .. }))
        .expect("the verdict is published");
    assert_eq!(
        run.events[verify_at],
        TransferEvent::Verify {
            asset_id: asset_id.clone(),
            durable: true,
            blobs: hashes.len() as u64,
            not_durable: 0,
        }
    );
    let last_finalize = run
        .events
        .iter()
        .rposition(|event| matches!(event, TransferEvent::Finalize { .. }))
        .unwrap();
    assert!(
        last_finalize < verify_at,
        "custody is asked after the transfer"
    );
    let receipts: Vec<(String, String, u64)> = run.events[verify_at + 1..]
        .iter()
        .map(|event| match event {
            TransferEvent::Receipt {
                asset_id,
                blob,
                receipt_seq,
            } => (asset_id.clone(), blob.clone(), *receipt_seq),
            other => panic!("only receipts follow the verdict, got {other:?}"),
        })
        .collect();
    assert_eq!(
        receipts,
        hashes
            .iter()
            .enumerate()
            .map(|(seq, hash)| (asset_id.clone(), hash.clone(), seq as u64 + 1))
            .collect::<Vec<_>>(),
        "one verified receipt per blob, in ladder order"
    );
}

/// **A refused verdict is published, and no receipt follows it.**
#[tokio::test]
async fn a_non_durable_verdict_is_published_without_receipts() {
    let (_dir, bundle, run) = custody_run(Faults {
        not_durable: true,
        ..Faults::default()
    })
    .await;
    let blobs = bundle_blobs(&bundle).len() as u64;
    let tail: Vec<&TransferEvent> = run
        .events
        .iter()
        .skip_while(|event| !matches!(event, TransferEvent::Verify { .. }))
        .collect();
    assert_eq!(
        tail,
        [&TransferEvent::Verify {
            asset_id: bundle.asset_id.to_string(),
            durable: false,
            blobs,
            not_durable: blobs,
        }]
    );
}

/// **The lost finalize race.** Every chunk is answered `204`, so the transfer reads as a
/// success — but the finalize never landed, and the verdict is what says so.
#[tokio::test]
async fn a_lost_finalize_is_not_custody() {
    let (_dir, bundle, run) = custody_run(Faults {
        finalize_lost: true,
        ..Faults::default()
    })
    .await;
    assert!(run.pushed.is_ok(), "the transfer itself reads as a success");
    match run.custody.unwrap() {
        Err(PushError::NotDurable {
            asset_id,
            not_durable,
        }) => {
            assert_eq!(asset_id, bundle.asset_id.to_string());
            assert_eq!(not_durable.len(), bundle_blobs(&bundle).len());
        }
        other => panic!("expected NotDurable, got {other:?}"),
    }
}

/// **The failed finalize.** Finalization fails terminally after the commit; the chunk that
/// triggered it is answered `500 storage_inconsistent`, which the client waits out and resumes
/// from `HEAD`, and the `failed_processing` that `HEAD` reports fails the push with the
/// server's code before custody is ever asked about.
#[tokio::test]
async fn a_failed_finalize_fails_the_push() {
    let (_dir, _bundle, run) = custody_run(Faults {
        finalize_fails: true,
        ..Faults::default()
    })
    .await;
    let error = run.pushed.expect_err("a failed finalize is a failed push");
    assert_eq!(error.kind(), "upload");
    assert_eq!(
        error.error_code(),
        Some(error_codes::UPLOAD_STORAGE_INCONSISTENT)
    );
    assert!(run.custody.is_none());
}

/// **Not durable.** The server says plainly that it does not hold the asset.
#[tokio::test]
async fn a_non_durable_verdict_fails_custody() {
    let (_dir, _bundle, run) = custody_run(Faults {
        not_durable: true,
        ..Faults::default()
    })
    .await;
    let error = run
        .custody
        .unwrap()
        .expect_err("not durable is not custody");
    assert_eq!(error.kind(), "not_durable");
}

/// **The aggregate is not trusted.** `durable: true` over per-blob detail that says otherwise
/// is refused — the same re-check a release makes.
#[tokio::test]
async fn a_durable_aggregate_over_missing_blobs_fails_custody() {
    let (_dir, _bundle, run) = custody_run(Faults {
        lying_aggregate: true,
        not_durable: true,
        ..Faults::default()
    })
    .await;
    assert!(matches!(
        run.custody.unwrap(),
        Err(PushError::NotDurable { ref not_durable, .. }) if !not_durable.is_empty()
    ));
}

/// **A withheld receipt.** Durable, but the log holds no receipt for the metadata blob: a
/// server that withholds receipts never becomes the sole holder of an asset unnoticed.
#[tokio::test]
async fn a_missing_receipt_fails_custody() {
    let (_dir, bundle, run) = custody_run(Faults {
        drop_receipt: Some(BlobRole::Metadata),
        ..Faults::default()
    })
    .await;
    match run.custody.unwrap() {
        Err(PushError::ReceiptMissing { hash, .. }) => {
            assert_eq!(Some(hash), bundle.metadata_blob_hash.map(|h| h.to_hex()));
        }
        other => panic!("expected ReceiptMissing, got {other:?}"),
    }
}

/// **An unpinned signer.** Receipts signed by a key the server never published are refused.
#[tokio::test]
async fn a_receipt_under_an_unpinned_key_fails_custody() {
    let (_dir, _bundle, run) = custody_run(Faults {
        foreign_key: true,
        ..Faults::default()
    })
    .await;
    assert!(matches!(
        run.custody.unwrap(),
        Err(PushError::ReceiptRejected {
            rejection: ReceiptRejection::Signature,
            ..
        })
    ));
}

/// **A receipt for other bytes.** Validly signed, but attesting a size the client never sent.
#[tokio::test]
async fn a_receipt_for_a_different_size_fails_custody() {
    let (_dir, _bundle, run) = custody_run(Faults {
        wrong_size: Some(BlobRole::Original),
        ..Faults::default()
    })
    .await;
    let error = run
        .custody
        .unwrap()
        .expect_err("a mismatched receipt is refused");
    assert_eq!(error.kind(), "receipt_rejected");
    assert!(matches!(
        error,
        PushError::ReceiptRejected {
            rejection: ReceiptRejection::FieldMismatch("size"),
            ..
        }
    ));
}

/// A deferred tier was never sent, so custody is not asked of it: the index tier alone is what
/// a metered staged run relies on.
#[test]
fn custody_excludes_the_tiers_a_staged_policy_deferred() {
    let (_dir, bundle) = real_bundle();
    let metered = StagedScheduler::new(
        capsule_library::import::UploadPolicy::Staged,
        ConnectionClass::Metered,
    );
    let planned = plan(&metered, &bundle, &HashSet::<String>::new(), false);
    assert_eq!(
        planned.deferred_hashes,
        vec![bundle.ciphertext_hash.to_hex()]
    );

    let blobs = bundle_blobs(&bundle);
    assert_eq!(
        custody_hashes(&blobs, &planned.deferred_hashes),
        blobs[..2]
            .iter()
            .map(|(_, h)| h.clone())
            .collect::<Vec<_>>()
    );
}

/// A `derivative_manifest` blob has no arm in the core receipt verifier, so its receipt is held
/// to the same signature and field checks against its wire role.
#[test]
fn a_derivative_manifest_receipt_is_checked_against_its_wire_role() {
    let (_dir, mut bundle) = real_bundle();
    bundle.derivative_manifests = vec![b"signed thumbnail manifest".to_vec()];
    let blobs = bundle_blobs(&bundle);
    let (blob, hash) = blobs
        .iter()
        .find(|(blob, _)| blob.role == BlobRole::DerivativeManifest)
        .unwrap();
    let parsed = Hash32::from_hex(hash).unwrap();
    let key = attestation_key();
    let pinned = [key.verifying_key()];

    let good = receipt_for(&key, &bundle, 1, blob, hash);
    assert_eq!(good.core.blob_role, "derivative_manifest");
    assert_eq!(check_receipt(&good, &pinned, blob, parsed, now()), Ok(()));

    let mut wrong_role = good.clone();
    wrong_role.core.blob_role = "derivative".into();
    wrong_role.server_sig = key.sign(&wrong_role.core.signing_bytes());
    assert_eq!(
        check_receipt(&wrong_role, &pinned, blob, parsed, now()),
        Err(ReceiptRejection::FieldMismatch("blob_role"))
    );

    let unpinned = receipt_for(&foreign_key(), &bundle, 1, blob, hash);
    assert_eq!(
        check_receipt(&unpinned, &pinned, blob, parsed, now()),
        Err(ReceiptRejection::Signature)
    );
}

/// **Upgrade required.** An upload the protocol window refused surfaces the protocol code, so
/// the CLI's `--json` output carries `error.protocol.version_unsupported` rather than nothing.
#[test]
fn an_upgrade_refusal_carries_the_protocol_code() {
    let error = PushError::Upload(UploadError::UpgradeRequired {
        reason: crate::net::negotiation::UpgradeReason::BuildBelowFloor,
        min: None,
        max: None,
        min_client_build: Some("999.0.0".to_owned()),
        message: "update".to_owned(),
    });
    assert_eq!(
        error.error_code(),
        Some(error_codes::PROTOCOL_VERSION_UNSUPPORTED)
    );
}

// ─── A refused create (issue #822) ────────────────────────────────────────────

/// Push `bundle` through a server whose finalization refuses the blob at `refused` with
/// `error.upload.envelope_rejected` — the answer a stale-epoch create meets — and accepts every
/// other blob.
async fn push_refusing(
    bundle: &UploadBundle,
    refused: String,
) -> Result<AssetPushReport, PushError> {
    let sessions: Arc<Mutex<HashMap<String, (String, u64)>>> = Arc::default();
    let server = MockServer::start(move |req: &MockRequest| {
        if req.method == "POST" {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let hash = body["hash"].as_str().unwrap().to_owned();
            let size = body["size"].as_u64().unwrap();
            let id = format!("sess-{}", &hash[..16]);
            sessions.lock().unwrap().insert(id.clone(), (hash, size));
            return MockResponse::new(201, "Created")
                .header("X-Capsule-Offset", "0")
                .json_body(format!(
                    r#"{{"id":{id:?},"upload_url":"/upload/{id}","suggested_chunk_size":262144}}"#
                ));
        }
        let id = req.path.rsplit('/').next().unwrap().to_owned();
        let (hash, size) = sessions.lock().unwrap()[&id].clone();
        let offset: u64 = req
            .header("X-Capsule-Offset")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let next = offset + req.body.len() as u64;
        if next == size && hash == refused {
            return MockResponse::api_error(
                400,
                "Bad Request",
                error_codes::UPLOAD_ENVELOPE_REJECTED,
                "the manifest envelope did not survive re-validation at finalization",
            );
        }
        MockResponse::new(204, "No Content").header("X-Capsule-Offset", next.to_string())
    })
    .await;
    let scheduler = StagedScheduler::new(
        capsule_library::import::UploadPolicy::Full,
        ConnectionClass::Unmetered,
    );
    push_bundle(
        &server.client(&bundle.protocol_version),
        &scheduler,
        bundle,
        &HashSet::new(),
        false,
    )
    .await
}

/// **A refused create is named.** Finalization refusing a create's provenance blob is the
/// stale-epoch signal: it comes back as [`PushError::CreateRefused`] carrying the epoch the
/// manifest named, with the server's code, so the caller can decide whether to re-seal.
#[tokio::test]
async fn a_refused_create_provenance_is_a_create_refusal() {
    let (_dir, bundle) = real_bundle();
    let provenance = hash_bytes(&bundle.provenance_blob).to_hex();

    let error = push_refusing(&bundle, provenance)
        .await
        .expect_err("a refused provenance blob fails the push");
    assert!(
        matches!(
            &error,
            PushError::CreateRefused { asset_id, amk_version, .. }
                if *asset_id == bundle.asset_id.to_string() && *amk_version == bundle.amk_version
        ),
        "{error:?}"
    );
    assert_eq!(error.kind(), "create_refused");
    assert_eq!(
        error.error_code(),
        Some(error_codes::UPLOAD_ENVELOPE_REJECTED)
    );
}

/// **Only the provenance rung is a create refusal.** The same answer on a later rung can follow
/// a provenance blob that landed — the asset may be published — so it stays a plain upload
/// failure and nothing invites a re-seal.
#[tokio::test]
async fn a_refusal_after_the_provenance_rung_is_a_plain_upload_failure() {
    let (_dir, bundle) = real_bundle();
    let metadata = bundle.metadata_blob_hash.unwrap().to_hex();

    let error = push_refusing(&bundle, metadata)
        .await
        .expect_err("a refused metadata blob fails the push");
    assert!(matches!(error, PushError::Upload(_)), "{error:?}");
    assert_eq!(error.kind(), "upload");
}

/// **Only a create is a create refusal.** A head with a predecessor was published before it,
/// so a refusal of its provenance rung is never offered for a re-seal.
#[tokio::test]
async fn a_refused_non_create_is_a_plain_upload_failure() {
    let (_dir, mut bundle) = real_bundle();
    bundle.action = capsule_core::crypto::provenance::Action::Replace;
    bundle.prior_provenance_hash = Some(Hash32([3; 32]));
    let provenance = hash_bytes(&bundle.provenance_blob).to_hex();

    let error = push_refusing(&bundle, provenance)
        .await
        .expect_err("the refusal fails the push");
    assert!(matches!(error, PushError::Upload(_)), "{error:?}");
}

// ─── Redaction ────────────────────────────────────────────────────────────────

/// One listed upload session: `(id, asset_id, status)`.
type ListedSession = (&'static str, &'static str, &'static str);

/// A mock upload server holding `sessions`, answering each `DELETE /{id}` from `cancel`, and
/// recording every request line it served.
async fn session_server(
    sessions: &'static [ListedSession],
    cancel: fn(&str) -> MockResponse,
) -> (MockServer, Arc<Mutex<Vec<String>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let server = MockServer::start(move |req: &MockRequest| {
        log.lock()
            .unwrap()
            .push(format!("{} {}", req.method, req.path));
        match req.method.as_str() {
            "GET" if req.path.ends_with("/sessions") => {
                let rows: Vec<String> = sessions
                    .iter()
                    .map(|(id, asset, status)| {
                        format!(
                            r#"{{"id":"{id}","asset_id":"{asset}","album_id":null,"blob_role":"original","status":"{status}","received_bytes":4096,"total_size":8192,"created_at":"2026-01-01T00:00:00Z","last_progress_at":"2026-01-01T00:00:00Z"}}"#
                        )
                    })
                    .collect();
                MockResponse::new(200, "OK")
                    .json_body(format!(r#"{{"sessions":[{}]}}"#, rows.join(",")))
            }
            "DELETE" => cancel(req.path.rsplit('/').next().unwrap_or_default()),
            _ => MockResponse::new(500, "Internal Server Error"),
        }
    })
    .await;
    (server, seen)
}

/// **A delete aborts only that asset's sessions.** Every listed session of the deleted asset
/// is cancelled; another asset's session is never touched.
#[tokio::test]
async fn redaction_cancels_every_session_of_the_asset_and_no_other() {
    static SESSIONS: &[ListedSession] = &[
        ("s-meta", "asset-a", "uploading"),
        ("s-other", "asset-b", "uploading"),
        ("s-orig", "asset-a", "pending"),
    ];
    let (server, seen) = session_server(SESSIONS, |_| MockResponse::new(204, "No Content")).await;
    let client = server.client("2026-01-01");

    let redaction = redact_asset(&client, "asset-a").await.expect("redacts");

    assert_eq!(redaction.cancelled, vec!["s-meta", "s-orig"]);
    assert!(redaction.finalizing.is_empty());
    let deletes: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.starts_with("DELETE"))
        .cloned()
        .collect();
    assert_eq!(deletes.len(), 2, "{deletes:?}");
    assert!(
        deletes.iter().all(|line| !line.ends_with("/s-other")),
        "another asset's session was cancelled: {deletes:?}"
    );
}

/// **The finalize race.** A session whose finalization began before the abort reached it
/// answers `409 session_not_active`; that is not a failure, it is reported as finalizing so the
/// delete that follows supersedes the create it completes. A session already gone (`404`) is a
/// cancellation.
#[tokio::test]
async fn a_session_finalizing_during_the_abort_is_reported_not_failed() {
    static SESSIONS: &[ListedSession] = &[
        ("s-done", "asset-a", "waiting_for_processing"),
        ("s-gone", "asset-a", "uploading"),
    ];
    let (server, _) = session_server(SESSIONS, |id| {
        if id == "s-done" {
            MockResponse::api_error(
                409,
                "Conflict",
                error_codes::UPLOAD_SESSION_NOT_ACTIVE,
                "Session not active",
            )
        } else {
            MockResponse::new(404, "Not Found")
        }
    })
    .await;
    let client = server.client("2026-01-01");

    let redaction = redact_asset(&client, "asset-a").await.expect("redacts");

    assert_eq!(redaction.finalizing, vec!["s-done"]);
    assert_eq!(redaction.cancelled, vec!["s-gone"]);
}

/// Any other refusal fails the redaction, so the caller knows a session may still be open.
#[tokio::test]
async fn an_unexpected_cancel_refusal_fails_the_redaction() {
    static SESSIONS: &[ListedSession] = &[("s-1", "asset-a", "uploading")];
    let (server, _) = session_server(SESSIONS, |_| {
        MockResponse::api_error(403, "Forbidden", "error.auth.forbidden", "no")
    })
    .await;
    let client = server.client("2026-01-01");

    let error = redact_asset(&client, "asset-a")
        .await
        .expect_err("a 403 is not a cancellation");
    assert!(
        matches!(error, UploadError::Rejected { status: 403, .. }),
        "{error:?}"
    );
}

/// **A settled session is not cancelled.** The listing keeps `completed` and
/// `failed_processing` records until their lifetime cap; deleting an asset whose upload already
/// settled sends no `DELETE` for them and reports none of them as finalizing.
#[tokio::test]
async fn a_terminal_session_is_skipped_not_cancelled_or_finalizing() {
    static SESSIONS: &[ListedSession] = &[
        ("s-done", "asset-a", "completed"),
        ("s-failed", "asset-a", "failed_processing"),
        ("s-open", "asset-a", "waiting_for_processing"),
    ];
    let (server, seen) = session_server(SESSIONS, |id| {
        if id == "s-open" {
            MockResponse::new(204, "No Content")
        } else {
            MockResponse::new(500, "a settled session was cancelled")
        }
    })
    .await;
    let client = server.client("2026-01-01");

    let redaction = redact_asset(&client, "asset-a").await.expect("redacts");

    assert_eq!(redaction.cancelled, vec!["s-open"]);
    assert!(redaction.finalizing.is_empty(), "{redaction:?}");
    let deletes: Vec<String> = seen
        .lock()
        .unwrap()
        .iter()
        .filter(|line| line.starts_with("DELETE"))
        .cloned()
        .collect();
    assert_eq!(deletes.len(), 1, "{deletes:?}");
}

/// An asset with no open session costs one listing and no cancel.
#[tokio::test]
async fn an_asset_with_no_open_session_redacts_to_nothing() {
    static SESSIONS: &[ListedSession] = &[("s-1", "asset-b", "uploading")];
    let (server, seen) = session_server(SESSIONS, |_| MockResponse::new(500, "boom")).await;
    let client = server.client("2026-01-01");

    let redaction = redact_asset(&client, "asset-a").await.expect("redacts");

    assert_eq!(redaction, Redaction::default());
    assert_eq!(seen.lock().unwrap().len(), 1, "only the listing was asked");
}

// ─── Pushing many bundles across lanes ───────────────────────────────────────

/// A server that opens a fresh session per create, acknowledges every chunk whole, and records
/// which asset each create was for, in arrival order.
async fn lane_server() -> (MockServer, Arc<Mutex<Vec<String>>>) {
    let creates = Arc::new(Mutex::new(Vec::new()));
    let seen = creates.clone();
    let sessions = AtomicUsize::new(0);
    let server = MockServer::start(move |req: &MockRequest| {
        if req.method == "POST" {
            let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
            let asset = body["manifest_envelope"]["file_id"].as_str().unwrap();
            seen.lock().unwrap().push(asset.to_owned());
            let id = format!("s-{}", sessions.fetch_add(1, Ordering::SeqCst));
            return MockResponse::new(201, "Created")
                .header("X-Capsule-Offset", "0")
                .json_body(format!(
                    r#"{{"id":{id:?},"upload_url":"/upload/{id}","suggested_chunk_size":262144}}"#
                ));
        }
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
    (server, creates)
}

/// Push two real bundles on one lane under `sessions`, with a one-byte budget, and return each
/// bundle's asset id and the asset each create was for.
async fn push_two_on_one_lane(sessions: u32) -> (String, String, Vec<String>) {
    let (_dir_a, a) = real_bundle();
    let (_dir_b, b) = real_bundle();
    let (server, creates) = lane_server().await;
    let client = server.client(&a.protocol_version);
    let scheduler = StagedScheduler::new(
        capsule_library::import::UploadPolicy::Full,
        ConnectionClass::Unmetered,
    );
    let held = HashSet::new();
    let lanes = [LanePush {
        server: ServerKey::new(server.base_url()),
        client: &client,
        bundles: vec![
            LaneBundle {
                bundle: &a,
                held: &held,
            },
            LaneBundle {
                bundle: &b,
                held: &held,
            },
        ],
    }];
    // One byte of budget: every range reserves all of it, so the two bundles' ranges take
    // turns, and the run still finishes.
    let results = push_lanes(&scheduler, &lanes, false, 1, LaneWindows::new(sessions, 1)).await;
    assert_eq!(results.len(), 1);
    for result in &results[0] {
        let report = result.as_ref().expect("both bundles push");
        assert_eq!(
            report.tier_sequence(),
            vec![UploadTier::Index, UploadTier::Index, UploadTier::Original],
            "each bundle still drives its ladder in order"
        );
    }
    assert_eq!(
        results[0][0].as_ref().unwrap().asset_id,
        a.asset_id.to_string()
    );
    assert_eq!(
        results[0][1].as_ref().unwrap().asset_id,
        b.asset_id.to_string()
    );
    let order = creates.lock().unwrap().clone();
    (a.asset_id.to_string(), b.asset_id.to_string(), order)
}

/// **W bundles of a lane run at once.** With a sessions window of two, the second asset's
/// first session opens before the first asset's ladder is done; with a window of one, the
/// lane pushes one asset after the other.
#[tokio::test]
async fn a_lane_runs_up_to_w_bundles_at_once() {
    let (a, b, order) = push_two_on_one_lane(2).await;
    assert_eq!(order.len(), 6, "three sessions per asset: {order:?}");
    let first_b = order.iter().position(|asset| *asset == b).unwrap();
    let last_a = order.iter().rposition(|asset| *asset == a).unwrap();
    assert!(first_b < last_a, "the two ladders overlapped: {order:?}");

    let (a, b, order) = push_two_on_one_lane(1).await;
    assert_eq!(
        order,
        [&a, &a, &a, &b, &b, &b].map(String::clone),
        "one asset at a time"
    );
}

/// An empty run returns at once, and the run's future can move across threads.
#[tokio::test]
async fn an_empty_push_run_returns_nothing() {
    fn assert_send<T: Send>(value: T) -> T {
        value
    }
    let scheduler = StagedScheduler::new(
        capsule_library::import::UploadPolicy::Full,
        ConnectionClass::Unmetered,
    );
    let lanes: [LanePush<'_, std::hash::RandomState>; 0] = [];
    let run = assert_send(push_lanes(
        &scheduler,
        &lanes,
        false,
        1,
        LaneWindows::default(),
    ));
    assert!(run.await.is_empty());
}

/// **The gate a bundle runs under** waits for the shared budget and wakes when a reservation is
/// released, and applies the server's advice to its lane and its session.
#[tokio::test]
async fn the_queue_gate_waits_for_the_budget_and_applies_advice() {
    use std::time::{Duration, Instant};

    let server = ServerKey::new("https://lane.test");
    let mut queue =
        UploadQueue::new(100, Duration::from_secs(3600)).with_windows(LaneWindows::new(4, 4));
    queue.enqueue(&server, QueuedJob::new(UploadTier::Original, (0, 0)));
    queue.enqueue(&server, QueuedJob::new(UploadTier::Original, (0, 1)));
    let now = Instant::now();
    let a = queue.next(now).unwrap();
    let b = queue.next(now).unwrap();
    let shared = LaneShared {
        queue: Mutex::new(queue),
        freed: tokio::sync::Notify::new(),
    };
    let gate_a = QueueGate {
        shared: &shared,
        server: &server,
        grant: a.id,
    };
    let gate_b = QueueGate {
        shared: &shared,
        server: &server,
        grant: b.id,
    };
    let pending = Duration::from_millis(50);

    let whole = gate_a.reserve(100).await.expect("the whole budget");
    let mut waiting = gate_b.reserve(10);
    assert!(
        tokio::time::timeout(pending, &mut waiting).await.is_err(),
        "b waits while the budget is full"
    );
    gate_a.release(whole);
    let range = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the release wakes the waiting range")
        .expect("reserved");
    assert_eq!(shared.queue().in_use(), 10);

    gate_b.advised(&ServerAdvice::parse(Some("sessions=1, ranges=1, ttl=60")));
    assert_eq!(shared.queue().sessions_window(&server, Instant::now()), 1);
    let mut second = gate_b.reserve(10);
    assert!(
        tokio::time::timeout(pending, &mut second).await.is_err(),
        "b's session now holds one range at a time"
    );
    gate_b.release(range);
    tokio::time::timeout(Duration::from_secs(5), second)
        .await
        .expect("woken once b's range returned")
        .expect("reserved");

    shared.queue().complete(b.id, Instant::now());
    assert_eq!(
        gate_b.reserve(10).await,
        None,
        "a finished grant reserves nothing"
    );
}

/// **The gates serve waiting ranges first-come.** A holder of the whole budget that releases it
/// and asks again at once goes behind the ranges already waiting; and a waiter woken by that
/// release but refused for being behind another is woken again once the one ahead is served.
#[tokio::test]
async fn the_queue_gates_serve_waiting_ranges_first_come() {
    use std::time::{Duration, Instant};

    let server = ServerKey::new("https://lane.test");
    let mut queue =
        UploadQueue::new(100, Duration::from_secs(3600)).with_windows(LaneWindows::new(4, 1));
    for bundle in 0..3 {
        queue.enqueue(&server, QueuedJob::new(UploadTier::Original, (0, bundle)));
    }
    let now = Instant::now();
    let grants: Vec<_> = std::iter::from_fn(|| queue.next(now)).collect();
    let shared = LaneShared {
        queue: Mutex::new(queue),
        freed: tokio::sync::Notify::new(),
    };
    let gate = |at: usize| QueueGate {
        shared: &shared,
        server: &server,
        grant: grants[at].id,
    };
    let (holder, first, second) = (gate(0), gate(1), gate(2));
    let pending = Duration::from_millis(50);

    let whole = holder.reserve(100).await.expect("the whole budget");
    let mut first_waits = first.reserve(10);
    assert!(
        tokio::time::timeout(pending, &mut first_waits)
            .await
            .is_err()
    );
    let mut second_waits = second.reserve(10);
    assert!(
        tokio::time::timeout(pending, &mut second_waits)
            .await
            .is_err()
    );

    holder.release(whole);
    let mut again = holder.reserve(100);
    assert!(
        tokio::time::timeout(pending, &mut again).await.is_err(),
        "the holder asks again behind the ranges already waiting"
    );
    // The second waiter is polled first: refused behind the first, it is woken again once the
    // first is served.
    let (second_range, first_range) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(second_waits, first_waits)
    })
    .await
    .expect("both waiting ranges are served");
    assert_eq!(shared.queue().in_use(), 20);
    assert!(
        tokio::time::timeout(pending, &mut again).await.is_err(),
        "the whole-budget range waits for the budget to drain"
    );

    first.release(first_range.expect("reserved"));
    second.release(second_range.expect("reserved"));
    tokio::time::timeout(Duration::from_secs(5), again)
        .await
        .expect("the holder is served once the line ahead of it drains")
        .expect("reserved");
    assert_eq!(shared.queue().in_use(), 100);
}
