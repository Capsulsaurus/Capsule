//! `GET /v1/sync` — the key-free sync feed (slice `S-C2`), end to end.
//!
//! Every case drives the built service in-process. What is asserted against the *index* is
//! asserted against the store the server actually read, never against a second reading of the
//! response body — a response can say anything.

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_server::blob::{BlobStore, ContentAddress};
use capsule_server::index::{AssetIndex, BlobRecord, PendingAsset};
use capsule_server::store::{AssetId, BlobRole};
use capsule_server::sync::{CURSOR_KEY_LEN, CursorCodec};
use jiff::Timestamp;
use kynos::http::StatusCode;
use serde_json::Value;
use support::{CURSOR_KEY, Fixture, PROTOCOL_VERSION, album, owner};

/// The bytes a manifest for `asset` is made of.
///
/// Distinct per asset so "the feed served *this* asset's manifest" is checkable, and not valid
/// CBOR on purpose: this surface is contractually forbidden from parsing it, so a test that fed
/// it something parseable would be testing less.
fn manifest_bytes(asset: &str) -> Vec<u8> {
    format!("signed-manifest-for-{asset}").into_bytes()
}

/// Put `bytes` in the blob store at their own address and return it.
async fn store_blob(fixture: &Fixture, bytes: &[u8]) -> ContentAddress {
    let address = ContentAddress::parse(&support::checksum(bytes)).expect("a content address");
    fixture
        .blobs
        .put(&address, bytes)
        .await
        .expect("the in-memory store accepts");
    address
}

/// Publish `asset` into the fixture's index with a real provenance blob behind it.
///
/// Returns the asset id and the sequence number publication minted.
async fn publish(fixture: &Fixture, asset: &str) -> (AssetId, u64) {
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

    let provenance = store_blob(fixture, &manifest_bytes(asset)).await;
    record(fixture, &id, BlobRole::Provenance, &provenance, 0).await;

    let metadata = store_blob(fixture, format!("metadata-{asset}").as_bytes()).await;
    let seq = record(fixture, &id, BlobRole::Metadata, &metadata, 0)
        .await
        .expect("landing the index tier publishes the asset");
    (id, seq)
}

/// Record one finalized blob, returning the sequence number it minted.
async fn record(
    fixture: &Fixture,
    asset: &AssetId,
    role: BlobRole,
    address: &ContentAddress,
    size: u64,
) -> Option<u64> {
    match fixture
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
        .expect("the index records")
    {
        capsule_server::index::BlobOutcome::Recorded { minted, .. } => minted,
        other => panic!("recording a {role:?} blob answered {other:?}"),
    }
}

/// Ask for a page, asserting the status.
async fn page(fixture: &Fixture, bearer: &str, query: &str, expect: StatusCode) -> Value {
    let path = if query.is_empty() {
        "/v1/sync".to_owned()
    } else {
        format!("/v1/sync?{query}")
    };
    fixture
        .client
        .get(&path)
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(expect)
        .json()
}

/// The bearer for the fixture's seeded account.
async fn bearer(fixture: &Fixture) -> String {
    format!("Bearer {}", fixture.login().await.access_token)
}

// ===========================================================================================

/// A client with no cursor gets everything, in order, with a cursor that resumes.
#[tokio::test]
async fn a_first_sync_returns_the_whole_library_in_sequence_order() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let mut published = Vec::new();
    for n in 0..3 {
        published.push(publish(&fixture, &format!("asset-{n}")).await);
    }

    let body = page(&fixture, &bearer, "", StatusCode::OK).await;
    let entries = body["entries"].as_array().expect("a page of entries");
    assert_eq!(entries.len(), 3);
    for (entry, (asset, seq)) in entries.iter().zip(&published) {
        assert_eq!(entry["asset_id"], asset.as_str());
        assert_eq!(entry["sync_seq"], *seq);
        assert_eq!(
            entry["change"], "created",
            "a client that has never looked has never seen any of these"
        );
        assert_eq!(entry["album_id"], album().as_str());
        assert_eq!(entry["protocol_version"], PROTOCOL_VERSION);
    }
    assert_eq!(
        body["has_more"], false,
        "the whole library fit in one page and the client should not poll again"
    );

    // The cursor resumes exactly where the page ended.
    let cursor = body["next_cursor"].as_str().expect("every page has one");
    let resumed = page(
        &fixture,
        &bearer,
        &format!("cursor={cursor}"),
        StatusCode::OK,
    )
    .await;
    assert!(
        resumed["entries"].as_array().expect("an array").is_empty(),
        "resuming from the end of a page returned entries the client already has"
    );
}

