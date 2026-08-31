//! `replace` rides the upload protocol (slice `S-C43`), end to end.
//!
//! The action the authorization doc always listed beside `create` — *"a write that moves blob
//! bytes is an upload by definition"* — and which neither surface served: the upload gate
//! accepted `create` and nothing else, and the lifecycle route refuses everything that moves
//! bytes. So `replace` fell between them.
//!
//! What makes it more than a widened allow-list is that a replace mutates an asset that is
//! **already visible**. A `create` assembles its bundle incrementally in a `Pending` row nobody
//! can see; a replace cannot, because a window in which the new original is referenced by the
//! old manifest is a window in which `verify_asset` fails for every client that fetches the
//! asset. So it is applied as one act, at the moment its manifest lands — which makes the
//! manifest the member that lands **last**, and makes it the member that has to be able to name
//! what the asset will hold.

mod support;

use capsule_core::crypto::hash::hash_bytes;
use capsule_server::index::{AssetIndex, AssetState};
use capsule_server::store::{AssetId, BlobRole};
use kynos::http::StatusCode;
use support::{Fixture, PROTOCOL_VERSION, checksum, create_request, payload, replace_request};

/// The asset every case in this file works on — the id `create_request` fixes.
fn asset() -> AssetId {
    AssetId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e61")
}

/// Upload `bytes` under `role` through the real protocol, in one chunk.
async fn upload(fixture: &Fixture, body: &serde_json::Value, bytes: &[u8], bearer: &str) {
    let id = fixture.open_session_with(body, bearer).await;
    fixture
        .chunk(&id, 0, bytes, bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
}

/// Publish the asset the ordinary way, and return `(original bytes, chain head hex)`.
async fn publish(fixture: &Fixture, bearer: &str) -> (Vec<u8>, String) {
    let original = payload(b'o', 8192);
    let metadata = payload(b'm', 4096);
    let manifest = payload(b'p', 2048);

    for (bytes, role) in [
        (&original, "original"),
        (&metadata, "metadata"),
        (&manifest, "provenance"),
    ] {
        let body = create_request(&fixture.clock, bytes, role);
        upload(fixture, &body, bytes, bearer).await;
    }

    let row = fixture
        .index
        .read(&asset())
        .await
        .expect("the index answers")
        .expect("the asset exists");
    assert_eq!(row.state, AssetState::Visible, "the bundle published");
    (original, checksum(&manifest))
}

/// The address the asset holds under `role`.
async fn held(fixture: &Fixture, role: BlobRole) -> String {
    fixture
        .index
        .read(&asset())
        .await
        .expect("the index answers")
        .expect("the asset exists")
        .address_for(role)
        .expect("the role is held")
        .as_str()
        .to_owned()
}

// ===========================================================================================

#[tokio::test]
async fn a_replace_bundle_supersedes_the_original_and_reaches_the_feed() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (first_original, head) = publish(&fixture, &bearer).await;
    let published_seq = fixture
        .index
        .read(&asset())
        .await
        .expect("the index answers")
        .expect("the asset exists")
        .sync_seq
        .expect("publication minted a sequence number");

    // A device that has already seen the asset, so the replace is an update *to it*.
    let cursor = caught_up(&fixture, &bearer).await;

    let original = payload(b'O', 8192);
    let metadata = payload(b'M', 4096);
    let manifest = payload(b'P', 2048);
    assert_ne!(original, first_original);

    // Bytes first. Each of these commits its blob and applies nothing — the index still points
    // at the old bundle after both of them.
    for (bytes, role) in [(&original, "original"), (&metadata, "metadata")] {
        let body = replace_request(&fixture.clock, bytes, role, &head, None);
        upload(&fixture, &body, bytes, &bearer).await;
    }
    assert_eq!(
        held(&fixture, BlobRole::Original).await,
        checksum(&first_original),
        "a replace's bytes landing must not move the asset; only its manifest does"
    );

    // The manifest, which is the act.
    let body = replace_request(
        &fixture.clock,
        &manifest,
        "provenance",
        &head,
        Some((&checksum(&original), &checksum(&metadata))),
    );
    upload(&fixture, &body, &manifest, &bearer).await;

    assert_eq!(
        held(&fixture, BlobRole::Original).await,
        checksum(&original)
    );
    assert_eq!(
        held(&fixture, BlobRole::Metadata).await,
        checksum(&metadata)
    );
    assert_eq!(
        held(&fixture, BlobRole::Provenance).await,
        checksum(&manifest),
        "the feed serves the newest manifest, so the provenance reference moves with the chain"
    );

    let row = fixture
        .index
        .read(&asset())
        .await
        .expect("the index answers")
        .expect("the asset exists");
    assert_eq!(
        row.chain_head,
        Some(hash_bytes(&manifest)),
        "the new manifest is the chain the next lifecycle write must name"
    );
    assert!(
        row.sync_seq > Some(published_seq),
        "a replace is a change every device has to hear about, so it takes a new position"
    );

    // And it reaches a device that had already seen the asset as an **update**, not as a new
    // asset. The distinction is a fact about the reader rather than about the row — a device
    // syncing for the first time sees a `created` — so this is asserted from a cursor taken
    // before the replace, which is the only place the difference is observable.
    let entry = entry_after(&fixture, &bearer, cursor.as_deref()).await;
    assert_eq!(entry["change"], "updated");
}

