//! `POST /v1/albums/{album_id}/ops` — the lifecycle-write surface (slice `S-C16`), end to end.
//!
//! This is the only producer of a tombstone the sync feed has, so half of what these cases
//! assert is not about the response at all: it is about what a *second device* sees on the feed
//! afterwards. A delete that returns `200` and never reaches the feed has deleted nothing as far
//! as every other device is concerned.

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::hash_bytes;
use capsule_server::blob::{BlobStore, ContentAddress};
use capsule_server::index::{AssetIndex, BlobRecord, PendingAsset};
use capsule_server::store::{AssetId, BlobRole, Clock, OwnerId};
use jiff::Timestamp;
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{Fixture, PROTOCOL_VERSION, album, device, owner, second_album, user};

/// The asset every case operates on.
const ASSET: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61";

/// The bearer for the seeded account.
async fn token(fixture: &Fixture) -> String {
    format!("Bearer {}", fixture.login().await.access_token)
}

/// Put `bytes` in the store at their own address.
async fn store(fixture: &Fixture, bytes: &[u8]) -> ContentAddress {
    let address = ContentAddress::parse(&support::checksum(bytes)).expect("a content address");
    fixture.blobs.put(&address, bytes).await.expect("stored");
    address
}

/// The chain head a published asset carries: its create manifest's provenance blob.
///
/// A lifecycle op must name it, and a real client knows it because it wrote the manifest. The
/// tests spell it out so that "what does the first op chain onto" is visible rather than
/// implied.
fn created_head() -> String {
    support::checksum(b"published-provenance")
}

