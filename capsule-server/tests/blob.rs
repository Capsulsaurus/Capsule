//! `GET /v1/blob/{hash}` — key-free ranged blob serving, end to end.
//!
//! The status taxonomy is the contract here, so most of these cases are about *which* refusal
//! rather than about bytes: `404` and `410` make a client degrade permanently, `409` makes it
//! wait, and getting the split wrong makes a client abandon an asset whose original is still
//! being uploaded. The byte cases exist to prove the range rides on the blob **port** — every
//! one of them serves out of an in-memory store, which is only possible because nothing here
//! assumes a file.

mod support;

use capsule_server::blob::{BlobStore, ContentAddress};
use capsule_server::gc::CollectionStore;
use capsule_server::index::{AssetIndex, BlobRecord, HoldOutcome, PendingAsset, ServingHold};
use capsule_server::membership::{MemberRole, MembershipStore as _, RosterRecord};
use capsule_server::store::{AlbumId, AssetId, BlobRole, OwnerId, UserId};
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
    publish_in(fixture, asset, &owner(), &album()).await
}

/// The same, under another owner's album.
async fn publish_in(fixture: &Fixture, asset: &str, owner: &OwnerId, album: &AlbumId) -> AssetId {
    let id = AssetId::new(asset);
    fixture
        .index
        .reserve(PendingAsset {
            asset_id: id.clone(),
            owner_id: owner.clone(),
            album_id: album.clone(),
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
                manifest_sha256: None,
                amk_version: None,
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

/// An original still uploading is a transient `409`, not a permanent `404`.
///
/// The whole point of the status split: a client told `404` degrades to the representation it
/// already holds and stops asking, which for an original that is thirty seconds from arriving
/// is exactly the wrong behaviour.
///
/// The promise is the **upload session**, not an index row. That is what makes this reachable at
/// all — the index records a reference at finalization, after the bytes commit, so it has
/// nothing to say about bytes in flight — and it is what keeps the promise bounded: an
/// abandoned session expires and takes the `409` with it, instead of leaving a permanent one.
#[tokio::test]
async fn an_original_still_uploading_is_transient_rather_than_unknown() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = payload(b'o', 8192);
    let address = ContentAddress::parse(&support::checksum(&bytes)).expect("a content address");

    // Nothing knows about it yet.
    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);

    // Now the account declares it. No bytes have been sent, no reference exists, and the index
    // is exactly as ignorant as it was a moment ago — the answer changes because the *session*
    // exists.
    let upload = fixture.open_session(&bytes, "original", &bearer).await;
    let response = fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await;
    response.assert_status(StatusCode::CONFLICT);
    let problem: serde_json::Value = response.json();
    assert_eq!(problem["code"], "error.blob.pending_upload");
    assert!(
        problem.get("upload_id").is_none() && problem.get("received_bytes").is_none(),
        "the fetcher is a different device from the uploader; which session and how far along \
         are another device's business and nothing a client can act on"
    );

    // Abandoning the upload withdraws the promise, and the address goes back to being unknown.
    // This is the property that made the session the right place to keep it: no reconciliation
    // worker had to be written to stop a transient status becoming a permanent one.
    fixture
        .client
        .delete(&format!("/v1/upload/{upload}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// The transient answer reports the caller's own upload and nobody else's.
#[tokio::test]
async fn another_accounts_upload_is_not_this_accounts_pending_answer() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = payload(b'p', 8192);
    let address = ContentAddress::parse(&support::checksum(&bytes)).expect("a content address");
    fixture.open_session(&bytes, "original", &bearer).await;

    // Unscoped, this would tell any authenticated caller who can name a hash that somebody,
    // somewhere, is uploading those exact bytes. The case the `409` exists for is a second
    // device of the *same* account, which learned the address from the signed manifest.
    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;
    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &stranger)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// Finalizing discharges the promise: the transient answer stops, because the thing that made
/// it stopped.
///
/// It does not become a `200` here, and that is not a gap in this slice. A blob is servable once
/// a *visible* asset references it, and an upload alone leaves the asset row pending — the
/// lifecycle op that publishes it is `S-C16`'s, and `a_live_blob_is_served_whole` covers the
/// served end. What this case pins is the boundary the transient status owns: it is true exactly
/// while an upload is in flight, and not one request longer.
#[tokio::test]
async fn finalizing_discharges_the_promise() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = payload(b'q', 8192);
    let address = ContentAddress::parse(&support::checksum(&bytes)).expect("a content address");
    let upload = fixture.open_session(&bytes, "original", &bearer).await;
    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::CONFLICT);

    // One chunk carrying the whole declared size finalizes the session.
    fixture
        .chunk(&upload, 0, &bytes, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    assert_eq!(
        fixture
            .blobs
            .blob_for_test(&support::checksum(&bytes))
            .await,
        Some(bytes),
        "the bytes committed, so the promise was kept rather than abandoned"
    );

    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// A reference with no bytes is `410` whatever its role — a reference is written *after* the
/// bytes commit, so its bytes being absent means they were removed, never that they have not
/// arrived. This is why `S-C40`'s transient answer lives in the no-reference arm.
#[tokio::test]
async fn a_reference_without_bytes_is_gone_even_for_an_original() {
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
    assert_eq!(problem["code"], "error.blob.gone");
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

/// A blob the collector has marked is gone, even though its bytes are still on disk.
#[tokio::test]
async fn a_blob_awaiting_collection_is_gone_while_its_bytes_are_still_there() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = ciphertext();
    let address = published_original(&fixture, "collectable", &bytes).await;

    fixture
        .marks
        .mark(&address, Timestamp::UNIX_EPOCH)
        .await
        .expect("the collector marks");

    let response = fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await;
    response.assert_status(StatusCode::GONE);
    assert!(
        fixture.blobs.stat(&address).await.expect("stat").is_some(),
        "the bytes are still there, which is exactly why the refusal has to come from the mark \
         rather than from their absence: they are on their way out"
    );

    // Cancelling the mark makes it servable again — the reference reappeared, so the sweep is off.
    fixture
        .marks
        .unmark(&address)
        .await
        .expect("the collector unmarks");
    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);
}

/// A taken-down asset stops serving, keeps its bytes, and serves again when the hold is lifted.
///
/// `S-C17`. The three assertions are the whole of design/moderation.md's takedown contract:
/// `410` rather than `404` (a takedown signals removal of content the fetcher already knows
/// about, unlike capability-URL serving, which must not confirm a URL ever existed); the blob
/// is preserved, because the user owns the data and a takedown is a serving constraint rather
/// than a destruction; and it is reversible by default.
#[tokio::test]
async fn a_taken_down_asset_stops_serving_but_keeps_its_bytes() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let bytes = ciphertext();
    let address = published_original(&fixture, "takedown", &bytes).await;
    let asset = AssetId::new("takedown");

    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);

    assert_eq!(
        fixture
            .index
            .set_hold(&asset, Some(ServingHold::Takedown))
            .await
            .expect("the hold is placed"),
        HoldOutcome::Applied
    );

    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::GONE);
    assert!(
        fixture.blobs.stat(&address).await.expect("stat").is_some(),
        "a takedown is a serving constraint, not a destruction: the user's bytes stay",
    );

    fixture
        .index
        .set_hold(&asset, None)
        .await
        .expect("the hold is lifted");
    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);
}

