//! `POST /v1/storage/verify` — the key-free durability verdict (slice `S-C3`), end to end.
//!
//! Every case here is really the same question asked from a different angle: *would a client
//! that believed this answer and deleted its local copy have lost the photo?* That is why the
//! failure directions are not symmetric. A wrong `durable = false` costs a client some disk;
//! a wrong `durable = true` costs a user their photograph. The suite is weighted accordingly —
//! most of it is about the ways this endpoint must refuse to say yes.

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::storage_attestation::{
    AttestationRejection, DeclaredAsset, StorageAttestation, verify_storage_attestation,
};
use capsule_server::blob::{BlobStore, ContentAddress};
use capsule_server::gc::CollectionStore;
use capsule_server::index::{AssetIndex, BlobRecord, PendingAsset, ServingHold};
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
                manifest_sha256: None,
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

/// A held asset is stored and **not** retrievable, which is the honest pair.
///
/// `S-C17`. This is the decision the whole surface exists to inform: a client asks before it
/// releases its only local copy. A taken-down asset's bytes are present and staying present —
/// so `stored` is true and lying about it would be caught — but the server will not hand them
/// over, so `retrievable` must be false. Reporting durable here would tell a client it may drop
/// the last copy of a photo it can never fetch back.
#[tokio::test]
async fn a_held_asset_is_stored_but_not_retrievable() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, metadata) = publish(&fixture, "held").await;
    fixture
        .index
        .set_hold(&asset, Some(ServingHold::Takedown))
        .await
        .expect("the hold is placed");

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
    for blob in blobs {
        assert_eq!(
            blob["stored"], true,
            "a takedown preserves the bytes; the server still holds them"
        );
        assert_eq!(
            blob["retrievable"], false,
            "and will not serve them, so a client must not be told it can fetch them back"
        );
    }
    assert_eq!(body["verdicts"][0]["durable"], false);

    // Reversible by default: lifting restores the promise as well as the serving.
    fixture
        .index
        .set_hold(&asset, None)
        .await
        .expect("the hold is lifted");
    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &metadata])] }),
        StatusCode::OK,
    )
    .await;
    assert_eq!(body["verdicts"][0]["durable"], true);
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

#[tokio::test]
async fn a_deep_scan_catches_corruption_a_structural_check_calls_stored() {
    // The contract's own deep-scan bullet, and the whole reason the option exists: `stored` is a
    // question about the filesystem, and a blob whose bytes have rotted underneath it is still
    // stored. Only re-hashing tells the two apart.
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, metadata) = publish(&fixture, "deep").await;

    // Structural first: everything is fine, and `deep` is absent because nobody looked.
    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &metadata])] }),
        StatusCode::OK,
    )
    .await;
    let blobs = body["verdicts"][0]["blobs"].as_array().expect("blobs");
    assert_eq!(blobs[0]["stored"], true);
    assert!(
        blobs[0].get("deep").is_none(),
        "absent means nobody looked at the bytes, which is not the same as looking and finding \
         them fine"
    );

    // Now a deep scan over intact bytes.
    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &metadata])], "deep": true }),
        StatusCode::OK,
    )
    .await;
    let blobs = body["verdicts"][0]["blobs"].as_array().expect("blobs");
    assert_eq!(blobs[0]["deep"], "intact");
    assert_eq!(body["verdicts"][0]["durable"], true);

    // Rot the bytes under the address, exactly as a failing disk would.
    fixture.blobs.corrupt(&provenance).await;

    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &metadata])], "deep": true }),
        StatusCode::OK,
    )
    .await;
    let blobs = body["verdicts"][0]["blobs"].as_array().expect("blobs");
    assert_eq!(
        blobs[0]["stored"], true,
        "the structural check still says stored, which is the point"
    );
    assert_eq!(blobs[0]["deep"], "corrupt");
}