/// `S-C30`: the manifest a client verifies is the manifest a client uploaded.
///
/// The defect this replaces was not "the bytes were slightly different" — the retired feed
/// re-serialized the server's envelope *projection*, which carries neither `device_sig` nor
/// `write_sig`, so `verify_asset` had nothing to check. Byte equality is the only assertion
/// that distinguishes "served the blob" from "rebuilt something similar".
#[tokio::test]
async fn the_feed_serves_the_uploaded_manifest_byte_for_byte() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    publish(&fixture, "verbatim").await;

    let body = page(&fixture, &bearer, "", StatusCode::OK).await;
    let served = body["entries"][0]["manifest_cbor"]
        .as_str()
        .expect("a published asset carries its manifest");
    let decoded = BASE64.decode(served).expect("the feed emits base64");

    assert_eq!(
        decoded,
        manifest_bytes("verbatim"),
        "the feed did not serve the bytes the provenance blob holds"
    );
}

/// An asset whose original has not landed is `awaiting-original`, and the flip reaches the feed.
#[tokio::test]
async fn original_held_flips_when_the_original_lands() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, published) = publish(&fixture, "staged").await;

    let body = page(&fixture, &bearer, "", StatusCode::OK).await;
    assert_eq!(
        body["entries"][0]["original_held"], false,
        "an asset published from its index tier alone is awaiting-original"
    );
    assert!(
        body["entries"][0]["blobs"]
            .as_array()
            .expect("an array")
            .is_empty(),
        "no original and no derivative have landed, so the blob list is empty"
    );

    let original = store_blob(&fixture, b"ciphertext-original").await;
    record(&fixture, &asset, BlobRole::Original, &original, 4096).await;

    let after = page(
        &fixture,
        &bearer,
        &format!("cursor={}", body["next_cursor"].as_str().expect("a cursor")),
        StatusCode::OK,
    )
    .await;
    let entry = &after["entries"][0];
    assert_eq!(entry["original_held"], true);
    assert_eq!(
        entry["change"], "updated",
        "the client had already seen this asset created"
    );
    assert!(
        entry["sync_seq"].as_u64().expect("a number") > published,
        "the flip must advance the sequence or no client learns about it"
    );
    assert_eq!(entry["blobs"][0]["role"], "original");
    assert_eq!(entry["blobs"][0]["hash"], original.as_str());
    assert_eq!(entry["blobs"][0]["size"], 4096);
}

/// A tombstone tells a client the asset is gone and nothing else.
#[tokio::test]
async fn a_tombstone_carries_no_byte_references() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let (asset, _) = publish(&fixture, "doomed").await;
    fixture
        .index
        .tombstone(&asset, Timestamp::UNIX_EPOCH)
        .await
        .expect("the index tombstones");

    let body = page(&fixture, &bearer, "", StatusCode::OK).await;
    let entry = &body["entries"][0];
    assert_eq!(entry["change"], "deleted");
    assert!(
        entry["manifest_cbor"].is_null(),
        "a tombstone must not hand back a manifest for an asset that is gone"
    );
    assert!(entry["metadata_blob"].is_null());
    assert!(
        entry["blobs"].as_array().expect("an array").is_empty(),
        "a tombstone's blob list invites a client to fetch bytes GC may already have collected"
    );
    assert_eq!(entry["original_held"], false);
}

/// Paging sees every entry exactly once, at any page size.
#[tokio::test]
async fn paging_resumes_without_gaps_or_repeats() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    let mut expected = Vec::new();
    for n in 0..5 {
        expected.push(publish(&fixture, &format!("paged-{n}")).await.0);
    }

    let mut seen = Vec::new();
    let mut cursor = String::new();
    loop {
        let query = if cursor.is_empty() {
            "page_size=2".to_owned()
        } else {
            format!("cursor={cursor}&page_size=2")
        };
        let body = page(&fixture, &bearer, &query, StatusCode::OK).await;
        let entries = body["entries"].as_array().expect("an array").clone();
        cursor = body["next_cursor"].as_str().expect("a cursor").to_owned();
        if entries.is_empty() {
            assert_eq!(
                body["has_more"], false,
                "an empty page must not tell the client to keep asking"
            );
            break;
        }
        assert!(entries.len() <= 2, "the page size was not honoured");
        for entry in entries {
            seen.push(entry["asset_id"].as_str().expect("an asset id").to_owned());
        }
    }

    let expected: Vec<String> = expected.iter().map(|id| id.to_string()).collect();
    assert_eq!(seen, expected);
}