/// The replaced asset's entry on a feed page read from `cursor`.
async fn entry_after(fixture: &Fixture, bearer: &str, cursor: Option<&str>) -> serde_json::Value {
    let path = cursor.map_or_else(
        || "/v1/sync".to_owned(),
        |cursor| format!("/v1/sync?cursor={cursor}"),
    );
    let feed: serde_json::Value = fixture
        .client
        .get(&path)
        .header("authorization", bearer)
        .header("accept", "application/json")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    feed["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["asset_id"] == serde_json::json!(asset().as_str()))
        .cloned()
        .expect("the replaced asset is on the feed")
}

/// The cursor a device holds after syncing everything there is.
async fn caught_up(fixture: &Fixture, bearer: &str) -> Option<String> {
    let feed: serde_json::Value = fixture
        .client
        .get("/v1/sync")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    feed["next_cursor"].as_str().map(ToOwned::to_owned)
}

#[tokio::test]
async fn a_manifest_that_arrives_before_its_bundle_is_refused_and_retryable() {
    // The one ordering rule this protocol has, and the reason it is a `409` rather than a `400`:
    // the request is not wrong, it is early. A client that retries the manifest after the rest
    // of the bundle commits succeeds, which is what makes this different from every other
    // refusal on this surface.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (_, head) = publish(&fixture, &bearer).await;

    let original = payload(b'O', 8192);
    let metadata = payload(b'M', 4096);
    let manifest = payload(b'P', 2048);
    let bundle = Some((checksum(&original), checksum(&metadata)));
    let (original_hex, metadata_hex) = bundle.clone().expect("a bundle");

    let body = replace_request(
        &fixture.clock,
        &manifest,
        "provenance",
        &head,
        Some((&original_hex, &metadata_hex)),
    );
    let early = fixture.open_session_with(&body, &bearer).await;
    let refused = fixture.chunk(&early, 0, &manifest, &bearer).send().await;
    refused.assert_status(StatusCode::CONFLICT);
    let problem: serde_json::Value = refused.json();
    assert_eq!(problem["code"], "error.upload.replace_incomplete");
    assert_eq!(
        held(&fixture, BlobRole::Provenance).await,
        checksum(&payload(b'p', 2048)),
        "a refused replace changes nothing at all"
    );

    // The bytes, then the same manifest again.
    for (bytes, role) in [(&original, "original"), (&metadata, "metadata")] {
        let body = replace_request(&fixture.clock, bytes, role, &head, None);
        upload(&fixture, &body, bytes, &bearer).await;
    }
    let body = replace_request(
        &fixture.clock,
        &manifest,
        "provenance",
        &head,
        Some((&original_hex, &metadata_hex)),
    );
    upload(&fixture, &body, &manifest, &bearer).await;
    assert_eq!(held(&fixture, BlobRole::Original).await, original_hex);
}

#[tokio::test]
async fn a_replace_that_does_not_chain_onto_the_head_is_refused() {
    // Invariant 17, decided where the comparison and the write are one operation. A gate-side
    // check would let two concurrent replaces both pass and double-apply, which is the stale
    // revival the invariant exists to catch, reintroduced by the code enforcing it.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (_, head) = publish(&fixture, &bearer).await;

    let original = payload(b'O', 8192);
    let metadata = payload(b'M', 4096);
    let manifest = payload(b'P', 2048);
    for (bytes, role) in [(&original, "original"), (&metadata, "metadata")] {
        let body = replace_request(&fixture.clock, bytes, role, &head, None);
        upload(&fixture, &body, bytes, &bearer).await;
    }

    let stale = checksum(b"a manifest this asset never had");
    let body = replace_request(
        &fixture.clock,
        &manifest,
        "provenance",
        &stale,
        Some((&checksum(&original), &checksum(&metadata))),
    );
    let id = fixture.open_session_with(&body, &bearer).await;
    let refused = fixture.chunk(&id, 0, &manifest, &bearer).send().await;
    refused.assert_status(StatusCode::CONFLICT);
    let problem: serde_json::Value = refused.json();
    assert_eq!(problem["code"], "error.upload.stale_revival");
    assert_eq!(
        held(&fixture, BlobRole::Original).await,
        checksum(&payload(b'o', 8192)),
        "a refused replace leaves the asset exactly as it was"
    );

    let _ = head;
}