/// A legal hold refuses the same way, and every blob of the asset goes with it.
#[tokio::test]
async fn a_legal_hold_covers_every_blob_of_the_asset() {
    // The hold is the *asset's*, not a blob's, which is what keeps content addressing from
    // making a takedown either leaky or over-broad: two assets legitimately share a thumbnail,
    // so holding an address would take down somebody else's photo.
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let asset = publish(&fixture, "legal-hold").await;
    let original = ciphertext();
    let original_address = store(&fixture, &original).await;
    reference(
        &fixture,
        &asset,
        BlobRole::Original,
        &original_address,
        original.len() as u64,
    )
    .await;
    // The same bytes `publish` stored, so the same content address — which is the point: a hold
    // reaches every blob of the asset, not only the one somebody thought to name.
    let metadata_address = store(&fixture, b"legal-hold-metadata").await;

    fixture
        .index
        .set_hold(&asset, Some(ServingHold::LegalHold))
        .await
        .expect("the hold is placed");

    for address in [&original_address, &metadata_address] {
        fixture
            .client
            .get(&format!("/v1/blob/{address}"))
            .header("authorization", &bearer)
            .send()
            .await
            .assert_status(StatusCode::GONE);
    }
    assert!(
        fixture
            .blobs
            .stat(&original_address)
            .await
            .expect("stat")
            .is_some(),
        "a legal hold preserves the bytes; it is a constraint on serving them",
    );
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

// ===========================================================================================
// Who may fetch
// ===========================================================================================

/// A live blob of somebody else's is unknown, not served.
///
/// This is the hole `S-C39` closed. Before it, `GET /v1/blob/{hash}` authorized on "a valid
/// access token" and nothing else, so **any** authenticated account could fetch **any** live
/// ciphertext whose address it could name. The defence was that a content address is the hash of
/// ciphertext and so a capability rather than a guessable name — true, and not the contract, and
/// a capability that never expires because the address never changes.
#[tokio::test]
async fn another_accounts_live_blob_is_unknown_rather_than_served() {
    let fixture = Fixture::working();
    let owner = bearer(&fixture).await;
    let bytes = ciphertext();
    let address = published_original(&fixture, "mine", &bytes).await;

    // The owner is served, so the address really is live and the refusal below is about who is
    // asking rather than about what is there.
    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &owner)
        .send()
        .await
        .assert_status(StatusCode::OK);

    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;
    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &stranger)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

