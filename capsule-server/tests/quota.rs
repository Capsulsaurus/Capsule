//! Storage quota, end to end.
//!
//! The cases that carry the slice are the two refusals and the one that must *never* be a
//! refusal: uploads stop at the hard limit, metadata growth stops when the grace expires, and a
//! delete is admitted whatever the state — because a user who cannot delete cannot get back
//! under the limit, and a quota that could lock someone out of freeing space would be a trap
//! rather than a limit.
//!
//! The suspended state (#705) is held to the same rule at every enforcement point: each growth
//! write is refused while the operator has suspended the account and admitted after unsuspend,
//! and reads, cancellations and deletes are never refused.

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::Hash32;
use capsule_server::blob::BlobStore;
use capsule_server::drop::{DropStore, UploadLinkRecord};
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
    let created_head = publish_asset(&fixture).await;
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
// Suspension (#705)
// ===========================================================================================

/// The seeded account, as the ledger names it.
fn account() -> capsule_server::store::UserId {
    capsule_server::store::UserId::new(user().as_str())
}

/// Suspend the seeded account's quota, as the operator command does.
async fn suspend(fixture: &Fixture) {
    assert!(
        fixture
            .quotas
            .suspend(&account(), fixture.clock.now())
            .await
            .expect("the ledger suspends"),
        "the account was not suspended before"
    );
}

/// Lift the seeded account's suspension, as the operator command does.
async fn unsuspend(fixture: &Fixture) {
    assert!(
        fixture
            .quotas
            .unsuspend(&account())
            .await
            .expect("the ledger unsuspends"),
        "the account was suspended"
    );
}