/// The same cursor twice is the same page: a cursor names a position, it does not consume one.
#[tokio::test]
async fn the_feed_is_idempotent_for_one_cursor() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    publish(&fixture, "stable").await;

    let first = page(&fixture, &bearer, "", StatusCode::OK).await;
    let second = page(&fixture, &bearer, "", StatusCode::OK).await;
    assert_eq!(
        first, second,
        "a retried sync returned something different, so a lost response is not harmless"
    );
}

/// A page size past the ceiling is clamped, not refused.
#[tokio::test]
async fn an_outsized_page_request_is_clamped() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    publish(&fixture, "clamped").await;

    for query in ["page_size=0", "page_size=4294967295"] {
        let body = page(&fixture, &bearer, query, StatusCode::OK).await;
        assert_eq!(
            body["entries"].as_array().expect("an array").len(),
            1,
            "{query} should have been clamped into range, not refused"
        );
    }
}

/// The feed is scoped to the caller's own library.
#[tokio::test]
async fn the_feed_shows_only_the_callers_own_library() {
    let fixture = Fixture::working();
    publish(&fixture, "mine").await;

    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;
    let body = page(&fixture, &stranger, "", StatusCode::OK).await;
    assert!(
        body["entries"].as_array().expect("an array").is_empty(),
        "another account's feed served this library's assets"
    );
    assert_eq!(body["has_more"], false);
}

/// A forged or mutated cursor is refused with the code the client switches on.
#[tokio::test]
async fn a_forged_cursor_is_refused_with_its_code() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    publish(&fixture, "guarded").await;

    for cursor in [
        "not-a-cursor",
        "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ] {
        let body = page(
            &fixture,
            &bearer,
            &format!("cursor={cursor}"),
            StatusCode::BAD_REQUEST,
        )
        .await;
        assert_eq!(
            body["code"], "error.sync.cursor_invalid",
            "the client switches on the code, not on the status"
        );
    }
}

/// A validly-MAC'd cursor issued to another owner does not authenticate here.
///
/// The retired MAC covered only the position, so this cursor would have been accepted. It
/// matters now because `S-C37` mints sequence numbers per owner: position 500 is a different
/// point in every library, so a foreign cursor is a way to skip your own unseen entries.
#[tokio::test]
async fn another_owners_cursor_is_refused_even_under_the_right_key() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    publish(&fixture, "bound").await;

    let codec = CursorCodec::new(&CURSOR_KEY);
    let foreign = codec.encode(
        &capsule_server::store::OwnerId::new("01937b7c-0000-7000-8000-0000000000ff"),
        0,
    );

    let body = page(
        &fixture,
        &bearer,
        &format!("cursor={foreign}"),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(body["code"], "error.sync.cursor_invalid");
}

/// A cursor from a server holding a different key does not authenticate.
#[tokio::test]
async fn another_servers_cursor_is_refused() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    publish(&fixture, "keyed").await;

    let elsewhere = CursorCodec::new(&[0x11; CURSOR_KEY_LEN]).encode(&owner(), 0);
    let body = page(
        &fixture,
        &bearer,
        &format!("cursor={elsewhere}"),
        StatusCode::BAD_REQUEST,
    )
    .await;
    assert_eq!(body["code"], "error.sync.cursor_invalid");
}

/// An index that cannot answer is a coded `500`, not a silent empty page.
///
/// The distinction matters to a client: an empty page means "you are caught up" and advances
/// nothing, while a `500` means "ask again". The retired feed had no code here at all.
#[tokio::test]
async fn an_unreachable_index_is_a_coded_failure_not_an_empty_page() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;
    publish(&fixture, "unreachable").await;

    fixture.index.set_unavailable(true);
    let body = page(&fixture, &bearer, "", StatusCode::INTERNAL_SERVER_ERROR).await;
    assert_eq!(body["code"], "error.sync.unavailable");
    fixture.index.set_unavailable(false);

    let recovered = page(&fixture, &bearer, "", StatusCode::OK).await;
    assert_eq!(recovered["entries"].as_array().expect("an array").len(), 1);
}