/// And the refusal is byte-identical to the one an address nothing references gets.
///
/// The disclosure property, asserted rather than assumed. A `403` here — or any answer that
/// differed from the unknown-address answer — would confirm that the address is referenced by
/// *somebody*, which is an existence oracle over content addresses handed to anyone who can name
/// one. The `403` the contract describes is reserved for a caller the server can see once *had*
/// access — a former member, whose row the membership store keeps.
#[tokio::test]
async fn a_strangers_refusal_is_indistinguishable_from_an_unknown_address() {
    let fixture = Fixture::working();
    let owner_bearer = bearer(&fixture).await;
    let bytes = ciphertext();
    let address = published_original(&fixture, "opaque", &bytes).await;
    let _ = &owner_bearer;
    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;

    let refused = fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &stranger)
        .header("accept", "application/problem+json")
        .send()
        .await;
    refused.assert_status(StatusCode::NOT_FOUND);
    let refused: serde_json::Value = refused.json();

    let unknown = fixture
        .client
        .get(&format!("/v1/blob/{}", support::checksum(b"never existed")))
        .header("authorization", &stranger)
        .header("accept", "application/problem+json")
        .send()
        .await;
    unknown.assert_status(StatusCode::NOT_FOUND);
    let unknown: serde_json::Value = unknown.json();

    assert_eq!(
        refused, unknown,
        "a live blob the caller may not have and an address nobody holds must be one answer, or \
         the difference between them is the oracle"
    );
}

/// A stranger cannot read another account's deletions or takedowns off the status line either.
///
/// The ordering property. Every refusal below the authority check — the tombstone `410`, the
/// takedown `410`, the collection `410`, the dangling `410` — is a fact about somebody's asset.
/// Deciding ownership last would have left all of them legible to anyone who could name the
/// address.
#[tokio::test]
async fn a_stranger_cannot_tell_a_takedown_from_an_unknown_address() {
    let fixture = Fixture::working();
    let bytes = ciphertext();
    let address = published_original(&fixture, "held", &bytes).await;
    let held = AssetId::new("held");
    assert_eq!(
        fixture
            .index
            .set_hold(&held, Some(ServingHold::Takedown))
            .await
            .expect("the index holds"),
        HoldOutcome::Applied
    );

    // The owner is told, because a user whose asset stops serving is never left to guess.
    let owner = bearer(&fixture).await;
    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &owner)
        .send()
        .await
        .assert_status(StatusCode::GONE);

    // A stranger is told nothing at all.
    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;
    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", &stranger)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

