//! `GET /v1/blob/{hash}` — key-free ranged blob serving (slice `S-C10`), end to end.
//!
//! The status taxonomy is the contract here, so most of these cases are about *which* refusal
//! rather than about bytes: `404` and `410` make a client degrade permanently, `409` makes it
//! wait, and getting the split wrong makes a client abandon an asset whose original is still
//! being uploaded. The byte cases exist to prove the range rides on the blob **port** — every
//! one of them serves out of an in-memory store, which is only possible because nothing here
//! assumes a file.

mod support;

use capsule_server::blob::{BlobStore, ContentAddress};
use capsule_server::index::{AssetIndex, BlobRecord, PendingAsset};
use capsule_server::store::{AssetId, BlobRole};
use jiff::Timestamp;
use kynos::http::StatusCode;
use support::{Fixture, PROTOCOL_VERSION, album, owner, payload};

/// The ciphertext chunk a client ranges at — a 65,520-byte plaintext chunk plus its GCM tag.
///
/// Spelled out rather than imported because the *server* does not know it: this surface serves
/// whatever span is asked for, and the stride is the client's business. A test that read the
/// constant from core would be asserting a coupling the design deliberately does not have.
const CIPHERTEXT_CHUNK: u64 = 65_536;

/// Three ciphertext chunks, so a range can start and end on a stride boundary and still have
/// something on either side of it.
fn ciphertext() -> Vec<u8> {
    let mut bytes = Vec::with_capacity(3 * CIPHERTEXT_CHUNK as usize);
    for chunk in 0..3_u8 {
        bytes.extend(payload(b'a' + chunk, CIPHERTEXT_CHUNK as usize));
    }
    bytes
}

/// Put `bytes` in the store at their own address.
async fn store(fixture: &Fixture, bytes: &[u8]) -> ContentAddress {
    let address = ContentAddress::parse(&support::checksum(bytes)).expect("a content address");
    fixture.blobs.put(&address, bytes).await.expect("stored");
    address
}