/// Upload session creation refuses a suspended account, and admits it again after unsuspend.
///
/// On an unlimited deployment, because a suspension is not reached by bytes: the default
/// self-hosted server must enforce it too.
#[tokio::test]
async fn a_suspended_account_cannot_open_an_upload_until_it_is_unsuspended() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    // A session opened before the suspension, to cancel during it.
    let opened = open(&fixture, &bearer, &payload(b's', 512), StatusCode::CREATED).await;

    suspend(&fixture).await;
    let snapshot_while_suspended = snapshot(&fixture, &bearer).await;
    assert_eq!(
        snapshot_while_suspended["state"], "suspended",
        "a read is never refused, and it reports the state"
    );
    let problem = open(
        &fixture,
        &bearer,
        &payload(b't', 512),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(problem["code"], "error.quota.suspended");
    assert_eq!(
        snapshot(&fixture, &bearer).await["used"],
        snapshot_while_suspended["used"],
        "a refused upload is not charged"
    );

    // Cancelling frees space, so it is a lifecycle write and admitted.
    let id = opened["id"].as_str().expect("a session id");
    fixture
        .client
        .delete(&format!("/v1/upload/{id}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    unsuspend(&fixture).await;
    assert_eq!(snapshot(&fixture, &bearer).await["state"], "ok");
    open(&fixture, &bearer, &payload(b't', 512), StatusCode::CREATED).await;
}

/// A lifecycle op that grows metadata is refused while suspended; a delete never is.
#[tokio::test]
async fn a_suspension_stops_metadata_growth_but_never_a_delete() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    let created_head = publish_asset(&fixture).await;

    suspend(&fixture).await;
    let refused = apply(
        &fixture,
        &bearer,
        &bundle(&fixture, "metadata-update", "s1", &created_head, true),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(refused["code"], "error.quota.suspended");

    unsuspend(&fixture).await;
    apply(
        &fixture,
        &bearer,
        &bundle(&fixture, "metadata-update", "s1", &created_head, true),
        StatusCode::OK,
    )
    .await;

    // Suspended again, a delete is still admitted: a user must be able to delete their way back.
    suspend(&fixture).await;
    apply(
        &fixture,
        &bearer,
        &bundle(&fixture, "delete", "s2", &manifest_hash("s1"), false),
        StatusCode::OK,
    )
    .await;
}

/// Issue a share link over the wire.
async fn issue_share(fixture: &Fixture, bearer: &str, tag: u8, expect: StatusCode) -> Value {
    let record = payload(tag, 64);
    let metadata = checksum(&record);
    fixture
        .client
        .post("/v1/shares")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(&json!({
            "opaque_id": format!("{:032x}", u128::from(tag) | 0xabcd_ef01_2345_6789_abcd_ef01_2345_0000),
            "metadata_hash": metadata,
            "serves": [metadata],
            "metadata_blob": BASE64.encode(&record),
        }))
        .send()
        .await
        .assert_status(expect)
        .json()
}

/// A share link is metadata growth: refused while suspended, admitted after.
#[tokio::test]
async fn a_suspended_account_cannot_issue_a_share_link_until_it_is_unsuspended() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;

    suspend(&fixture).await;
    let problem = issue_share(&fixture, &bearer, 1, StatusCode::FORBIDDEN).await;
    assert_eq!(problem["code"], "error.quota.suspended");
    let record = capsule_server::blob::ContentAddress::parse(&checksum(&payload(1, 64)))
        .expect("a content address");
    assert!(
        fixture
            .blobs
            .stat(&record)
            .await
            .expect("the store answers")
            .is_none(),
        "a refused link stores nothing"
    );

    unsuspend(&fixture).await;
    issue_share(&fixture, &bearer, 1, StatusCode::CREATED).await;
}

/// A guest drop is charged to the link owner, so the owner's suspension refuses it.
#[tokio::test]
async fn a_drop_to_a_suspended_owners_link_is_refused_until_the_owner_is_unsuspended() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    let id = format!("{:032x}", 0xabcd_ef01_2345_6789_abcd_ef01_2345_0001_u128);
    fixture
        .client
        .post("/v1/drops/links")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({
            "opaque_id": id,
            "drop_pubkey": BASE64.encode([7_u8; 32]),
            "crypto_suite_id": capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
            // One slot: the refused drop must refund it, or the admitted one below has none.
            "max_file_count": 1,
        }))
        .send()
        .await
        .assert_status(StatusCode::CREATED);
    let drop = |expect| {
        let bytes = payload(b'g', 512);
        let client = &fixture.client;
        let path = format!("/d/{id}");
        async move {
            client
                .post(&path)
                .header("accept", "application/json")
                .json(&json!({
                    "content_type": "image/jpeg",
                    "size": bytes.len(),
                    "ciphertext_hash": checksum(&bytes),
                    "kem_ct": BASE64.encode([9_u8; 64]),
                    "suggested_filename": "from-a-guest.jpg",
                }))
                .send()
                .await
                .assert_status(expect)
                .json::<Value>()
        }
    };

    suspend(&fixture).await;
    let problem = drop(StatusCode::FORBIDDEN).await;
    assert_eq!(problem["code"], "error.quota.suspended");

    unsuspend(&fixture).await;
    drop(StatusCode::CREATED).await;
}

/// An upload link's opaque id, distinct per `tag`.
fn link_id(tag: u8) -> String {
    format!(
        "{:032x}",
        u128::from(tag) | 0x1234_5678_9abc_def0_1234_5678_9abc_0000
    )
}

/// Provision an upload link over the wire, asserting the status, and return the body.
async fn provision_link(fixture: &Fixture, bearer: &str, tag: u8, expect: StatusCode) -> Value {
    fixture
        .client
        .post("/v1/drops/links")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(&json!({
            "opaque_id": link_id(tag),
            "drop_pubkey": BASE64.encode([7_u8; 32]),
            "crypto_suite_id": capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
        }))
        .send()
        .await
        .assert_status(expect)
        .json()
}