// ===========================================================================================
// Membership
// ===========================================================================================

/// A second account, on the seeded album's roster in whatever state a case puts it.
const BOB: &str = "01937b7c-0000-7000-8000-0000000000b0";

/// Publish the seeded album's roster at `version`, naming `members`.
async fn roster(fixture: &Fixture, version: u64, members: &[(&str, MemberRole)]) {
    roster_of(fixture, &album(), version, members).await;
}

/// Publish `album`'s roster at `version`, naming `members`.
async fn roster_of(
    fixture: &Fixture,
    album: &AlbumId,
    version: u64,
    members: &[(&str, MemberRole)],
) {
    fixture
        .members
        .apply_roster(
            RosterRecord {
                album_id: album.clone(),
                roster_version: version,
                amk_epoch: version,
                attested_by_device: support::device(),
                received_at: Timestamp::UNIX_EPOCH,
                document: format!("blob-test-v{version}").into_bytes(),
            },
            members
                .iter()
                .map(|(user, role)| (UserId::new(*user), *role))
                .collect(),
        )
        .await
        .expect("the store applies");
}

/// Fetch `address` as `bearer`, asking for a problem body.
async fn fetch(fixture: &Fixture, bearer: &str, address: &str) -> kynos::test::TestResponse {
    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", bearer)
        .header("accept", "application/problem+json")
        .send()
        .await
}

#[tokio::test]
async fn a_member_of_either_role_reads_the_owners_blobs() {
    let fixture = Fixture::working();
    let bytes = ciphertext();
    let address = published_original(&fixture, "shared", &bytes).await;
    let bob = fixture.other_bearer(BOB).await;

    for role in [MemberRole::Reader, MemberRole::Writer] {
        roster(
            &fixture,
            u64::from(role == MemberRole::Writer) + 1,
            &[(BOB, role)],
        )
        .await;
        let response = fetch(&fixture, &bob, address.as_str()).await;
        response.assert_status(StatusCode::OK);
        assert_eq!(
            response.bytes().as_ref(),
            bytes.as_slice(),
            "{role:?} reads the bytes"
        );
        fixture
            .client
            .get(&format!("/v1/blob/{address}"))
            .header("authorization", &bob)
            .header("range", "bytes=0-1023")
            .send()
            .await
            .assert_status(StatusCode::PARTIAL_CONTENT);
    }
}