/// A manifest the store cannot produce costs the entry its manifest, not the client its page.
///
/// Every way this fails is the *server's* inconsistency. Failing the whole page would make one
/// storage fault look like an outage, and would hide the assets the client can still act on.
#[tokio::test]
async fn a_missing_provenance_blob_still_yields_an_entry() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;

    let id = AssetId::new("dangling");
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
    // Recorded against the index but never put in the store: the shape a half-completed GC or
    // a restored database leaves behind.
    let absent = ContentAddress::parse(&support::checksum(b"never-stored")).expect("an address");
    record(&fixture, &id, BlobRole::Provenance, &absent, 0).await;
    let metadata = store_blob(&fixture, b"metadata-dangling").await;
    record(&fixture, &id, BlobRole::Metadata, &metadata, 0).await;

    let body = page(&fixture, &bearer, "", StatusCode::OK).await;
    let entry = &body["entries"][0];
    assert_eq!(entry["asset_id"], "dangling");
    assert!(
        entry["manifest_cbor"].is_null(),
        "there is no manifest to serve and the feed must not invent one"
    );
    assert_eq!(
        entry["metadata_blob"],
        metadata.as_str(),
        "the rest of the entry is still true and still useful"
    );
}

/// An unauthenticated request never reaches the feed.
#[tokio::test]
async fn the_feed_requires_a_credential() {
    let fixture = Fixture::working();
    publish(&fixture, "private").await;

    fixture
        .client
        .get("/v1/sync")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

/// An upload through the real surface becomes a feed entry — E2E case 3's server half.
///
/// Every other case in this file reaches the index directly, which proves the feed reads what
/// the index holds and nothing about whether an *upload* ever puts anything there. This is the
/// case that fails if the two modules are wired to different stores, or if creation mints a
/// fresh asset id per session instead of taking the manifest's.
#[tokio::test]
async fn an_upload_through_the_surface_becomes_a_feed_entry() {
    let fixture = Fixture::working();
    let bearer = bearer(&fixture).await;

    // The bundle's index tier, both blobs under the one asset the manifest names.
    let manifest = upload(&fixture, &bearer, "provenance", 0xA1).await;
    let metadata = upload(&fixture, &bearer, "metadata", 0xB2).await;

    let body = page(&fixture, &bearer, "", StatusCode::OK).await;
    let entries = body["entries"].as_array().expect("entries");
    assert_eq!(
        entries.len(),
        1,
        "two blobs of one bundle became {} assets",
        entries.len()
    );

    let entry = &entries[0];
    assert_eq!(
        entry["asset_id"], "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61",
        "the feed's asset id is the manifest's `file_id`, not a server-minted one"
    );
    assert_eq!(entry["change"], "created");
    assert_eq!(entry["album_id"], album().as_str());
    assert_eq!(
        entry["original_held"], false,
        "the original has not landed, and the feed says so rather than implying it"
    );
    assert_eq!(entry["metadata_blob"], metadata.as_str());
    assert!(
        entry["blobs"].as_array().expect("blob refs").is_empty(),
        "the index tier rides in its own fields; `blobs` is the original and derivatives, and \
         listing the manifest there twice is how a client double-fetches it"
    );
    assert_ne!(manifest.as_str(), metadata.as_str());

    let decoded = BASE64
        .decode(
            entry["manifest_cbor"]
                .as_str()
                .expect("the entry carries its manifest"),
        )
        .expect("the feed emits base64");
    assert_eq!(
        decoded,
        support::payload(0xA1, 8192),
        "the feed served something other than the provenance bytes the client uploaded"
    );

    // The original arrives afterwards and the flip is observable, which is what makes this a
    // round trip rather than a snapshot.
    let original = upload(&fixture, &bearer, "original", 0xC3).await;
    let body = page(&fixture, &bearer, "", StatusCode::OK).await;
    let entry = &body["entries"][0];
    assert_eq!(entry["original_held"], true);
    assert_eq!(entry["change"], "created", "still new to a reader at zero");
    assert!(
        entry["blobs"]
            .as_array()
            .expect("blob refs")
            .iter()
            .any(|blob| blob["hash"] == original.as_str()),
        "the original's reference did not reach the feed"
    );
}

/// Upload one blob of `role` end to end and return the address it committed to.
async fn upload(fixture: &Fixture, bearer: &str, role: &str, marker: u8) -> ContentAddress {
    let bytes = support::payload(marker, 8192);
    let id = fixture.open_session(&bytes, role, bearer).await;
    for offset in [0_u64, 4096] {
        let start = usize::try_from(offset).expect("a test offset fits");
        fixture
            .chunk(&id, offset, &bytes[start..start + 4096], bearer)
            .send()
            .await
            .assert_status(StatusCode::NO_CONTENT);
    }
    ContentAddress::parse(&support::checksum(&bytes)).expect("a content address")
}