/// Publish [`ASSET`] into the index so there is something to operate on.
async fn publish(fixture: &Fixture) -> AssetId {
    let id = AssetId::new(ASSET);
    fixture
        .index
        .reserve(PendingAsset {
            asset_id: id.clone(),
            owner_id: owner(),
            album_id: album(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            crypto_suite_id: capsule_core::crypto::CRYPTO_SUITE_ID,
            created_at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the index reserves");
    for (role, seed) in [
        (BlobRole::Provenance, "published-provenance"),
        (BlobRole::Metadata, "published-metadata"),
    ] {
        let address = store(fixture, seed.as_bytes()).await;
        fixture
            .index
            .record_blob(
                &id,
                BlobRecord {
                    role,
                    address,
                    size: seed.len() as u64,
                    finalized_at: Timestamp::UNIX_EPOCH,
                },
            )
            .await
            .expect("the index records");
    }
    id
}

/// A signed manifest's bytes, distinct per `seed`.
///
/// Not valid CBOR on purpose: this surface stores the bytes verbatim and never parses them, so
/// feeding it something parseable would be testing less than the contract allows.
fn manifest(seed: &str) -> Vec<u8> {
    format!("signed-lifecycle-manifest-{seed}").into_bytes()
}

/// A well-formed op bundle for `action`, chaining onto `prior`.
fn bundle(
    fixture: &Fixture,
    action: &str,
    seed: &str,
    prior: Option<&str>,
    metadata: Option<&[u8]>,
) -> Value {
    let manifest = manifest(seed);
    json!({
        "manifest_envelope": {
            "crypto_suite_id": capsule_core::crypto::CRYPTO_SUITE_ID,
            "protocol_version": PROTOCOL_VERSION,
            "album_id": album().as_str(),
            "file_id": ASSET,
            "amk_version": 1,
            "ciphertext_hash": support::checksum(b"the asset's original ciphertext"),
            "plaintext_size": 4096,
            "chunk_size": 65_536,
            "key_mode": "derived",
            "metadata_blob_hash": metadata.map(|b| support::checksum(b)),
            "created_by_user": user().as_str(),
            "created_by_device": device().to_string(),
            "client_version": "capsule-cli/0.1.0",
            "timestamp": fixture.clock.now().to_string(),
            "action": action,
            "prior_provenance_hash": prior,
            "retention_until": Value::Null,
        },
        "manifest_cbor": BASE64.encode(&manifest),
        "metadata_blob": metadata.map(|b| BASE64.encode(b)),
    })
}

/// The hex hash of the manifest `bundle` would send for `seed` — the chain head it becomes.
fn manifest_hash(seed: &str) -> String {
    hash_bytes(&manifest(seed)).to_hex()
}

/// Apply `body` to the seeded album, asserting the status.
async fn apply(fixture: &Fixture, bearer: &str, body: &Value, expect: StatusCode) -> Value {
    apply_to(fixture, bearer, album().as_str(), body, expect).await
}

/// The same, against an arbitrary album path.
async fn apply_to(
    fixture: &Fixture,
    bearer: &str,
    album_path: &str,
    body: &Value,
    expect: StatusCode,
) -> Value {
    fixture
        .client
        .post(&format!("/v1/albums/{album_path}/ops"))
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(body)
        .send()
        .await
        .assert_status(expect)
        .json()
}

/// The caller's feed, as a second device would read it.
async fn feed(fixture: &Fixture, bearer: &str) -> Value {
    fixture
        .client
        .get("/v1/sync")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json()
}

// ===========================================================================================

/// A delete tombstones the asset, and the tombstone reaches the feed carrying no byte
/// references.
#[tokio::test]
async fn a_delete_tombstones_the_asset_and_reaches_the_feed() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    publish(&fixture).await;

    let applied = apply(
        &fixture,
        &bearer,
        &bundle(&fixture, "delete", "d1", Some(&created_head()), None),
        StatusCode::OK,
    )
    .await;
    assert_eq!(applied["asset_id"], ASSET);
    assert_eq!(applied["action"], "delete");
    assert_eq!(applied["replayed"], false);

    let page = feed(&fixture, &bearer).await;
    let entry = &page["entries"][0];
    assert_eq!(entry["change"], "deleted");
    assert_eq!(entry["sync_seq"], applied["sync_seq"]);
    assert_eq!(
        entry["manifest_cbor"],
        Value::Null,
        "a tombstone points at nothing: a client that fetched its blobs would be fetching \
         bytes the delete exists to stop it wanting"
    );
    assert!(
        entry["blobs"].as_array().expect("blob refs").is_empty(),
        "a tombstone carries no byte references"
    );
}

/// A restore returns the asset, and both changes are on the feed in order.
#[tokio::test]
async fn a_delete_then_restore_round_trips_through_the_feed() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    publish(&fixture).await;

    let deleted = apply(
        &fixture,
        &bearer,
        &bundle(&fixture, "delete", "d1", Some(&created_head()), None),
        StatusCode::OK,
    )
    .await;
    let restored = apply(
        &fixture,
        &bearer,
        &bundle(
            &fixture,
            "trash-restore",
            "r1",
            Some(&manifest_hash("d1")),
            None,
        ),
        StatusCode::OK,
    )
    .await;
    assert!(
        restored["sync_seq"].as_u64() > deleted["sync_seq"].as_u64(),
        "a restore must sit above the delete it undoes, or a caught-up reader never sees it"
    );

    let page = feed(&fixture, &bearer).await;
    let entry = &page["entries"][0];
    assert_eq!(
        entry["change"], "created",
        "to a reader at zero it is a new asset"
    );
    assert_eq!(entry["sync_seq"], restored["sync_seq"]);
    assert_ne!(
        entry["manifest_cbor"],
        Value::Null,
        "a restored asset points at its manifest again"
    );
}

/// The feed serves the exact manifest bytes the op carried.
#[tokio::test]
async fn the_feed_serves_the_lifecycle_manifest_byte_for_byte() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    publish(&fixture).await;

    apply(
        &fixture,
        &bearer,
        &bundle(
            &fixture,
            "metadata-update",
            "m1",
            Some(&created_head()),
            Some(b"new metadata"),
        ),
        StatusCode::OK,
    )
    .await;

    let page = feed(&fixture, &bearer).await;
    let served = page["entries"][0]["manifest_cbor"]
        .as_str()
        .expect("the entry carries its manifest");
    assert_eq!(
        BASE64.decode(served).expect("base64"),
        manifest("m1"),
        "the feed must serve the bytes the client signed, not a re-serialization of them"
    );
    assert_eq!(
        page["entries"][0]["metadata_blob"],
        support::checksum(b"new metadata"),
        "the metadata update did not re-point the metadata blob"
    );
}

