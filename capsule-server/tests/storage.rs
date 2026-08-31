//! `POST /v1/storage/verify` — the key-free durability verdict (slice `S-C3`), end to end.
//!
//! Every case here is really the same question asked from a different angle: *would a client
//! that believed this answer and deleted its local copy have lost the photo?* That is why the
//! failure directions are not symmetric. A wrong `durable = false` costs a client some disk;
//! a wrong `durable = true` costs a user their photograph. The suite is weighted accordingly —
//! most of it is about the ways this endpoint must refuse to say yes.

mod support;

use capsule_server::blob::{BlobStore, ContentAddress};
use capsule_server::gc::CollectionStore;
use capsule_server::index::{AssetIndex, BlobRecord, PendingAsset};
use capsule_server::store::{AssetId, BlobRole, OwnerId};
use jiff::Timestamp;
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{Fixture, PROTOCOL_VERSION, album, owner};

/// Put `bytes` in the store at their own address.
async fn store(fixture: &Fixture, bytes: &[u8]) -> ContentAddress {
    let address = ContentAddress::parse(&support::checksum(bytes)).expect("a content address");
    fixture.blobs.put(&address, bytes).await.expect("stored");
    address
}

/// Reserve `asset` under `owner` and land its index tier from real stored bytes.
///
/// Returns the asset id and the two addresses, so a case can declare exactly what it relies on.
async fn publish_for(
    fixture: &Fixture,
    who: &OwnerId,
    asset: &str,
) -> (AssetId, ContentAddress, ContentAddress) {
    let id = AssetId::new(asset);
    fixture
        .index
        .reserve(PendingAsset {
            asset_id: id.clone(),
            owner_id: who.clone(),
            album_id: album(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            crypto_suite_id: 1,
            created_at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the index reserves");

    let provenance = store(fixture, format!("{asset}-provenance").as_bytes()).await;
    record(fixture, &id, BlobRole::Provenance, &provenance).await;
    let metadata = store(fixture, format!("{asset}-metadata").as_bytes()).await;
    record(fixture, &id, BlobRole::Metadata, &metadata).await;
    (id, provenance, metadata)
}

/// The seeded account's own asset.
async fn publish(fixture: &Fixture, asset: &str) -> (AssetId, ContentAddress, ContentAddress) {
    publish_for(fixture, &owner(), asset).await
}

/// Record `address` against `asset`.
async fn record(fixture: &Fixture, asset: &AssetId, role: BlobRole, address: &ContentAddress) {
    fixture
        .index
        .record_blob(
            asset,
            BlobRecord {
                role,
                address: address.clone(),
                size: 32,
                finalized_at: Timestamp::UNIX_EPOCH,
            },
        )
        .await
        .expect("the index records");
}

/// The bearer for the fixture's seeded account.
async fn bearer(fixture: &Fixture) -> String {
    format!("Bearer {}", fixture.login().await.access_token)
}

/// Ask for a verdict, asserting the status.
async fn verify(fixture: &Fixture, bearer: &str, body: &Value, expect: StatusCode) -> Value {
    fixture
        .client
        .post("/v1/storage/verify")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(body)
        .send()
        .await
        .assert_status(expect)
        .json()
}

/// One asset's declaration.
fn declare(asset: &AssetId, hashes: &[&ContentAddress]) -> Value {
    json!({
        "asset_id": asset.as_str(),
        "blob_hashes": hashes.iter().map(|h| h.as_str()).collect::<Vec<_>>(),
    })
}

// ===========================================================================================

/// The whole point: an asset whose every declared blob is really there.
#[tokio::test]
async fn a_complete_asset_is_durable() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, metadata) = publish(&fixture, "durable").await;

    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &metadata])] }),
        StatusCode::OK,
    )
    .await;

    let verdict = &body["verdicts"][0];
    assert_eq!(verdict["asset_id"], asset.as_str());
    assert_eq!(verdict["durable"], true);
    assert!(
        !verdict["checked_at"]
            .as_str()
            .expect("an instant")
            .is_empty(),
        "a verdict says when the server looked, or a stale one reads as fresh"
    );
    for blob in verdict["blobs"].as_array().expect("per-blob detail") {
        assert_eq!(blob["stored"], true);
        assert_eq!(blob["indexed"], true);
        assert_eq!(blob["retrievable"], true);
    }
}

