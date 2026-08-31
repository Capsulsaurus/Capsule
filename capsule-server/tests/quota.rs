//! Storage quota, end to end (slice `S-C6`).
//!
//! The cases that carry the slice are the two refusals and the one that must *never* be a
//! refusal: uploads stop at the hard limit, metadata growth stops when the grace expires, and a
//! delete is admitted whatever the state — because a user who cannot delete cannot get back
//! under the limit, and a quota that could lock someone out of freeing space would be a trap
//! rather than a limit.

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::Hash32;
use capsule_server::blob::BlobStore;
use capsule_server::index::{AssetIndex, BlobRecord, PendingAsset};
use capsule_server::quota::{DEFAULT_GRACE_WINDOW, QuotaLimits, QuotaStore};
use capsule_server::store::{AssetId, BlobRole, Clock};
use jiff::{SignedDuration, Timestamp};
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{
    Fixture, PROTOCOL_VERSION, album, checksum, create_request, device, owner, payload, user,
};

/// A deployment with real thresholds, sized so one 8 KiB blob fits and two do not.
fn limits() -> QuotaLimits {
    QuotaLimits::new(4096, 12_288, DEFAULT_GRACE_WINDOW)
}

/// The bearer for the seeded account.
async fn token(fixture: &Fixture) -> String {
    format!("Bearer {}", fixture.login().await.access_token)
}

/// Ask for the quota snapshot.
async fn snapshot(fixture: &Fixture, bearer: &str) -> Value {
    fixture
        .client
        .get("/v1/quota")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json()
}

/// Open a session for `bytes`, asserting the status, and return the body.
async fn open(fixture: &Fixture, bearer: &str, bytes: &[u8], expect: StatusCode) -> Value {
    fixture
        .client
        .post("/v1/upload")
        .header("authorization", bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("accept", "application/json")
        .json(&create_request(&fixture.clock, bytes, "original"))
        .send()
        .await
        .assert_status(expect)
        .json()
}

// ===========================================================================================

/// An unlimited deployment reports no limits and refuses nothing.
#[tokio::test]
async fn an_unlimited_deployment_reports_no_limits() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;

    let body = snapshot(&fixture, &bearer).await;
    assert_eq!(body["used"], 0);
    assert_eq!(body["state"], "ok");
    assert_eq!(
        body["soft_limit"],
        Value::Null,
        "a number that is not a limit is a number some client will put in a progress bar"
    );
    assert_eq!(body["hard_limit"], Value::Null);
}

/// An upload is charged at session creation, and the snapshot says so.
#[tokio::test]
async fn an_upload_is_charged_at_session_creation() {
    let fixture = Fixture::with_quota(limits());
    let bearer = token(&fixture).await;
    let bytes = payload(b'q', 8192);

    open(&fixture, &bearer, &bytes, StatusCode::CREATED).await;

    let body = snapshot(&fixture, &bearer).await;
    assert_eq!(body["used"], 8192);
    assert_eq!(body["soft_limit"], 4096);
    assert_eq!(body["hard_limit"], 12_288);
    assert_eq!(
        body["state"], "soft_warning",
        "over the soft limit, under the hard one: uploads still succeed and the client warns"
    );
}