/// Reserve `asset` and land its index tier, so it is published.
async fn publish(fixture: &Fixture, asset: &str) -> AssetId {
    let id = AssetId::new(asset);
    fixture
        .index
        .reserve(PendingAsset {
            asset_id: id.clone(),
            owner_id: owner(),
            album_id: album(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            crypto_suite_id: 1,
            created_at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the index reserves");
    for (role, seed) in [
        (BlobRole::Provenance, format!("{asset}-provenance")),
        (BlobRole::Metadata, format!("{asset}-metadata")),
    ] {
        let address = store(fixture, seed.as_bytes()).await;
        reference(fixture, &id, role, &address, seed.len() as u64).await;
    }
    id
}

/// Record `address` against `asset` in the index.
async fn reference(
    fixture: &Fixture,
    asset: &AssetId,
    role: BlobRole,
    address: &ContentAddress,
    size: u64,
) {
    fixture
        .index
        .record_blob(
            asset,
            BlobRecord {
                role,
                address: address.clone(),
                size,
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

/// A published asset holding `bytes` as its original, and the address they live at.
async fn published_original(fixture: &Fixture, asset: &str, bytes: &[u8]) -> ContentAddress {
    let id = publish(fixture, asset).await;
    let address = store(fixture, bytes).await;
    reference(
        fixture,
        &id,
        BlobRole::Original,
        &address,
        bytes.len() as u64,
    )
    .await;
    address
}

// ===========================================================================================

/// The whole blob, byte for byte, with the address as its validator.
#[tokio::test]
async fn a_live_blob_is_served_whole() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = ciphertext();
    let address = published_original(&fixture, "whole", &bytes).await;

    let response = fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await;

    response.assert_status(StatusCode::OK);
    assert_eq!(
        response.bytes().as_ref(),
        bytes.as_slice(),
        "the server served something other than the ciphertext it holds"
    );
    assert_eq!(
        response.header("etag"),
        Some(format!("\"{address}\"").as_str()),
        "the content address is the validator; there is no other honest one"
    );
    assert_eq!(response.header("accept-ranges"), Some("bytes"));
    assert_eq!(
        response.header("content-type"),
        Some("application/octet-stream"),
        "the server holds no key and must never claim to know what these bytes are"
    );
}

/// A range at the ciphertext stride, which is the only shape a client actually asks for.
#[tokio::test]
async fn a_range_is_served_at_the_ciphertext_stride() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = ciphertext();
    let address = published_original(&fixture, "ranged", &bytes).await;
    let complete = bytes.len() as u64;

    // The middle chunk: a client that already decrypted chunk 0 and wants chunk 1.
    let first = CIPHERTEXT_CHUNK;
    let last = 2 * CIPHERTEXT_CHUNK - 1;
    let response = fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .header("range", &format!("bytes={first}-{last}"))
        .send()
        .await;

    response.assert_status(StatusCode::PARTIAL_CONTENT);
    response.assert_part(first, last, complete);
    assert_eq!(
        response.bytes().as_ref(),
        &bytes[first as usize..=last as usize],
        "the span served is not the span asked for, so a chunk would decrypt to noise"
    );
}

/// Two halves fetched separately splice into exactly what one fetch would have produced.
#[tokio::test]
async fn a_resumed_fetch_splices_into_the_whole() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = ciphertext();
    let address = published_original(&fixture, "resumed", &bytes).await;
    let split = CIPHERTEXT_CHUNK;

    let mut spliced = Vec::new();
    for range in [format!("bytes=0-{}", split - 1), format!("bytes={split}-")] {
        let response = fixture
            .client
            .get(&format!("/v1/blob/{address}"))
            .header("authorization", &bearer)
            .header("range", &range)
            .header("if-range", &format!("\"{address}\""))
            .send()
            .await;
        response.assert_status(StatusCode::PARTIAL_CONTENT);
        spliced.extend_from_slice(response.bytes());
    }

    assert_eq!(
        spliced, bytes,
        "an interrupted download did not resume into the same object"
    );
}

/// A range past the end is refused rather than clamped, so a client learns it was wrong.
#[tokio::test]
async fn an_unsatisfiable_range_is_refused() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = ciphertext();
    let address = published_original(&fixture, "unsatisfiable", &bytes).await;

    let response = fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .header("range", &format!("bytes={}-", bytes.len() as u64 + 1))
        .send()
        .await;

    response.assert_status(StatusCode::RANGE_NOT_SATISFIABLE);
}

/// A client that already holds the bytes is told so, and no octets cross the wire.
#[tokio::test]
async fn a_matching_validator_is_not_modified() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = ciphertext();
    let address = published_original(&fixture, "cached", &bytes).await;

    let response = fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .header("if-none-match", &format!("\"{address}\""))
        .send()
        .await;

    response.assert_status(StatusCode::NOT_MODIFIED);
    assert!(
        response.bytes().is_empty(),
        "a 304 that carries the body saves nothing"
    );
}

/// An unknown address and a malformed one answer identically, or this route is an oracle.
#[tokio::test]
async fn an_unknown_address_is_indistinguishable_from_a_malformed_one() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    // A well-formed address nothing holds, and a string that is not an address at all.
    let unknown = support::checksum(b"never stored anywhere");

    let mut bodies = Vec::new();
    for hash in [unknown.as_str(), "not-a-content-address"] {
        let response = fixture
            .client
            .get(&format!("/v1/blob/{hash}"))
            .header("authorization", &bearer)
            .send()
            .await;
        response.assert_status(StatusCode::NOT_FOUND);
        let problem: serde_json::Value = response.json();
        assert_eq!(problem["code"], "error.blob.not_found");
        bodies.push(problem);
    }

    assert_eq!(
        bodies[0], bodies[1],
        "a distinguishable malformed-address answer tells a caller when a guess was well-formed"
    );
}