#[tokio::test]
async fn the_deep_budget_throttles_the_deep_half_and_keeps_the_structural_one() {
    // A deep scan is an *addition* to a structural verdict. Refusing the whole request when the
    // optional half is throttled would make the limiter cost a client more than it saves, and a
    // client that stopped verifying at all is the outcome nobody wants.
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, metadata) = publish(&fixture, "budget").await;
    let request = json!({ "assets": [declare(&asset, &[&provenance, &metadata])], "deep": true });

    for _ in 0..4 {
        let body = verify(&fixture, &bearer, &request, StatusCode::OK).await;
        assert_eq!(body["verdicts"][0]["blobs"][0]["deep"], "intact");
    }

    let body = verify(&fixture, &bearer, &request, StatusCode::OK).await;
    assert_eq!(
        body["verdicts"][0]["blobs"][0]["deep"], "rate_limited",
        "the client is told the bytes were not read, rather than left to infer it from an \
         absent field that also means `you did not ask`"
    );
    assert_eq!(
        body["verdicts"][0]["durable"], true,
        "and the structural verdict is still there and still usable"
    );

    // A structural request is not throttled at all: it is cheap, and spending the deep budget on
    // one would throttle the operation clients actually run.
    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &metadata])] }),
        StatusCode::OK,
    )
    .await;
    assert!(body["verdicts"][0]["blobs"][0].get("deep").is_none());
}

#[tokio::test]
async fn a_blob_the_asset_does_not_hold_is_never_re_hashed() {
    // The store is deliberately not asked about it even on a deep scan: answering would be a
    // cross-account existence oracle, which is the property `S-C3` built this surface around.
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, _) = publish(&fixture, "unassociated-deep").await;
    let stranger = store(&fixture, b"somebody else's bytes").await;

    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance, &stranger])], "deep": true }),
        StatusCode::OK,
    )
    .await;
    let blobs = body["verdicts"][0]["blobs"].as_array().expect("blobs");
    assert_eq!(blobs[1]["stored"], false);
    assert!(
        blobs[1].get("deep").is_none(),
        "a hash this asset does not hold has no deep verdict, however much the store may hold it"
    );
}

// ===========================================================================================
// Signed storage attestation (transfer/durability.md, Signed Storage Attestation)
// ===========================================================================================

/// A nonce, distinct per `seed`.
fn nonce(seed: u8) -> [u8; 32] {
    [seed; 32]
}

/// Decode the response's signed attestation.
fn attestation_of(body: &Value) -> StorageAttestation {
    let encoded = body["attestation"]
        .as_str()
        .expect("a signed request carries an attestation");
    let bytes = BASE64.decode(encoded).expect("base64");
    StorageAttestation::from_canonical_cbor(&bytes).expect("a canonical attestation")
}

/// The client's side of the question, for [`verify_storage_attestation`].
fn declared(asset: &AssetId, hashes: &[&ContentAddress]) -> Vec<DeclaredAsset> {
    vec![DeclaredAsset {
        asset_id: asset.as_str().to_owned(),
        blob_hashes: hashes
            .iter()
            .map(|hash| Hash32::from_hex(hash.as_str()).expect("a digest"))
            .collect(),
    }]
}

#[tokio::test]
async fn a_signed_verdict_verifies_under_the_attestation_key_and_binds_the_nonce() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, metadata) = publish(&fixture, "signed").await;
    let sent = nonce(0x42);

    let body = verify(
        &fixture,
        &bearer,
        &json!({
            "assets": [declare(&asset, &[&provenance, &metadata])],
            "attestation_nonce": Hash32(sent).to_hex(),
        }),
        StatusCode::OK,
    )
    .await;

    let attestation = attestation_of(&body);
    let pinned = [fixture.attestation_key.verifying_key()];
    let asked = declared(&asset, &[&provenance, &metadata]);
    assert_eq!(
        verify_storage_attestation(&attestation, &pinned, &sent, &asked),
        Ok(())
    );
    // The signed answer and the readable one are the same answer.
    let verdict = &attestation.core.verdicts[0];
    assert!(verdict.durable);
    assert_eq!(body["verdicts"][0]["durable"], true);
    assert_eq!(
        attestation.core.checked_at,
        body["verdicts"][0]["checked_at"]
            .as_str()
            .expect("an instant")
    );
    assert_eq!(verdict.blobs[0].role, "provenance");
    assert!(verdict.blobs.iter().all(|blob| blob.deep.is_none()));

    // A replay: the same signed answer presented to a later request with a new nonce.
    assert_eq!(
        verify_storage_attestation(&attestation, &pinned, &nonce(0x43), &asked),
        Err(AttestationRejection::Nonce)
    );
    // A tamper: a server that later lost the bytes cannot rewrite what it signed.
    let mut tampered = attestation.clone();
    tampered.core.verdicts[0].blobs[1].stored = false;
    assert_eq!(
        verify_storage_attestation(&tampered, &pinned, &sent, &asked),
        Err(AttestationRejection::Signature)
    );
}