/// The hard limit is the one enforcement point, and it refuses at creation.
#[tokio::test]
async fn an_upload_past_the_hard_limit_is_refused_at_creation() {
    let fixture = Fixture::with_quota(limits());
    let bearer = token(&fixture).await;

    open(&fixture, &bearer, &payload(b'a', 8192), StatusCode::CREATED).await;
    let problem = open(
        &fixture,
        &bearer,
        &payload(b'b', 8192),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(problem["code"], "error.quota.exceeded");

    assert_eq!(
        snapshot(&fixture, &bearer).await["used"],
        8192,
        "a refused upload must not have been charged: the debit is released, not left standing"
    );
}

/// Cancelling a session gives the reservation back.
#[tokio::test]
async fn cancelling_a_session_releases_its_reservation() {
    let fixture = Fixture::with_quota(limits());
    let bearer = token(&fixture).await;
    let bytes = payload(b'c', 8192);
    let opened = open(&fixture, &bearer, &bytes, StatusCode::CREATED).await;
    let id = opened["id"].as_str().expect("a session id");

    fixture
        .client
        .delete(&format!("/v1/upload/{id}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    assert_eq!(snapshot(&fixture, &bearer).await["used"], 0);

    // And the space is usable again, which is the point of releasing it.
    open(&fixture, &bearer, &payload(b'd', 8192), StatusCode::CREATED).await;
}

/// Deduplicated bytes are charged to the first uploader only.
#[tokio::test]
async fn a_blob_another_account_already_holds_costs_nothing() {
    let fixture = Fixture::with_quota(limits());
    let bearer = token(&fixture).await;
    let bytes = payload(b'e', 8192);
    let address =
        capsule_server::blob::ContentAddress::parse(&checksum(&bytes)).expect("a content address");

    // Somebody else got there first.
    fixture
        .quotas
        .charge(
            &capsule_server::store::UserId::new("somebody-else"),
            &address,
            8192,
            Timestamp::UNIX_EPOCH,
            limits(),
        )
        .await
        .expect("the ledger accepts");

    open(&fixture, &bearer, &bytes, StatusCode::CREATED).await;
    assert_eq!(
        snapshot(&fixture, &bearer).await["used"],
        0,
        "a merge is not storage, and charging for it would let one account exhaust another's \
         quota by re-uploading blobs whose addresses it already knows"
    );
}

/// Past the grace window, metadata growth stops — and deletes do not.
#[tokio::test]
async fn the_grace_window_stops_metadata_growth_but_never_a_delete() {
    let fixture = Fixture::with_quota(limits());
    let bearer = token(&fixture).await;

    // Publish an asset to operate on, and put the account over the hard limit.
    let asset = AssetId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61");
    fixture
        .index
        .reserve(PendingAsset {
            asset_id: asset.clone(),
            owner_id: owner(),
            album_id: album(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            crypto_suite_id: capsule_core::crypto::CRYPTO_SUITE_ID,
            created_at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the index reserves");
    let provenance = payload(b'p', 64);
    let created_head = checksum(&provenance);
    for (role, bytes) in [
        (BlobRole::Provenance, provenance.clone()),
        (BlobRole::Metadata, payload(b'm', 64)),
    ] {
        let address = capsule_server::blob::ContentAddress::parse(&checksum(&bytes))
            .expect("a content address");
        fixture
            .blobs
            .put(&address, &bytes)
            .await
            .expect("the store accepts");
        fixture
            .index
            .record_blob(
                &asset,
                BlobRecord {
                    manifest_sha256: (role == BlobRole::Provenance)
                        .then(|| Hash32::from_hex(address.as_str()).expect("a digest")),
                    role,
                    address,
                    size: bytes.len() as u64,
                    finalized_at: Timestamp::UNIX_EPOCH,
                },
            )
            .await
            .expect("the index records");
    }
    // Put the account over the limit through the ledger rather than through an upload, because
    // an upload cannot get there: the enforcement point refuses on the *projected* total, so
    // `hard_exceeded` is reached by a lowered limit or by growth the session check did not
    // project — never by a session it admitted. See `capsule_server::quota`.
    fixture
        .quotas
        .charge(
            &capsule_server::store::UserId::new(user().as_str()),
            &capsule_server::blob::ContentAddress::parse(&checksum(b"pre-existing storage"))
                .expect("a content address"),
            20_000,
            fixture.clock.now(),
            limits(),
        )
        .await
        .expect("the ledger accepts");
    assert_eq!(snapshot(&fixture, &bearer).await["state"], "hard_exceeded");

    // Inside the window, a metadata update still works.
    let update = bundle(&fixture, "metadata-update", "m1", &created_head, true);
    apply(&fixture, &bearer, &update, StatusCode::OK).await;

    // Past it, it does not.
    fixture
        .clock
        .advance(DEFAULT_GRACE_WINDOW + SignedDuration::from_hours(1));
    // Fourteen days is well past an access token's life, so the client signs in again — which
    // is what a real one would have done long before reaching this state.
    let bearer = token(&fixture).await;
    assert_eq!(snapshot(&fixture, &bearer).await["state"], "grace_expired");
    let refused = apply(
        &fixture,
        &bearer,
        &bundle(
            &fixture,
            "metadata-update",
            "m2",
            &manifest_hash("m1"),
            true,
        ),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(refused["code"], "error.quota.grace_locked");

    // But a delete is admitted, because a user must be able to delete their way back under.
    apply(
        &fixture,
        &bearer,
        &bundle(&fixture, "delete", "d1", &manifest_hash("m1"), false),
        StatusCode::OK,
    )
    .await;
}

/// A quota snapshot needs a credential.
#[tokio::test]
async fn the_snapshot_requires_a_credential() {
    let fixture = Fixture::working();
    fixture
        .client
        .get("/v1/quota")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

// ===========================================================================================
// Lifecycle-bundle helpers
// ===========================================================================================

/// The manifest bytes an op seeded with `seed` carries.
fn manifest(seed: &str) -> Vec<u8> {
    format!("signed-lifecycle-manifest-{seed}").into_bytes()
}

/// Its content hash — the chain head it becomes.
fn manifest_hash(seed: &str) -> String {
    capsule_core::crypto::hash::hash_bytes(&manifest(seed)).to_hex()
}

/// A lifecycle bundle chaining onto `prior`, with or without a metadata blob.
fn bundle(fixture: &Fixture, action: &str, seed: &str, prior: &str, metadata: bool) -> Value {
    let blob = metadata.then(|| payload(b'n', 32));
    json!({
        "manifest_envelope": {
            "crypto_suite_id": capsule_core::crypto::CRYPTO_SUITE_ID,
            "protocol_version": PROTOCOL_VERSION,
            "album_id": album().as_str(),
            "file_id": "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61",
            "amk_version": 1,
            "ciphertext_hash": checksum(b"the asset's original ciphertext"),
            "plaintext_size": 4096,
            "chunk_size": 65_536,
            "key_mode": "derived",
            "metadata_blob_hash": blob.as_ref().map(|b| checksum(b)),
            "created_by_user": user().as_str(),
            "created_by_device": device().to_string(),
            "client_version": "capsule-cli/0.1.0",
            "timestamp": fixture.clock.now().to_string(),
            "action": action,
            "prior_provenance_hash": prior,
            "retention_until": Value::Null,
        },
        "manifest_cbor": BASE64.encode(manifest(seed)),
        "metadata_blob": blob.map(|b| BASE64.encode(b)),
    })
}

/// Apply a lifecycle bundle, asserting the status.
async fn apply(fixture: &Fixture, bearer: &str, body: &Value, expect: StatusCode) -> Value {
    fixture
        .client
        .post(&format!("/v1/albums/{}/ops", album()))
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(body)
        .send()
        .await
        .assert_status(expect)
        .json()
}