/// A half-finished upload is not fetchable: it is in nobody's feed.
#[tokio::test]
async fn an_unpublished_assets_blob_is_unknown() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let id = AssetId::new("unpublished");
    fixture
        .index
        .reserve(PendingAsset {
            asset_id: id.clone(),
            owner_id: owner(),
            album_id: album(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            crypto_suite_id: 1,
            created_at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the index reserves");
    let bytes = ciphertext();
    let address = store(&fixture, &bytes).await;
    reference(
        &fixture,
        &id,
        BlobRole::Provenance,
        &address,
        bytes.len() as u64,
    )
    .await;

    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// A deleted asset's blob is gone — permanently, so the client stops asking.
#[tokio::test]
async fn a_deleted_assets_blob_is_gone() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = ciphertext();
    let address = published_original(&fixture, "deleted", &bytes).await;
    fixture
        .index
        .tombstone(&AssetId::new("deleted"), Timestamp::UNIX_EPOCH)
        .await
        .expect("the index tombstones");

    let response = fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await;
    response.assert_status(StatusCode::GONE);
    let problem: serde_json::Value = response.json();
    assert_eq!(problem["code"], "error.blob.gone");
}

/// Deleting one asset must not take a shared blob's other holder with it.
#[tokio::test]
async fn a_shared_blob_survives_one_holders_deletion() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = payload(b'd', 4096);
    let address = store(&fixture, &bytes).await;

    for asset in ["sharer-one", "sharer-two"] {
        let id = publish(&fixture, asset).await;
        reference(
            &fixture,
            &id,
            BlobRole::Derivative,
            &address,
            bytes.len() as u64,
        )
        .await;
    }
    fixture
        .index
        .tombstone(&AssetId::new("sharer-one"), Timestamp::UNIX_EPOCH)
        .await
        .expect("the index tombstones");

    let response = fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await;
    response.assert_status(StatusCode::OK);
    assert_eq!(
        response.bytes().as_ref(),
        bytes.as_slice(),
        "content addressing means one blob serves many assets; deleting one must not \
         collect the bytes another still holds"
    );
}

/// The `awaiting-original` state is **not observable here**, and this pins that rather than
/// asserting the answer the contract wants.
///
/// The Salvo surface answered a transient `409 error.blob.pending_upload` for an original still
/// uploading. This port cannot: it learns an address at finalization, so a reference implies
/// bytes and missing bytes imply no reference. Recording the promise at reservation instead
/// would make an abandoned session promise an original forever — a permanent `409`, which is
/// the exact failure the split exists to prevent — so the shape is `S-C40`'s to decide.
///
/// Until then a missing original is a dangling reference like any other. This case exists so
/// that closing `S-C40` **fails a test** rather than quietly changing an unwatched status.
#[tokio::test]
async fn an_originals_absence_is_indistinguishable_from_a_dangling_reference() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let id = publish(&fixture, "awaiting").await;
    // Only reachable by writing the reference directly: no upload path produces one, because
    // finalization records the reference and the bytes together.
    let promised = ContentAddress::parse(&support::checksum(b"an original still uploading"))
        .expect("a content address");
    reference(&fixture, &id, BlobRole::Original, &promised, 8192).await;

    let response = fixture
        .client
        .get(&format!("/v1/blob/{promised}"))
        .header("authorization", &bearer)
        .send()
        .await;
    response.assert_status(StatusCode::GONE);
    let problem: serde_json::Value = response.json();
    assert_eq!(
        problem["code"], "error.blob.gone",
        "when `S-C40` lands this becomes 409 error.blob.pending_upload, and this assertion is \
         how anyone finds out the status moved"
    );
}

/// A referenced blob whose bytes are absent for any *other* reason is a dangling reference.
#[tokio::test]
async fn a_dangling_reference_is_gone_rather_than_pending() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let id = publish(&fixture, "dangling").await;
    let missing = ContentAddress::parse(&support::checksum(b"a derivative that was lost"))
        .expect("a content address");
    reference(&fixture, &id, BlobRole::Derivative, &missing, 4096).await;

    let response = fixture
        .client
        .get(&format!("/v1/blob/{missing}"))
        .header("authorization", &bearer)
        .send()
        .await;
    response.assert_status(StatusCode::GONE);
    let problem: serde_json::Value = response.json();
    assert_eq!(
        problem["code"], "error.blob.gone",
        "only a missing original on an awaiting-original asset is transient"
    );
}

/// Quarantining takes the bytes out of the store, which is already `410` without a second check.
#[tokio::test]
async fn a_quarantined_blob_is_gone_without_a_liveness_flag() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = ciphertext();
    let address = published_original(&fixture, "quarantined", &bytes).await;

    fixture
        .blobs
        .quarantine(
            &address,
            capsule_server::blob::QuarantineReason {
                code: "error.scrub.hash_mismatch".to_owned(),
                detail: "the bytes do not hash to their address".to_owned(),
                at: Timestamp::UNIX_EPOCH,
            },
        )
        .await
        .expect("the store quarantines");

    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::GONE);
}

/// Every fetch needs a credential; ciphertext is not public just because it is opaque.
#[tokio::test]
async fn serving_requires_a_credential() {
    let fixture = Fixture::working();
    let bytes = ciphertext();
    let address = published_original(&fixture, "guarded", &bytes).await;

    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

/// A store that cannot answer is a coded `500`, never a `404` that says the blob is gone.
#[tokio::test]
async fn an_unreachable_index_is_a_coded_failure_not_a_missing_blob() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = ciphertext();
    let address = published_original(&fixture, "unreachable", &bytes).await;
    fixture.index.set_unavailable(true);

    let response = fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await;
    response.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    let problem: serde_json::Value = response.json();
    assert_eq!(
        problem["code"], "error.blob.unavailable",
        "a client told `404` here would delete its local copy over a transient outage"
    );
}