/// The same manifest twice is one application, one sequence number, one identical body.
#[tokio::test]
async fn a_replayed_manifest_returns_the_same_response_and_writes_nothing() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    publish(&fixture).await;
    let body = bundle(&fixture, "delete", "d1", Some(&created_head()), None);

    let first = apply(&fixture, &bearer, &body, StatusCode::OK).await;
    let second = apply(&fixture, &bearer, &body, StatusCode::OK).await;

    assert_eq!(first["sync_seq"], second["sync_seq"]);
    assert_eq!(first["asset_id"], second["asset_id"]);
    assert_eq!(first["action"], second["action"]);
    assert_eq!(
        second["replayed"], true,
        "the advisory flag is the only field that may differ, and a correct client ignores it"
    );

    let page = feed(&fixture, &bearer).await;
    assert_eq!(
        page["entries"].as_array().expect("entries").len(),
        1,
        "a replay minted a second feed position"
    );
}

/// Invariant 17: a manifest that does not chain onto the head is refused, and told the head.
#[tokio::test]
async fn a_manifest_that_does_not_chain_is_a_stale_revival() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    publish(&fixture).await;
    apply(
        &fixture,
        &bearer,
        &bundle(&fixture, "delete", "d1", Some(&created_head()), None),
        StatusCode::OK,
    )
    .await;

    // A second op still naming the *create* as its predecessor — the shape a replayed old
    // manifest takes when it is resubmitted to resurrect a deleted asset. The delete has moved
    // the head, so this no longer chains.
    let problem = apply(
        &fixture,
        &bearer,
        &bundle(&fixture, "trash-restore", "r1", Some(&created_head()), None),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(problem["code"], "error.upload.stale_revival");
    assert_eq!(
        problem["chain_head"],
        manifest_hash("d1"),
        "the owner is told what to rebase onto, which is the difference between recovering \
         and retrying a losing manifest forever"
    );

    let page = feed(&fixture, &bearer).await;
    assert_eq!(
        page["entries"][0]["change"], "deleted",
        "a refused op must have written nothing"
    );
}

/// A lifecycle manifest with no predecessor is malformed, not stale.
///
/// The distinction matters to a client: `409` says "re-read and rebase", and this cannot be
/// rebased because the manifest never claimed to follow anything. Every non-`create` action
/// chains by definition — the asset it acts on was created by something.
#[tokio::test]
async fn a_lifecycle_manifest_with_no_predecessor_is_malformed_not_stale() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    publish(&fixture).await;

    let problem = apply(
        &fixture,
        &bearer,
        &bundle(&fixture, "delete", "d1", None, None),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(problem["code"], "error.upload.envelope_mismatch");
}

/// Invariant 16: an action that moves bytes is an upload, not a lifecycle op.
#[tokio::test]
async fn a_byte_moving_action_is_refused_as_an_upload() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    publish(&fixture).await;

    for action in ["create", "replace"] {
        let problem = apply(
            &fixture,
            &bearer,
            &bundle(&fixture, action, "x", Some(&created_head()), None),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(
            problem["code"], "error.upload.invalid_action",
            "{action} moves blob bytes and belongs to the upload protocol"
        );
    }
}

/// Invariant 16: an action outside the closed enum is refused before anything else.
#[tokio::test]
async fn an_unknown_action_is_refused() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    publish(&fixture).await;

    let problem = apply(
        &fixture,
        &bearer,
        &bundle(
            &fixture,
            "future-action-not-yet-defined",
            "x",
            Some(&created_head()),
            None,
        ),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(problem["code"], "error.upload.envelope_mismatch");
}