#[tokio::test]
async fn a_signed_verdict_records_a_loss_as_evidence() {
    // The reason the signed form exists: an honest server's self-reported loss becomes a
    // statement the client can keep.
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, metadata) = publish(&fixture, "signed-loss").await;
    fixture
        .blobs
        .remove(&metadata)
        .await
        .expect("the store removes");

    let body = verify(
        &fixture,
        &bearer,
        &json!({
            "assets": [declare(&asset, &[&provenance, &metadata])],
            "attestation_nonce": Hash32(nonce(7)).to_hex(),
        }),
        StatusCode::OK,
    )
    .await;
    let attestation = attestation_of(&body);
    assert_eq!(
        verify_storage_attestation(
            &attestation,
            &[fixture.attestation_key.verifying_key()],
            &nonce(7),
            &declared(&asset, &[&provenance, &metadata]),
        ),
        Ok(())
    );
    assert!(!attestation.core.verdicts[0].durable);
    assert!(!attestation.core.verdicts[0].blobs[1].stored);
}

#[tokio::test]
async fn an_unsigned_request_carries_no_attestation() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, _) = publish(&fixture, "unsigned").await;
    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance])] }),
        StatusCode::OK,
    )
    .await;
    assert!(body.get("attestation").is_none());
}

#[tokio::test]
async fn a_malformed_nonce_is_refused() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, _) = publish(&fixture, "bad-nonce").await;
    for bad in [
        String::new(),
        "ab".to_owned(),
        Hash32(nonce(0xab)).to_hex().to_uppercase(),
        "zz".repeat(32),
        format!("{}00", Hash32(nonce(1)).to_hex()),
    ] {
        let body = verify(
            &fixture,
            &bearer,
            &json!({
                "assets": [declare(&asset, &[&provenance])],
                "attestation_nonce": bad,
            }),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(body["code"], "error.storage.invalid_request");
    }
}

#[tokio::test]
async fn signing_spends_the_deep_budget_and_a_throttled_signed_request_is_a_429() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, metadata) = publish(&fixture, "signed-budget").await;
    let signed = |seed: u8| {
        json!({
            "assets": [declare(&asset, &[&provenance, &metadata])],
            "attestation_nonce": Hash32(nonce(seed)).to_hex(),
        })
    };

    // One budget: two signed requests and two deep ones spend it.
    for seed in 0..2 {
        verify(&fixture, &bearer, &signed(seed), StatusCode::OK).await;
    }
    for _ in 0..2 {
        let body = verify(
            &fixture,
            &bearer,
            &json!({ "assets": [declare(&asset, &[&provenance])], "deep": true }),
            StatusCode::OK,
        )
        .await;
        assert_eq!(body["verdicts"][0]["blobs"][0]["deep"], "intact");
    }

    // A signed request past it is refused whole, with a wait.
    let refused = fixture
        .client
        .post("/v1/storage/verify")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&signed(9))
        .send()
        .await;
    refused.assert_status(StatusCode::TOO_MANY_REQUESTS);
    let body = refused.json::<Value>();
    assert_eq!(body["code"], "error.storage.deep_rate_limited");
    let instant = body["retry_after"].as_u64().expect("a retry instant");
    let now = u64::try_from(fixture.now().as_second()).expect("a post-epoch clock");
    let header = refused
        .header("retry-after")
        .expect("and Retry-After")
        .parse::<u64>()
        .expect("delta-seconds");
    assert_eq!(header, instant - now);

    // A deep request past it still answers, structurally, as before.
    let body = verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance])], "deep": true }),
        StatusCode::OK,
    )
    .await;
    assert_eq!(body["verdicts"][0]["blobs"][0]["deep"], "rate_limited");

    // And a structural, unsigned request is never charged.
    verify(
        &fixture,
        &bearer,
        &json!({ "assets": [declare(&asset, &[&provenance])] }),
        StatusCode::OK,
    )
    .await;
}

#[tokio::test]
async fn a_signed_deep_request_is_charged_once() {
    // Four signed deep requests fit the four-an-hour budget: one request, one charge.
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, provenance, _) = publish(&fixture, "signed-deep").await;
    for seed in 0..4 {
        let body = verify(
            &fixture,
            &bearer,
            &json!({
                "assets": [declare(&asset, &[&provenance])],
                "deep": true,
                "attestation_nonce": Hash32(nonce(seed)).to_hex(),
            }),
            StatusCode::OK,
        )
        .await;
        let attestation = attestation_of(&body);
        assert_eq!(
            attestation.core.verdicts[0].blobs[0].deep.as_deref(),
            Some("intact"),
            "the deep finding is inside the signature"
        );
    }
}