#[tokio::test]
async fn a_former_member_is_told_access_was_revoked() {
    // The `403` the download contract describes, rendered at last: an authorization change, not
    // a durability loss, so the client re-syncs its membership before it degrades.
    let fixture = Fixture::working();
    let address = published_original(&fixture, "unshared", &ciphertext()).await;
    let bob = fixture.other_bearer(BOB).await;
    roster(&fixture, 1, &[(BOB, MemberRole::Writer)]).await;
    fetch(&fixture, &bob, address.as_str())
        .await
        .assert_status(StatusCode::OK);

    roster(&fixture, 2, &[]).await;
    let refused = fetch(&fixture, &bob, address.as_str()).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let problem: serde_json::Value = refused.json();
    assert_eq!(problem["code"], "error.blob.access_revoked");

    // Re-admitted: the bytes again.
    roster(&fixture, 3, &[(BOB, MemberRole::Reader)]).await;
    fetch(&fixture, &bob, address.as_str())
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_former_member_gets_the_403_before_any_policy_refusal() {
    // Authority first, as `S-C39` fixed it: a former member learns nothing about takedowns or
    // deletions either. The `403` is theirs whatever the asset's state.
    let fixture = Fixture::working();
    let address = published_original(&fixture, "held-from-former", &ciphertext()).await;
    let bob = fixture.other_bearer(BOB).await;
    roster(&fixture, 1, &[(BOB, MemberRole::Reader)]).await;
    roster(&fixture, 2, &[]).await;
    assert_eq!(
        fixture
            .index
            .set_hold(
                &AssetId::new("held-from-former"),
                Some(ServingHold::Takedown)
            )
            .await
            .expect("the index holds"),
        HoldOutcome::Applied
    );

    fetch(&fixture, &bob, address.as_str())
        .await
        .assert_status(StatusCode::FORBIDDEN);
    // The owner is told the truth, as before.
    fetch(&fixture, &bearer(&fixture).await, address.as_str())
        .await
        .assert_status(StatusCode::GONE);
}

#[tokio::test]
async fn a_never_member_is_indistinguishable_from_an_unknown_address_body_and_headers() {
    // The full disclosure property with a roster in play: an account the roster never named —
    // even while *other* accounts are on it — gets the unknown-address answer byte for byte,
    // headers included (the `date` header aside, which is the clock's).
    let fixture = Fixture::working();
    let address = published_original(&fixture, "never-shared", &ciphertext()).await;
    roster(&fixture, 1, &[(BOB, MemberRole::Writer)]).await;
    let carol = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000c0")
        .await;

    let refused = fetch(&fixture, &carol, address.as_str()).await;
    refused.assert_status(StatusCode::NOT_FOUND);
    let unknown = fetch(&fixture, &carol, &support::checksum(b"never existed")).await;
    unknown.assert_status(StatusCode::NOT_FOUND);

    assert_eq!(refused.bytes(), unknown.bytes());
    // Presence first, so the equalities below cannot pass on two absent headers. (The in-process
    // client does not materialise `content-length`, so the media type is the one header a problem
    // body is guaranteed to carry here.)
    assert!(
        !refused.headers("content-type").is_empty(),
        "a problem response carries `content-type`"
    );
    // Every header a problem response carries, `date` aside (which is the clock's). The test
    // client exposes headers by name, so the set is spelled out; a new response header joins it.
    for name in [
        "content-type",
        "content-length",
        "cache-control",
        "vary",
        "www-authenticate",
        "x-capsule-protocol-min",
        "x-capsule-protocol-max",
    ] {
        assert_eq!(
            refused.headers(name),
            unknown.headers(name),
            "the `{name}` header differs between a never-member's refusal and an unknown address"
        );
    }
}

#[tokio::test]
async fn a_membership_store_that_cannot_answer_is_an_outage_never_a_refusal() {
    // An outage must not look like a revocation — the client actions are opposite — nor like
    // an unknown address.
    let fixture = Fixture::working();
    let address = published_original(&fixture, "outage", &ciphertext()).await;
    let bob = fixture.other_bearer(BOB).await;
    roster(&fixture, 1, &[(BOB, MemberRole::Reader)]).await;

    fixture.members.set_unavailable(true);
    let failed = fetch(&fixture, &bob, address.as_str()).await;
    failed.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    let problem: serde_json::Value = failed.json();
    assert_eq!(problem["code"], "error.blob.unavailable");
    fixture.members.set_unavailable(false);

    // The owner never asks the roster, so the outage does not touch them.
    fetch(&fixture, &bearer(&fixture).await, address.as_str())
        .await
        .assert_status(StatusCode::OK);
}

/// A published asset holding `bytes` as a backup, and the address they live at.
async fn published_backup(fixture: &Fixture, asset: &str, bytes: &[u8]) -> ContentAddress {
    let id = publish(fixture, asset).await;
    let address = store(fixture, bytes).await;
    reference(fixture, &id, BlobRole::Backup, &address, bytes.len() as u64).await;
    address
}

#[tokio::test]
async fn only_the_owner_fetches_a_backup() {
    // A backup is the owner's own durability artefact, not part of what the album shares
    // (#525). A member of either role is refused it with the unknown-address `404`, not a `403`:
    // the feed never names a backup, so the refusal must not confirm the address is live.
    let fixture = Fixture::working();
    let bytes = ciphertext();
    let address = published_backup(&fixture, "backed-up", &bytes).await;
    let bob = fixture.other_bearer(BOB).await;

    let owned = fetch(&fixture, &bearer(&fixture).await, address.as_str()).await;
    owned.assert_status(StatusCode::OK);
    assert_eq!(owned.bytes().as_ref(), bytes.as_slice());

    let unknown = fetch(&fixture, &bob, &support::checksum(b"never existed")).await;
    unknown.assert_status(StatusCode::NOT_FOUND);
    for (version, role) in [(1, MemberRole::Reader), (2, MemberRole::Writer)] {
        roster(&fixture, version, &[(BOB, role)]).await;
        let refused = fetch(&fixture, &bob, address.as_str()).await;
        refused.assert_status(StatusCode::NOT_FOUND);
        assert_eq!(
            refused.bytes(),
            unknown.bytes(),
            "a {role:?}'s backup refusal is byte-identical to an unknown address"
        );
    }

    // A former member is not owed the `403` for it either: they never had it.
    roster(&fixture, 3, &[]).await;
    fetch(&fixture, &bob, address.as_str())
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_non_owners_backup_refusal_does_not_ask_the_roster() {
    // Decided on the role alone, so a roster outage cannot turn it into a `500` — which would
    // itself say the address is referenced by an album the caller might be on.
    let fixture = Fixture::working();
    let address = published_backup(&fixture, "backed-up-outage", &ciphertext()).await;
    let bob = fixture.other_bearer(BOB).await;
    roster(&fixture, 1, &[(BOB, MemberRole::Reader)]).await;

    fixture.members.set_unavailable(true);
    fetch(&fixture, &bob, address.as_str())
        .await
        .assert_status(StatusCode::NOT_FOUND);
    fetch(&fixture, &bearer(&fixture).await, address.as_str())
        .await
        .assert_status(StatusCode::OK);
    fixture.members.set_unavailable(false);
}

// ===========================================================================================
// Bytes shared across unrelated owners (#462)
// ===========================================================================================

/// An unrelated account whose album holds the same ciphertext as the seeded one.
const MALLORY: &str = "01937b7c-0000-7000-8000-0000000000ad";
/// A current member of the seeded album who was once on Mallory's roster and was removed.
const CAROL: &str = "01937b7c-0000-7000-8000-0000000000c0";
/// A former member of the seeded album, never on Mallory's roster.
const DAVE: &str = "01937b7c-0000-7000-8000-0000000000d0";

/// Mallory's album.
fn mallorys_album() -> AlbumId {
    AlbumId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5ead")
}

/// The same `bytes` held by Mallory's asset and by the seeded owner's, with Mallory's asset id
/// sorting **first** — the order that used to decide every caller against Mallory's album.
///
/// Returns the address and the seeded owner's asset.
async fn shared_with_mallory(fixture: &Fixture, bytes: &[u8]) -> (ContentAddress, AssetId) {
    let mallorys = publish_in(
        fixture,
        "00-mallorys-copy",
        &OwnerId::new(MALLORY),
        &mallorys_album(),
    )
    .await;
    let address = store(fixture, bytes).await;
    reference(
        fixture,
        &mallorys,
        BlobRole::Original,
        &address,
        bytes.len() as u64,
    )
    .await;
    let ours = publish(fixture, "zz-the-albums-copy").await;
    reference(
        fixture,
        &ours,
        BlobRole::Original,
        &address,
        bytes.len() as u64,
    )
    .await;
    let first = fixture
        .index
        .find_reference(&address)
        .await
        .expect("the index answers")
        .expect("the address is referenced");
    assert_eq!(
        first.asset_id, mallorys,
        "the case needs Mallory's holder to sort first, or it proves nothing"
    );
    (address, ours)
}

#[tokio::test]
async fn shared_bytes_are_decided_through_the_holder_the_caller_is_entitled_through() {
    let fixture = Fixture::working();
    let bytes = ciphertext();
    let (address, _) = shared_with_mallory(&fixture, &bytes).await;
    roster(
        &fixture,
        1,
        &[
            (BOB, MemberRole::Reader),
            (CAROL, MemberRole::Reader),
            (DAVE, MemberRole::Reader),
        ],
    )
    .await;
    roster(
        &fixture,
        2,
        &[(BOB, MemberRole::Reader), (CAROL, MemberRole::Reader)],
    )
    .await;
    roster_of(
        &fixture,
        &mallorys_album(),
        1,
        &[(CAROL, MemberRole::Reader)],
    )
    .await;
    roster_of(&fixture, &mallorys_album(), 2, &[]).await;

    // The owner reads their own bytes, whoever else holds a copy.
    let response = fetch(&fixture, &bearer(&fixture).await, address.as_str()).await;
    response.assert_status(StatusCode::OK);
    assert_eq!(response.bytes().as_ref(), bytes.as_slice());

    // A current member reads them, though Mallory's album has never heard of him.
    fetch(&fixture, &fixture.other_bearer(BOB).await, address.as_str())
        .await
        .assert_status(StatusCode::OK);

    // A current member who was once removed from *Mallory's* album is entitled here, and must
    // not be told her access was revoked.
    fetch(
        &fixture,
        &fixture.other_bearer(CAROL).await,
        address.as_str(),
    )
    .await
    .assert_status(StatusCode::OK);

    // A former member of the album is still told so, Mallory's unrelated holder notwithstanding.
    let refused = fetch(
        &fixture,
        &fixture.other_bearer(DAVE).await,
        address.as_str(),
    )
    .await;
    refused.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(
        refused.json::<serde_json::Value>()["code"],
        "error.blob.access_revoked"
    );

    // And a stranger to both holders is told nothing.
    fetch(
        &fixture,
        &fixture
            .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
            .await,
        address.as_str(),
    )
    .await
    .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn an_owners_deleted_copy_is_gone_while_a_strangers_copy_lives() {
    // Before #462 the serving path saw only the live holder — Mallory's — and told the owner
    // `404`, the "never heard of it" answer, about an asset they had deleted. The owner's own
    // holder is what decides their answer, and a deleted asset's blob is `410`.
    let fixture = Fixture::working();
    let (address, ours) = shared_with_mallory(&fixture, &ciphertext()).await;
    fixture
        .index
        .tombstone(&ours, Timestamp::UNIX_EPOCH)
        .await
        .expect("the index tombstones")
        .expect("the asset exists");

    fetch(&fixture, &bearer(&fixture).await, address.as_str())
        .await
        .assert_status(StatusCode::GONE);
    // Mallory still reads her live copy.
    fetch(
        &fixture,
        &fixture.other_bearer(MALLORY).await,
        address.as_str(),
    )
    .await
    .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn an_owner_whose_copy_sorts_second_is_served_through_a_roster_outage() {
    // Mallory's holder sorts first, and asking about it needs the roster. The owner's own
    // holder never does, so the outage on the first holder must not turn into a `500` for a
    // caller the second holder grants.
    let fixture = Fixture::working();
    let bytes = ciphertext();
    let (address, _) = shared_with_mallory(&fixture, &bytes).await;

    fixture.members.set_unavailable(true);
    let response = fetch(&fixture, &bearer(&fixture).await, address.as_str()).await;
    response.assert_status(StatusCode::OK);
    assert_eq!(response.bytes().as_ref(), bytes.as_slice());
}

#[tokio::test]
async fn a_roster_outage_on_a_later_holder_is_an_outage_not_a_refusal() {
    // Mallory's own copy sorts first and is taken down, so it grants her and cannot serve. The
    // album's copy is the second holder, and deciding it needs the roster, which is down. That
    // holder might have served her, so the answer is the outage — never the permanent `410` the
    // held copy alone would give, and never a `404`.
    let fixture = Fixture::working();
    let (address, _) = shared_with_mallory(&fixture, &ciphertext()).await;
    fixture
        .index
        .set_hold(
            &AssetId::new("00-mallorys-copy"),
            Some(ServingHold::Takedown),
        )
        .await
        .expect("the hold is placed");
    let mallory = fixture.other_bearer(MALLORY).await;
    let bob = fixture.other_bearer(BOB).await;

    fixture.members.set_unavailable(true);
    for (who, caller) in [("mallory", &mallory), ("a non-owner", &bob)] {
        let failed = fetch(&fixture, caller, address.as_str()).await;
        failed.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            failed.json::<serde_json::Value>()["code"],
            "error.blob.unavailable",
            "{who} is told the fetch could not be decided"
        );
    }

    // With the roster back, Mallory is not on the album's roster, so only her held copy grants.
    fixture.members.set_unavailable(false);
    fetch(&fixture, &mallory, address.as_str())
        .await
        .assert_status(StatusCode::GONE);
}

#[tokio::test]
async fn a_held_holder_does_not_hide_an_unheld_one_the_caller_is_entitled_through() {
    // A takedown is a fact about one asset. Bob reads both albums; Mallory's copy sorts first
    // and is held, the album's copy is not, and every other reader of the album's copy is
    // served — so Bob is too.
    let fixture = Fixture::working();
    let bytes = ciphertext();
    let (address, _) = shared_with_mallory(&fixture, &bytes).await;
    roster(&fixture, 1, &[(BOB, MemberRole::Reader)]).await;
    roster_of(&fixture, &mallorys_album(), 1, &[(BOB, MemberRole::Reader)]).await;
    fixture
        .index
        .set_hold(
            &AssetId::new("00-mallorys-copy"),
            Some(ServingHold::Takedown),
        )
        .await
        .expect("the hold is placed");

    let response = fetch(&fixture, &fixture.other_bearer(BOB).await, address.as_str()).await;
    response.assert_status(StatusCode::OK);
    assert_eq!(response.bytes().as_ref(), bytes.as_slice());

    // Mallory is entitled only through her held copy, so she is told it is gone.
    fetch(
        &fixture,
        &fixture.other_bearer(MALLORY).await,
        address.as_str(),
    )
    .await
    .assert_status(StatusCode::GONE);
}

/// Bob's own device, in Bob's own directory: an upload is checked against the *uploader's*
/// directory, whoever the album belongs to.
fn bobs_device() -> uuid::Uuid {
    uuid::Uuid::parse_str("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5eb0").expect("a uuid")
}

#[tokio::test]
async fn a_writer_members_upload_is_pending_to_the_member_and_not_to_the_owner() {
    // A member's session is filed under the album owner. Keyed on that owner, the transient
    // `409` went to the owner — about somebody else's transfer — and the member's own second
    // device got `404`. It belongs to the uploader (#462).
    let fixture = Fixture::working();
    let bob = UserId::new(BOB);
    fixture.authority.add_device(
        &bob,
        bobs_device(),
        capsule_server::store::Clock::now(&*fixture.clock),
    );
    fixture.authority.share(&album(), &bob, MemberRole::Writer);
    let bobs_bearer = fixture.other_bearer(BOB).await;

    let bytes = payload(b'w', 8192);
    let address = support::checksum(&bytes);
    let mut request = support::create_request(&fixture.clock, &bytes, "original");
    request["manifest_envelope"]["created_by_user"] = BOB.into();
    request["manifest_envelope"]["created_by_device"] = bobs_device().to_string().into();
    fixture.open_session_with(&request, &bobs_bearer).await;

    let pending = fetch(&fixture, &bobs_bearer, &address).await;
    pending.assert_status(StatusCode::CONFLICT);
    assert_eq!(
        pending.json::<serde_json::Value>()["code"],
        "error.blob.pending_upload"
    );
    fetch(&fixture, &bearer(&fixture).await, &address)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}