/// Bytes the store lost make the asset not durable, and the detail says which blob.
#[tokio::test]
async fn a_missing_blob_is_not_durable_and_is_named() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, metadata) = publish(&fixture, "lost").await;
    fixture
        .blobs
        .remove(&metadata)
        .await
        .expect("the store removes");

    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &metadata])] }),
        StatusCode::OK,
    )
    .await;

    let verdict = &body["verdicts"][0];
    assert_eq!(verdict["durable"], false);
    let blobs = verdict["blobs"].as_array().expect("per-blob detail");
    assert_eq!(
        blobs[0]["stored"], true,
        "the provenance blob is still there"
    );
    assert_eq!(blobs[1]["stored"], false);
    assert_eq!(
        blobs[1]["indexed"], true,
        "the index still references it, which is exactly what makes this a loss"
    );
    assert_eq!(blobs[1]["retrievable"], false);
    assert_eq!(blobs[1]["role"], "metadata");
}

/// A hash this asset does not hold is reported unassociated — **even when the bytes exist**.
#[tokio::test]
async fn an_unassociated_hash_is_not_stored_even_when_the_bytes_are_there() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, _) = publish(&fixture, "wrong-hash").await;
    // Real bytes, in the store, belonging to a different asset.
    let (_, elsewhere, _) = publish(&fixture, "somebody-elses-asset").await;
    assert!(
        fixture
            .blobs
            .stat(&elsewhere)
            .await
            .expect("stat")
            .is_some(),
        "the fixture must actually hold these bytes, or this case proves nothing"
    );

    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &elsewhere])] }),
        StatusCode::OK,
    )
    .await;

    let blobs = body["verdicts"][0]["blobs"]
        .as_array()
        .expect("per-blob detail");
    assert_eq!(blobs.len(), 2, "a declared hash is never silently omitted");
    assert_eq!(blobs[1]["indexed"], false);
    assert_eq!(
        blobs[1]["stored"], false,
        "the store is not asked about a hash this asset does not hold: answering would turn a \
         durability query into a cross-account existence oracle"
    );
    assert_eq!(blobs[1]["role"], "unknown");
    assert_eq!(body["verdicts"][0]["durable"], false);
}

/// Another account's asset is indistinguishable from one that never existed.
#[tokio::test]
async fn another_owners_asset_is_not_verifiable() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let theirs = OwnerId::new("somebody-else");
    let (asset, provenance, metadata) = publish_for(&fixture, &theirs, "not-mine").await;

    let mine = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &metadata])] }),
        StatusCode::OK,
    )
    .await;

    let unknown = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(
            &AssetId::new("no-such-asset-at-all"),
            &[&provenance, &metadata],
        )] }),
        StatusCode::OK,
    )
    .await;

    assert_eq!(mine["verdicts"][0]["durable"], false);
    assert_eq!(
        mine["verdicts"][0]["blobs"], unknown["verdicts"][0]["blobs"],
        "a verdict about another account's asset must look exactly like one about no asset"
    );
}

/// A deleted asset is not durable, whatever the store still holds.
#[tokio::test]
async fn a_tombstoned_asset_is_not_durable() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, metadata) = publish(&fixture, "deleted").await;
    fixture
        .index
        .tombstone(&asset, Timestamp::UNIX_EPOCH)
        .await
        .expect("the index tombstones");

    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &metadata])] }),
        StatusCode::OK,
    )
    .await;
    assert_eq!(body["verdicts"][0]["durable"], false);
}

/// Quarantining takes the bytes out of the store, so the verdict follows with no extra fact.
#[tokio::test]
async fn a_quarantined_blob_is_not_retrievable() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, metadata) = publish(&fixture, "quarantined").await;
    fixture
        .blobs
        .quarantine(
            &provenance,
            capsule_server::blob::QuarantineReason {
                code: "error.scrub.hash_mismatch".to_owned(),
                detail: "the bytes do not hash to their address".to_owned(),
                at: Timestamp::UNIX_EPOCH,
            },
        )
        .await
        .expect("the store quarantines");

    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &metadata])] }),
        StatusCode::OK,
    )
    .await;
    let blobs = body["verdicts"][0]["blobs"]
        .as_array()
        .expect("per-blob detail");
    assert_eq!(blobs[0]["retrievable"], false);
    assert_eq!(body["verdicts"][0]["durable"], false);
}