/// Invariant 25: a metadata blob must be the one its manifest committed to.
#[tokio::test]
async fn a_metadata_blob_must_be_the_one_the_manifest_committed_to() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    publish(&fixture).await;

    // The manifest commits to one blob and the bundle carries another.
    let mut body = bundle(
        &fixture,
        "metadata-update",
        "m1",
        Some(&created_head()),
        Some(b"the committed metadata"),
    );
    body["metadata_blob"] = json!(BASE64.encode(b"different metadata entirely"));

    let problem = apply(&fixture, &bearer, &body, StatusCode::BAD_REQUEST).await;
    assert_eq!(problem["code"], "error.upload.envelope_mismatch");

    // And a blob with nothing committing to it, or a commitment with no blob.
    let mut orphan = bundle(
        &fixture,
        "metadata-update",
        "m2",
        Some(&created_head()),
        None,
    );
    orphan["metadata_blob"] = json!(BASE64.encode(b"unclaimed"));
    apply(&fixture, &bearer, &orphan, StatusCode::BAD_REQUEST).await;

    let mut promised = bundle(
        &fixture,
        "metadata-update",
        "m3",
        Some(&created_head()),
        Some(b"promised metadata"),
    );
    promised["metadata_blob"] = Value::Null;
    apply(&fixture, &bearer, &promised, StatusCode::BAD_REQUEST).await;
}

/// The envelope must agree with the path it arrived on.
#[tokio::test]
async fn an_envelope_that_names_another_album_is_refused() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    publish(&fixture).await;
    fixture
        .authority
        .allow_album(&owner(), &second_album(), PROTOCOL_VERSION);

    let problem = apply_to(
        &fixture,
        &bearer,
        second_album().as_str(),
        &bundle(&fixture, "delete", "d1", Some(&created_head()), None),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(problem["code"], "error.upload.envelope_mismatch");
}

/// An album the caller cannot write is refused before the manifest is even parsed.
#[tokio::test]
async fn an_album_without_write_capability_is_refused() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    publish(&fixture).await;
    fixture.authority.close_album(&owner(), &album());

    let problem = apply(
        &fixture,
        &bearer,
        &bundle(&fixture, "delete", "d1", Some(&created_head()), None),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(problem["code"], "error.upload.album_access_denied");
}

/// An asset that is not this caller's is indistinguishable from one that does not exist.
#[tokio::test]
async fn an_asset_that_is_not_the_callers_is_refused_without_disclosure() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    // Somebody else's asset, under the album this caller *can* write.
    fixture
        .index
        .reserve(PendingAsset {
            asset_id: AssetId::new(ASSET),
            owner_id: OwnerId::new("somebody-else"),
            album_id: album(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            crypto_suite_id: capsule_core::crypto::CRYPTO_SUITE_ID,
            created_at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the index reserves");

    let theirs = apply(
        &fixture,
        &bearer,
        &bundle(&fixture, "delete", "d1", Some(&created_head()), None),
        StatusCode::FORBIDDEN,
    )
    .await;

    // And an asset that was never reserved at all.
    let empty = Fixture::working();
    let empty_bearer = token(&empty).await;
    let absent = apply(
        &empty,
        &empty_bearer,
        &bundle(&empty, "delete", "d1", Some(&created_head()), None),
        StatusCode::FORBIDDEN,
    )
    .await;

    assert_eq!(
        theirs, absent,
        "the asset id is the manifest's own field and therefore client-chosen, so a guess must \
         buy nothing — including the knowledge that it guessed right"
    );
}

/// A store that cannot answer is a coded failure, and nothing is applied.
#[tokio::test]
async fn an_unreachable_index_is_a_coded_failure() {
    let fixture = Fixture::working();
    let bearer = token(&fixture).await;
    publish(&fixture).await;
    fixture.index.set_unavailable(true);

    let problem = apply(
        &fixture,
        &bearer,
        &bundle(&fixture, "delete", "d1", Some(&created_head()), None),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    assert_eq!(problem["code"], "error.upload.unavailable");
}

/// The surface needs a credential.
#[tokio::test]
async fn a_lifecycle_write_requires_a_credential() {
    let fixture = Fixture::working();
    let body = bundle(&fixture, "delete", "d1", Some(&created_head()), None);
    fixture
        .client
        .post(&format!("/v1/albums/{}/ops", album()))
        .json(&body)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