/// Revoke an upload link over the wire; never refused, whatever the quota state.
async fn revoke_link(fixture: &Fixture, bearer: &str, tag: u8) {
    fixture
        .client
        .delete(&format!("/v1/drops/links/{}", link_id(tag)))
        .header("authorization", bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
}

/// The link the drop store holds under `tag`'s id, if any.
async fn stored_link(fixture: &Fixture, tag: u8) -> Option<UploadLinkRecord> {
    fixture
        .dropstore
        .resolve(&link_id(tag))
        .await
        .expect("the drop store answers")
}

/// Whether the drop store holds a link under `tag`'s id.
async fn link_exists(fixture: &Fixture, tag: u8) -> bool {
    stored_link(fixture, tag).await.is_some()
}

/// Whether the link under `tag`'s id has been revoked.
async fn link_revoked(fixture: &Fixture, tag: u8) -> bool {
    stored_link(fixture, tag)
        .await
        .is_some_and(|link| link.revoked_at.is_some())
}

/// A new upload link is metadata growth: refused while suspended, admitted after; revoking an
/// existing one is admitted throughout (#725).
#[tokio::test]
async fn a_suspended_account_cannot_provision_an_upload_link_but_can_revoke_one() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    provision_link(&fixture, &bearer, 1, StatusCode::CREATED).await;

    suspend(&fixture).await;
    let problem = provision_link(&fixture, &bearer, 2, StatusCode::FORBIDDEN).await;
    assert_eq!(problem["code"], "error.quota.suspended");
    assert!(
        !link_exists(&fixture, 2).await,
        "a refused link is not recorded"
    );
    revoke_link(&fixture, &bearer, 1).await;
    assert!(
        link_revoked(&fixture, 1).await,
        "the revocation took effect"
    );

    unsuspend(&fixture).await;
    provision_link(&fixture, &bearer, 2, StatusCode::CREATED).await;
    assert!(
        link_exists(&fixture, 2).await,
        "an admitted link is recorded"
    );
}

/// Past the grace window a new upload link is refused `grace_locked`, inside it and after the
/// account is back under its limit it is admitted, and a revocation is never refused (#725).
#[tokio::test]
async fn the_grace_window_stops_upload_link_provisioning_but_never_a_revocation() {
    let fixture = Fixture::with_quota(limits());
    let bearer = token(&fixture).await;
    provision_link(&fixture, &bearer, 1, StatusCode::CREATED).await;

    let storage = capsule_server::blob::ContentAddress::parse(&checksum(b"pre-existing storage"))
        .expect("a content address");
    fixture
        .quotas
        .charge(&account(), &storage, 20_000, fixture.clock.now(), limits())
        .await
        .expect("the ledger accepts");
    assert_eq!(snapshot(&fixture, &bearer).await["state"], "hard_exceeded");
    // Inside the window a link is still metadata growth the account may make.
    provision_link(&fixture, &bearer, 2, StatusCode::CREATED).await;

    fixture
        .clock
        .advance(DEFAULT_GRACE_WINDOW + SignedDuration::from_hours(1));
    let bearer = token(&fixture).await;
    assert_eq!(snapshot(&fixture, &bearer).await["state"], "grace_expired");
    let problem = provision_link(&fixture, &bearer, 3, StatusCode::FORBIDDEN).await;
    assert_eq!(problem["code"], "error.quota.grace_locked");
    assert!(
        !link_exists(&fixture, 3).await,
        "a refused link is not recorded"
    );
    revoke_link(&fixture, &bearer, 1).await;
    assert!(
        link_revoked(&fixture, 1).await,
        "the revocation took effect"
    );

    // Back under the limit, the state clears and provisioning is admitted again.
    assert!(
        fixture
            .quotas
            .release(&account(), &storage)
            .await
            .expect("the ledger releases"),
        "the storage was the account's"
    );
    assert_eq!(snapshot(&fixture, &bearer).await["state"], "ok");
    provision_link(&fixture, &bearer, 3, StatusCode::CREATED).await;
}

// ===========================================================================================
// Lifecycle-bundle helpers
// ===========================================================================================

/// Publish the asset the bundles below operate on, returning its chain head.
async fn publish_asset(fixture: &Fixture) -> String {
    let asset = AssetId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61");
    fixture
        .index
        .reserve(PendingAsset {
            asset_id: asset.clone(),
            owner_id: owner(),
            album_id: album(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            crypto_suite_id: capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
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
                    amk_version: None,
                    finalized_at: Timestamp::UNIX_EPOCH,
                },
            )
            .await
            .expect("the index records");
    }
    created_head
}

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
            "crypto_suite_id": capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
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