/// A marked blob is stored and **not** retrievable — the combination that matters most here.
#[tokio::test]
async fn a_blob_awaiting_collection_is_stored_but_not_retrievable() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, metadata) = publish(&fixture, "collectable").await;
    fixture
        .marks
        .mark(&provenance, Timestamp::UNIX_EPOCH)
        .await
        .expect("the collector marks");

    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &metadata])] }),
        StatusCode::OK,
    )
    .await;
    let blobs = body["verdicts"][0]["blobs"]
        .as_array()
        .expect("per-blob detail");
    assert_eq!(
        blobs[0]["stored"], true,
        "the bytes are on disk right now, and saying otherwise would be a lie a client could \
         catch"
    );
    assert_eq!(
        blobs[0]["retrievable"], false,
        "and they are on their way out, so a client that read `stored` alone and released its \
         copy would be releasing it into a window that closes"
    );
    assert_eq!(body["verdicts"][0]["durable"], false);
}

/// Declaring nothing must not be a vacuous yes.
#[tokio::test]
async fn an_empty_declaration_is_refused_rather_than_vacuously_durable() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, _, _) = publish(&fixture, "empty-declaration").await;

    let problem = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [{ "asset_id": asset.as_str(), "blob_hashes": [] }] }),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(problem["code"], "error.storage.invalid_request");
    assert!(
        problem["detail"]
            .as_str()
            .expect("a detail")
            .contains("vacuously durable"),
        "the refusal must say why, because `durable: true` over nothing is the dangerous answer"
    );
}

/// Every structural refusal is the same coded `400`.
#[tokio::test]
async fn a_malformed_question_is_refused_rather_than_answered() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;

    for body in [
        json!({ "assets": [] }),
        json!({ "assets": [{ "asset_id": "  ", "blob_hashes": ["ab"] }] }),
        json!({ "assets": [{ "asset_id": "an-asset", "blob_hashes": ["not-a-hash"] }] }),
    ] {
        let problem = verify(&fixture, &bearer, &body, StatusCode::BAD_REQUEST).await;
        assert_eq!(problem["code"], "error.storage.invalid_request");
    }
}

/// The per-request bounds are enforced, so one request cannot buy unbounded work.
#[tokio::test]
async fn a_request_past_its_bounds_is_refused() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, _) = publish(&fixture, "bounded").await;

    let too_many_assets = json!({
        "assets": (0..=capsule_server::verify::MAX_ASSETS_PER_REQUEST)
            .map(|n| json!({
                "asset_id": format!("asset-{n}"),
                "blob_hashes": [provenance.as_str()],
            }))
            .collect::<Vec<_>>(),
    });
    verify(&fixture, &bearer, &too_many_assets, StatusCode::BAD_REQUEST).await;

    let too_many_blobs = json!({
        "assets": [{
            "asset_id": asset.as_str(),
            "blob_hashes": vec![
                provenance.as_str();
                capsule_server::verify::MAX_BLOBS_PER_ASSET + 1
            ],
        }],
    });
    verify(&fixture, &bearer, &too_many_blobs, StatusCode::BAD_REQUEST).await;
}

/// Verdicts come back in request order, so a client can match them positionally.
#[tokio::test]
async fn verdicts_are_returned_in_request_order() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (first, first_p, first_m) = publish(&fixture, "ordered-one").await;
    let (second, second_p, second_m) = publish(&fixture, "ordered-two").await;

    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [
            declare(&second, &[&second_p, &second_m]),
            declare(&first, &[&first_p, &first_m]),
        ] }),
        StatusCode::OK,
    )
    .await;

    let verdicts = body["verdicts"].as_array().expect("verdicts");
    assert_eq!(verdicts[0]["asset_id"], second.as_str());
    assert_eq!(verdicts[1]["asset_id"], first.as_str());
}

/// A store that cannot answer is an error, never an optimistic verdict.
#[tokio::test]
async fn an_unreachable_index_is_a_failure_not_a_verdict() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, metadata) = publish(&fixture, "unreachable").await;
    fixture.index.set_unavailable(true);

    let problem = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &metadata])] }),
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    assert_eq!(
        problem["code"], "error.storage.unavailable",
        "an outage reported as `durable: false` would train a user to ignore the state that \
         actually means their photos are gone"
    );
}

/// Verification needs a credential.
#[tokio::test]
async fn verification_requires_a_credential() {
    let fixture = Fixture::working();
    fixture
        .client
        .post("/v1/storage/verify")
        .json(&json!({ "assets": [] }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
