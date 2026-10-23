//! Guest drops, end to end.
//!
//! Three cases carry the acceptance. `a_guest_drop_lands_in_the_owners_inbox_and_never_in_an_album`
//! is invariant 30's absence clause: nothing a guest sends can put bytes into an album.
//! `a_links_caps_are_spent_once_per_drop_and_refunded_when_one_is_refused` is invariant 26 —
//! the caps are the whole authorization model on a path with no credential.
//! `an_adoption_that_is_refused_returns_the_drop_to_the_inbox` is invariant 32's honest half: a
//! failed promotion must lose nothing and duplicate nothing.

mod support;

use std::net::SocketAddr;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::hash_bytes;
use capsule_core::crypto::receipts::{
    BlobRole as ReceiptRole, CustodyReceipt, ReceiptExpectations, verify_receipt,
};
use capsule_server::blob::{BlobStore, ContentAddress};
use capsule_server::index::{AssetIndex, BlobRecord, PendingAsset, Reservation};
use capsule_server::store::{AssetId, BlobRole, OwnerId};
use jiff::{SignedDuration, Timestamp};
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{Fixture, PROTOCOL_VERSION, album, checksum, owner, payload, user};

/// A well-formed opaque id with letters in it.
fn opaque(tag: u8) -> String {
    format!(
        "{:032x}",
        u128::from(tag) | 0xabcd_ef01_2345_6789_abcd_ef01_2345_0000
    )
}

/// Provision a link, asserting the status.
async fn provision(fixture: &Fixture, bearer: &str, body: &Value, expect: StatusCode) -> Value {
    fixture
        .client
        .post("/v1/drops/links")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(body)
        .send()
        .await
        .assert_status(expect)
        .json()
}

/// A plain link with the given caps.
fn link_body(id: &str, caps: Value) -> Value {
    let mut body = json!({
        "opaque_id": id,
        "drop_pubkey": BASE64.encode([7_u8; 32]),
        "crypto_suite_id": capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
    });
    for (key, value) in caps.as_object().expect("caps is an object") {
        body[key] = value.clone();
    }
    body
}

/// Open a drop session through a link.
async fn create_drop(fixture: &Fixture, id: &str, bytes: &[u8], expect: StatusCode) -> Value {
    create_drop_with(fixture, id, bytes, None, expect).await
}

/// The same, presenting a passphrase proof.
async fn create_drop_with(
    fixture: &Fixture,
    id: &str,
    bytes: &[u8],
    proof: Option<&str>,
    expect: StatusCode,
) -> Value {
    let mut body = json!({
        "content_type": "image/jpeg",
        "size": bytes.len(),
        "ciphertext_hash": checksum(bytes),
        "kem_ct": BASE64.encode([9_u8; 64]),
        "suggested_filename": "from-a-guest.jpg",
    });
    if let Some(proof) = proof {
        body["passphrase_proof"] = json!(proof);
    }
    fixture
        .client
        .post(&format!("/d/{id}"))
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await
        .assert_status(expect)
        .json()
}

/// The Argon2id verifier a passphrase-gated link is provisioned with, and its matching proof.
///
/// The server compares bytes and never derives, so the suite does not run Argon2id: what is
/// under test is *possession of the derived value*, and a real KDF here would test `argon2`.
fn gate() -> (String, String) {
    let verifier = BASE64.encode([0xA5_u8; 32]);
    (verifier.clone(), verifier)
}

/// Send the whole blob as one chunk.
async fn send(fixture: &Fixture, id: &str, upload_id: &str, bytes: &[u8], expect: StatusCode) {
    fixture
        .client
        .patch(&format!("/d/{id}/{upload_id}"))
        .header("x-capsule-offset", "0")
        .header("x-capsule-checksum", &checksum(bytes))
        .body("application/octet-stream", bytes.to_vec())
        .send()
        .await
        .assert_status(expect);
}

/// The owner's inbox.
async fn inbox(fixture: &Fixture, bearer: &str) -> Value {
    fixture
        .client
        .get("/v1/drops")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json()
}

/// Provision a link and deposit one drop through it.
async fn deposit(fixture: &Fixture, bearer: &str, tag: u8, bytes: &[u8]) -> String {
    let id = opaque(tag);
    provision(
        fixture,
        bearer,
        &link_body(&id, json!({})),
        StatusCode::CREATED,
    )
    .await;
    let opened = create_drop(fixture, &id, bytes, StatusCode::CREATED).await;
    let upload_id = opened["upload_id"]
        .as_str()
        .expect("a session id")
        .to_owned();
    send(fixture, &id, &upload_id, bytes, StatusCode::NO_CONTENT).await;
    id
}

#[tokio::test]
async fn a_guest_drop_lands_in_the_owners_inbox_and_never_in_an_album() {
    // Invariant 30's absence clause. A guest has no account, no keys and no membership, so
    // nothing they send can put bytes into an album — adoption is the *owner's* signed create,
    // and until it happens the drop is only an inbox row.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'g', 4096);
    let id = deposit(&fixture, &bearer, 1, &bytes).await;

    let body = inbox(&fixture, &bearer).await;
    let drops = body["drops"].as_array().expect("drops");
    assert_eq!(drops.len(), 1);
    assert_eq!(drops[0]["ciphertext_hash"], checksum(&bytes));
    assert_eq!(drops[0]["size"], bytes.len());
    assert_eq!(drops[0]["opaque_id"], id);
    assert_eq!(
        drops[0]["kem_ct"],
        BASE64.encode([9_u8; 64]),
        "the encapsulated key survives from the declaration to the inbox; without it the owner \
         holds bytes they can never decrypt"
    );
    assert_eq!(drops[0]["suggested_filename"], "from-a-guest.jpg");
    assert_eq!(drops[0]["adopting"], false);

    // The bytes are committed and content-addressed, and the asset index knows nothing.
    let address = ContentAddress::parse(&checksum(&bytes)).expect("an address");
    assert!(fixture.blobs.stat(&address).await.expect("stat").is_some());
    assert_eq!(
        fixture
            .client
            .get("/v1/sync")
            .header("authorization", &bearer)
            .header("accept", "application/json")
            .send()
            .await
            .assert_status(StatusCode::OK)
            .json::<Value>()["entries"]
            .as_array()
            .expect("entries")
            .len(),
        0,
        "a drop is not an asset until its owner signs for it"
    );
}

/// Provision a link and open one drop session through it, returning the link and the session.
async fn open_drop(fixture: &Fixture, bearer: &str, tag: u8, bytes: &[u8]) -> (String, String) {
    let id = opaque(tag);
    provision(
        fixture,
        bearer,
        &link_body(&id, json!({})),
        StatusCode::CREATED,
    )
    .await;
    let opened = create_drop(fixture, &id, bytes, StatusCode::CREATED).await;
    let upload_id = opened["upload_id"]
        .as_str()
        .expect("a session id")
        .to_owned();
    (id, upload_id)
}

/// Send one chunk at `offset`, returning the response unasserted.
async fn chunk_at(
    fixture: &Fixture,
    id: &str,
    upload_id: &str,
    offset: u64,
    bytes: &[u8],
) -> kynos::test::TestResponse {
    fixture
        .client
        .patch(&format!("/d/{id}/{upload_id}"))
        .header("x-capsule-offset", &offset.to_string())
        .header("x-capsule-checksum", &checksum(bytes))
        .body("application/octet-stream", bytes.to_vec())
        .send()
        .await
}

/// The refusal carries `code`, a `retry_after` instant, and a `Retry-After` delay that is the
/// same instant measured from the server's clock — what the guest's browser waits out.
fn assert_waits(response: &kynos::test::TestResponse, fixture: &Fixture, expected: &str) {
    let body = response.json::<Value>();
    assert_eq!(body["code"], expected);
    let instant = body["retry_after"]
        .as_u64()
        .expect("a wait names its retry instant");
    let now = u64::try_from(fixture.now().as_second()).expect("a post-epoch clock");
    let header = response
        .header("retry-after")
        .expect("and repeats it on Retry-After")
        .parse::<u64>()
        .expect("in delta-seconds");
    assert!(header > 0, "a wait is never zero seconds from now");
    assert_eq!(
        header,
        instant - now,
        "the header and the member name one instant"
    );
}

/// A full disk under a guest's chunk is the album path's `507`, under the drop surface's own
/// code and with a wait. Nothing of the refused chunk was kept, so the same chunk at the same
/// offset lands.
#[tokio::test]
async fn a_full_disk_under_a_guest_chunk_is_a_507_and_the_retry_lands() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'f', 4096);
    let (id, upload_id) = open_drop(&fixture, &bearer, 40, &bytes).await;
    let upload_id = upload_id.as_str();

    fixture.blobs.fill_disk_on_next_append();
    let full = chunk_at(&fixture, &id, upload_id, 0, &bytes).await;
    full.assert_status(StatusCode::INSUFFICIENT_STORAGE);
    assert_waits(&full, &fixture, "error.drop.storage_full");
    assert_eq!(
        fixture.blobs.staged_len_for_test(upload_id).await,
        Some(0),
        "nothing of the refused chunk was kept"
    );
    assert!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .is_empty(),
        "a refused chunk deposits nothing"
    );

    send(&fixture, &id, upload_id, &bytes, StatusCode::NO_CONTENT).await;
    assert_eq!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .len(),
        1,
        "the retry at the same offset completes the drop"
    );
}

/// A store that cannot answer under a guest's chunk is a `503` with a wait, not a `500`: the
/// guest's browser can tell an outage it should wait out from a server that is broken.
#[tokio::test]
async fn an_outage_under_a_guest_chunk_is_a_503_and_the_retry_lands() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'o', 4096);
    let (id, upload_id) = open_drop(&fixture, &bearer, 41, &bytes).await;

    fixture.dropstore.set_unavailable(true);
    let down = chunk_at(&fixture, &id, &upload_id, 0, &bytes).await;
    down.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    assert_waits(&down, &fixture, "error.drop.chunk_unavailable");
    fixture.dropstore.set_unavailable(false);

    fixture.uploads.set_unavailable(true);
    let down = chunk_at(&fixture, &id, &upload_id, 0, &bytes).await;
    down.assert_status(StatusCode::SERVICE_UNAVAILABLE);
    assert_waits(&down, &fixture, "error.drop.chunk_unavailable");
    fixture.uploads.set_unavailable(false);

    send(&fixture, &id, &upload_id, &bytes, StatusCode::NO_CONTENT).await;
    assert_eq!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .len(),
        1,
        "the same chunk at the same offset completes the drop once the store is back"
    );
}

/// A stage shorter than the session's counter stays a `500` with no wait: bytes the server
/// acknowledged are gone, and no resend of the next chunk can bring them back.
#[tokio::test]
async fn a_stage_that_lost_acknowledged_bytes_is_a_500_without_a_wait() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let first = payload(b'p', 4096);
    let rest = payload(b'q', 4096);
    let both: Vec<u8> = first.iter().chain(rest.iter()).copied().collect();
    let (id, upload_id) = open_drop(&fixture, &bearer, 42, &both).await;

    fixture.blobs.swallow_next_append();
    chunk_at(&fixture, &id, &upload_id, 0, &first)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    let broken = chunk_at(&fixture, &id, &upload_id, 4096, &rest).await;
    broken.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    let body = broken.json::<Value>();
    assert_eq!(body["code"], "error.drop.unavailable");
    assert!(
        body.get("retry_after").is_none() && broken.header("retry-after").is_none(),
        "a failure a retry cannot repair is never told to wait"
    );
}

#[tokio::test]
async fn a_drop_that_names_an_album_is_refused_rather_than_ignored() {
    // The other half of invariant 30. A field the server quietly dropped would let a guest
    // believe they had written into an album.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let id = opaque(1);
    provision(
        &fixture,
        &bearer,
        &link_body(&id, json!({})),
        StatusCode::CREATED,
    )
    .await;
    let bytes = payload(b'g', 1024);

    fixture
        .client
        .post(&format!("/d/{id}"))
        .json(&json!({
            "content_type": "image/jpeg",
            "size": bytes.len(),
            "ciphertext_hash": checksum(&bytes),
            "kem_ct": BASE64.encode([9_u8; 64]),
            "album_id": album().as_str(),
        }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
}

#[tokio::test]
async fn a_links_caps_are_spent_once_per_drop_and_refunded_when_one_is_refused() {
    // Invariant 26. The caps are the whole authorization model on a path with no credential, so
    // a refused drop that still spent a slot would let anyone burn a link down without
    // depositing anything.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let id = opaque(1);
    provision(
        &fixture,
        &bearer,
        &link_body(&id, json!({ "max_file_count": 1 })),
        StatusCode::CREATED,
    )
    .await;

    // A refusal that happens *after* the charge — the quota — must refund.
    let fixture_full = Fixture::with_quota(capsule_server::quota::QuotaLimits::new(
        10,
        20,
        SignedDuration::from_hours(24),
    ));
    let full_bearer = fixture_full.bearer().await;
    let full_id = opaque(2);
    provision(
        &fixture_full,
        &full_bearer,
        &link_body(&full_id, json!({ "max_file_count": 1 })),
        StatusCode::CREATED,
    )
    .await;
    let big = payload(b'g', 4096);
    let problem = create_drop(&fixture_full, &full_id, &big, StatusCode::FORBIDDEN).await;
    assert_eq!(problem["code"], "error.quota.exceeded");

    // The link's one slot is still there, which is what the refund buys.
    let small = payload(b'g', 4);
    create_drop(&fixture_full, &full_id, &small, StatusCode::CREATED).await;

    // And on the ordinary link, the second drop is refused for the cap rather than vanishing.
    let bytes = payload(b'g', 512);
    create_drop(&fixture, &id, &bytes, StatusCode::CREATED).await;
    let problem = create_drop(&fixture, &id, &payload(b'h', 512), StatusCode::CONFLICT).await;
    assert_eq!(
        problem["code"], "error.drop.cap_exhausted",
        "a full link is a 409, not the indistinguishable 404: the guest was handed a real link \
         and needs to ask for a new one"
    );
}

#[tokio::test]
async fn a_held_opaque_id_cannot_be_reprovisioned_and_the_refusal_is_one_answer() {
    // #553: every guest a link reaches knows its opaque id. Were a re-provision a replace, any
    // account could swap in its own drop key and take the guests' deposits, and the owner
    // could reset a spent link's totals.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;
    let id = opaque(3);
    provision(
        &fixture,
        &bearer,
        &link_body(&id, json!({ "max_file_count": 1 })),
        StatusCode::CREATED,
    )
    .await;
    create_drop(&fixture, &id, &payload(b'g', 512), StatusCode::CREATED).await;

    let mut takeover = link_body(&id, json!({}));
    takeover["drop_pubkey"] = json!(BASE64.encode([0xEE_u8; 32]));
    let from_stranger = provision(&fixture, &stranger, &takeover, StatusCode::BAD_REQUEST).await;
    let from_owner = provision(&fixture, &bearer, &takeover, StatusCode::BAD_REQUEST).await;
    assert_eq!(
        from_stranger, from_owner,
        "the refusal must not say whose the id is"
    );
    assert_eq!(from_stranger["code"], "error.drop.malformed");

    let problem = create_drop(&fixture, &id, &payload(b'h', 512), StatusCode::CONFLICT).await;
    assert_eq!(
        problem["code"], "error.drop.cap_exhausted",
        "a refused re-provision must leave the spent cap spent"
    );
}

#[tokio::test]
async fn an_oversized_file_is_told_so_rather_than_told_the_link_is_full() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let id = opaque(1);
    provision(
        &fixture,
        &bearer,
        &link_body(
            &id,
            json!({ "max_file_size": 100, "max_total_bytes": 1_000_000 }),
        ),
        StatusCode::CREATED,
    )
    .await;

    let problem = create_drop(
        &fixture,
        &id,
        &payload(b'g', 101),
        StatusCode::PAYLOAD_TOO_LARGE,
    )
    .await;
    assert_eq!(problem["code"], "error.drop.file_too_large");
    assert_eq!(problem["limit"], 100);
}

#[tokio::test]
async fn an_unknown_expired_revoked_or_spent_link_is_one_answer() {
    // The guest path carries no credential, so anything that distinguished these would be an
    // enumeration oracle exactly as on the share path.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'g', 64);

    let expired = opaque(2);
    provision(
        &fixture,
        &bearer,
        &link_body(&expired, json!({ "expires_at": "1970-01-01T00:00:01Z" })),
        StatusCode::CREATED,
    )
    .await;

    let revoked = opaque(3);
    provision(
        &fixture,
        &bearer,
        &link_body(&revoked, json!({})),
        StatusCode::CREATED,
    )
    .await;
    fixture
        .client
        .delete(&format!("/v1/drops/links/{revoked}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let spent = opaque(4);
    provision(
        &fixture,
        &bearer,
        &link_body(&spent, json!({ "single_use": true })),
        StatusCode::CREATED,
    )
    .await;
    create_drop(&fixture, &spent, &bytes, StatusCode::CREATED).await;

    fixture.clock.advance(SignedDuration::from_hours(1));

    let mut bodies = Vec::new();
    for id in [
        opaque(9),
        expired,
        revoked,
        spent,
        "not-an-opaque-id".to_owned(),
    ] {
        bodies.push(create_drop(&fixture, &id, &bytes, StatusCode::NOT_FOUND).await);
    }
    for body in &bodies[1..] {
        assert_eq!(body, &bodies[0], "these must be byte-identical: {body}");
    }
}

#[tokio::test]
async fn a_single_use_link_still_lets_its_own_drop_finish() {
    // The one place "live for a new drop" and "live for these chunks" differ. A single-use link
    // is spent the moment it admits a drop and must nonetheless let that drop land.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let id = opaque(1);
    provision(
        &fixture,
        &bearer,
        &link_body(&id, json!({ "single_use": true })),
        StatusCode::CREATED,
    )
    .await;

    let bytes = payload(b'g', 8192);
    let opened = create_drop(&fixture, &id, &bytes, StatusCode::CREATED).await;
    let upload_id = opened["upload_id"].as_str().expect("a session").to_owned();

    // Two chunks, so the session is genuinely mid-flight when the link is already spent.
    fixture
        .client
        .patch(&format!("/d/{id}/{upload_id}"))
        .header("x-capsule-offset", "0")
        .header("x-capsule-checksum", &checksum(&bytes[..4096]))
        .body("application/octet-stream", bytes[..4096].to_vec())
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    fixture
        .client
        .patch(&format!("/d/{id}/{upload_id}"))
        .header("x-capsule-offset", "4096")
        .header("x-capsule-checksum", &checksum(&bytes[4096..]))
        .body("application/octet-stream", bytes[4096..].to_vec())
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    assert_eq!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .len(),
        1
    );
}

#[tokio::test]
async fn a_revoked_link_stops_a_drop_that_is_already_uploading() {
    // Which is the point of revoking: a guest mid-upload is exactly who the owner is revoking
    // against.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let id = opaque(1);
    provision(
        &fixture,
        &bearer,
        &link_body(&id, json!({})),
        StatusCode::CREATED,
    )
    .await;
    let bytes = payload(b'g', 8192);
    let opened = create_drop(&fixture, &id, &bytes, StatusCode::CREATED).await;
    let upload_id = opened["upload_id"].as_str().expect("a session").to_owned();

    fixture
        .client
        .delete(&format!("/v1/drops/links/{id}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    send(&fixture, &id, &upload_id, &bytes, StatusCode::NOT_FOUND).await;
    assert!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .is_empty()
    );
}

#[tokio::test]
async fn a_session_cannot_be_appended_to_through_another_account_s_link() {
    // Otherwise a guest holding one link could append to a session opened under another by
    // guessing its id.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let mine = opaque(1);
    let theirs = opaque(2);
    provision(
        &fixture,
        &bearer,
        &link_body(&mine, json!({})),
        StatusCode::CREATED,
    )
    .await;

    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;
    provision(
        &fixture,
        &stranger,
        &link_body(&theirs, json!({})),
        StatusCode::CREATED,
    )
    .await;

    let bytes = payload(b'g', 512);
    let opened = create_drop(&fixture, &mine, &bytes, StatusCode::CREATED).await;
    let upload_id = opened["upload_id"].as_str().expect("a session").to_owned();

    send(&fixture, &theirs, &upload_id, &bytes, StatusCode::NOT_FOUND).await;
}

#[tokio::test]
async fn an_inbox_is_the_owners_alone() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    deposit(&fixture, &bearer, 1, &payload(b'g', 256)).await;
    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;

    assert_eq!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .len(),
        1
    );
    assert!(
        inbox(&fixture, &stranger).await["drops"]
            .as_array()
            .expect("drops")
            .is_empty()
    );
}

#[tokio::test]
async fn discarding_removes_a_drop_and_is_only_the_owners() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    deposit(&fixture, &bearer, 1, &payload(b'g', 256)).await;
    let drop_id = inbox(&fixture, &bearer).await["drops"][0]["drop_id"]
        .as_str()
        .expect("a drop id")
        .to_owned();
    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;

    fixture
        .client
        .delete(&format!("/v1/drops/{drop_id}"))
        .header("authorization", &stranger)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .len(),
        1
    );

    fixture
        .client
        .delete(&format!("/v1/drops/{drop_id}"))
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    assert!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .is_empty()
    );
}

#[tokio::test]
async fn an_adoption_promotes_the_drop_and_empties_the_inbox_row() {
    // Invariant 32. The bytes are already committed, so this is a promotion in place rather
    // than a re-upload — and it is the *owner's* signed create that makes an asset.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'g', 4096);
    deposit(&fixture, &bearer, 1, &bytes).await;
    let drop_id = inbox(&fixture, &bearer).await["drops"][0]["drop_id"]
        .as_str()
        .expect("a drop id")
        .to_owned();

    let manifest = support::create_request(&fixture.clock, &bytes, "original");
    let body: Value = fixture
        .client
        .post(&format!("/v1/drops/{drop_id}/adopt"))
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({
            "album_id": album().as_str(),
            "asset_id": ADOPTED,
            "size": bytes.len(),
            "hash": checksum(&bytes),
            "content_type": "image/jpeg",
            "crypto_suite_id": capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
            "protocol_version": PROTOCOL_VERSION,
            "key_mode": "wrapped",
            "manifest_envelope": manifest["manifest_envelope"],
        }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();

    assert_eq!(body["asset_id"], ADOPTED);
    assert!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .is_empty(),
        "the row goes once the asset exists"
    );
    let _ = user();
}

#[tokio::test]
async fn an_adoption_that_is_refused_returns_the_drop_to_the_inbox() {
    // The honest half of invariant 32: across two ports there is no transaction, so a failed
    // promotion must lose nothing and duplicate nothing. A claim makes that true.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'g', 4096);
    deposit(&fixture, &bearer, 1, &bytes).await;
    let drop_id = inbox(&fixture, &bearer).await["drops"][0]["drop_id"]
        .as_str()
        .expect("a drop id")
        .to_owned();
    let manifest = support::create_request(&fixture.clock, &bytes, "original");

    // A `key_mode` outside its closed enum, refused before anything is claimed.
    let problem: Value = fixture
        .client
        .post(&format!("/v1/drops/{drop_id}/adopt"))
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({
            "album_id": album().as_str(),
            "asset_id": ADOPTED,
            "size": bytes.len(),
            "hash": checksum(&bytes),
            "content_type": "image/jpeg",
            "crypto_suite_id": capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
            "protocol_version": PROTOCOL_VERSION,
            "key_mode": "smuggled",
            "manifest_envelope": manifest["manifest_envelope"],
        }))
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .json();
    assert_eq!(problem["code"], "error.drop.adoption_refused");

    // A manifest naming somebody else's blob, refused *after* the claim — so this is the case
    // the release exists for.
    let other = payload(b'z', 4096);
    fixture
        .client
        .post(&format!("/v1/drops/{drop_id}/adopt"))
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({
            "album_id": album().as_str(),
            "asset_id": ADOPTED,
            "size": other.len(),
            "hash": checksum(&other),
            "content_type": "image/jpeg",
            "crypto_suite_id": capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
            "protocol_version": PROTOCOL_VERSION,
            "key_mode": "wrapped",
            "manifest_envelope": manifest["manifest_envelope"],
        }))
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    let held = inbox(&fixture, &bearer).await;
    let drops = held["drops"].as_array().expect("drops");
    assert_eq!(drops.len(), 1, "the drop is still there");
    assert_eq!(
        drops[0]["adopting"], false,
        "and it is claimable again, rather than stuck holding a claim nobody owns"
    );
}

#[tokio::test]
async fn another_accounts_drop_cannot_be_adopted() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'g', 4096);
    deposit(&fixture, &bearer, 1, &bytes).await;
    let drop_id = inbox(&fixture, &bearer).await["drops"][0]["drop_id"]
        .as_str()
        .expect("a drop id")
        .to_owned();
    let stranger = fixture
        .other_bearer("01937b7c-0000-7000-8000-0000000000ff")
        .await;
    let manifest = support::create_request(&fixture.clock, &bytes, "original");

    fixture
        .client
        .post(&format!("/v1/drops/{drop_id}/adopt"))
        .header("authorization", &stranger)
        .header("accept", "application/json")
        .json(&json!({
            "album_id": album().as_str(),
            "asset_id": ADOPTED,
            "size": bytes.len(),
            "hash": checksum(&bytes),
            "content_type": "image/jpeg",
            "crypto_suite_id": capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
            "protocol_version": PROTOCOL_VERSION,
            "key_mode": "wrapped",
            "manifest_envelope": manifest["manifest_envelope"],
        }))
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND);

    assert_eq!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .len(),
        1
    );
}

/// The asset id an adoption names: the `file_id` [`support::create_request`] signs, which the
/// request's `asset_id` must restate. The adoption-refusal cases collide with it.
const ADOPTED: &str = support::OPS_ASSET;

/// Deposit one drop and return its id, the bytes it holds, and a create manifest over them.
async fn pending_drop(fixture: &Fixture, bearer: &str) -> (String, Vec<u8>, Value) {
    let bytes = payload(b'g', 4096);
    deposit(fixture, bearer, 1, &bytes).await;
    let drop_id = inbox(fixture, bearer).await["drops"][0]["drop_id"]
        .as_str()
        .expect("a drop id")
        .to_owned();
    let manifest = support::create_request(&fixture.clock, &bytes, "original");
    (drop_id, bytes, manifest)
}

/// Adopt `drop_id` as [`ADOPTED`] with `envelope`, expecting `expect`.
async fn adopt(
    fixture: &Fixture,
    bearer: &str,
    drop_id: &str,
    bytes: &[u8],
    envelope: &Value,
    expect: StatusCode,
) -> Value {
    adopt_as(fixture, bearer, drop_id, ADOPTED, bytes, envelope, expect).await
}

/// Adopt `drop_id` as `asset_id` with `envelope`, expecting `expect`.
async fn adopt_as(
    fixture: &Fixture,
    bearer: &str,
    drop_id: &str,
    asset_id: &str,
    bytes: &[u8],
    envelope: &Value,
    expect: StatusCode,
) -> Value {
    fixture
        .client
        .post(&format!("/v1/drops/{drop_id}/adopt"))
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(&json!({
            "album_id": album().as_str(),
            "asset_id": asset_id,
            "size": bytes.len(),
            "hash": checksum(bytes),
            "content_type": "image/jpeg",
            "crypto_suite_id": capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
            "protocol_version": PROTOCOL_VERSION,
            "key_mode": "wrapped",
            "manifest_envelope": envelope,
        }))
        .send()
        .await
        .assert_status(expect)
        .json()
}

/// The refused adoption left its drop in the inbox, unclaimed.
async fn assert_drop_kept(fixture: &Fixture, bearer: &str) {
    let held = inbox(fixture, bearer).await;
    let drops = held["drops"].as_array().expect("drops");
    assert_eq!(
        drops.len(),
        1,
        "a refused adoption does not settle its drop"
    );
    assert_eq!(drops[0]["adopting"], false, "and it is claimable again");
}

/// Reserve [`ADOPTED`] in the index under `owner_id`, in the fixture's album and pin.
async fn reserve_adopted(fixture: &Fixture, owner_id: OwnerId) {
    let reserved = fixture
        .index
        .reserve(PendingAsset {
            asset_id: AssetId::new(ADOPTED),
            owner_id,
            album_id: album(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            crypto_suite_id: capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
            created_at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the index reserves");
    assert!(matches!(reserved, Reservation::Created(_)));
}

#[tokio::test]
async fn an_adoption_onto_another_owners_pending_asset_is_refused_and_keeps_the_drop() {
    // The asset id is client-chosen. One already filed under another account is a reservation
    // conflict, and an adoption over it must neither answer `200` nor settle the drop — which
    // would lose the drop and leave no asset of the caller's behind it.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (drop_id, bytes, manifest) = pending_drop(&fixture, &bearer).await;
    reserve_adopted(
        &fixture,
        OwnerId::new("01937b7c-0000-7000-8000-0000000000ff"),
    )
    .await;

    let problem = adopt(
        &fixture,
        &bearer,
        &drop_id,
        &bytes,
        &manifest["manifest_envelope"],
        StatusCode::BAD_REQUEST,
    )
    .await;

    assert_eq!(problem["code"], "error.drop.adoption_refused");
    assert_drop_kept(&fixture, &bearer).await;
    let row = fixture
        .index
        .read(&AssetId::new(ADOPTED))
        .await
        .expect("the index answers")
        .expect("the foreign row stands");
    assert!(row.blobs.is_empty(), "nothing was recorded against it");
}

#[tokio::test]
async fn an_adoption_onto_an_asset_holding_another_original_is_refused_and_keeps_the_drop() {
    // The owner's own pending row joins, but it already holds a different original: recording
    // this drop's blob is a conflict, not a re-point, and the drop must stay in the inbox.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (drop_id, bytes, manifest) = pending_drop(&fixture, &bearer).await;
    reserve_adopted(&fixture, owner()).await;
    let elsewhere = payload(b'o', 4096);
    let address = ContentAddress::parse(&checksum(&elsewhere)).expect("a content address");
    fixture
        .index
        .record_blob(
            &AssetId::new(ADOPTED),
            BlobRecord {
                role: BlobRole::Original,
                address,
                size: elsewhere.len() as u64,
                manifest_sha256: None,
                amk_version: None,
                finalized_at: Timestamp::UNIX_EPOCH,
            },
        )
        .await
        .expect("the index records");

    let problem = adopt(
        &fixture,
        &bearer,
        &drop_id,
        &bytes,
        &manifest["manifest_envelope"],
        StatusCode::BAD_REQUEST,
    )
    .await;

    assert_eq!(problem["code"], "error.drop.adoption_refused");
    assert_drop_kept(&fixture, &bearer).await;
    assert!(
        adopted_receipts(&fixture).await.is_empty(),
        "a refused adoption committed nothing, so it has no receipt"
    );
}

/// Every custody receipt the log holds for [`ADOPTED`].
async fn adopted_receipts(fixture: &Fixture) -> Vec<CustodyReceipt> {
    use capsule_server::attestation::ReceiptLog as _;
    fixture
        .receipts
        .for_asset(&AssetId::new(ADOPTED))
        .await
        .expect("the log answers")
}

#[tokio::test]
async fn an_adopted_blob_has_a_receipt_that_verifies_under_the_published_key() {
    // Invariant 33: adoption is the second path that indexes a blob, so it is the second path
    // that must write a receipt with it. Read back the way the owner reads it — the durable
    // chain — and verified the way a client verifies one.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (drop_id, bytes, manifest) = pending_drop(&fixture, &bearer).await;

    adopt(
        &fixture,
        &bearer,
        &drop_id,
        &bytes,
        &manifest["manifest_envelope"],
        StatusCode::OK,
    )
    .await;

    let body: Value = fixture
        .client
        .get(&format!("/v1/assets/{ADOPTED}/receipts"))
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let entries = body["receipts"].as_array().expect("a receipts array");
    assert_eq!(entries.len(), 1, "one custody event, one receipt");
    assert_eq!(
        entries[0]["upload_id"], drop_id,
        "the receipt is filed under the drop the owner adopted"
    );

    let cbor = BASE64
        .decode(entries[0]["receipt_cbor"].as_str().expect("base64 CBOR"))
        .expect("the CBOR decodes from base64");
    let receipt = CustodyReceipt::from_canonical_cbor(&cbor).expect("a receipt");
    assert_eq!(
        verify_receipt(
            &receipt,
            &[fixture.attestation_key.verifying_key()],
            &ReceiptExpectations {
                ciphertext_hash: hash_bytes(&bytes),
                size: bytes.len() as u64,
                role: ReceiptRole::Original,
                envelope_hash: None,
            },
            receipt
                .core
                .received_at
                .parse::<Timestamp>()
                .expect("an instant")
                .as_second(),
        ),
        Ok(()),
    );
    assert_eq!(receipt.core.asset_id, ADOPTED);
    assert_eq!(
        receipt.core.uploaded_by_user,
        user().as_str(),
        "an adopted drop's author is the adopter"
    );
}

#[tokio::test]
async fn a_retried_adoption_returns_the_same_receipt() {
    // The index commits the blob and its receipt, and the answer is lost: the adoption is
    // refused as unavailable and the drop returns to the inbox. The retry finds the blob
    // already held, and must get the receipt that committed — never a second one for the same
    // custody event.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (drop_id, bytes, manifest) = pending_drop(&fixture, &bearer).await;

    fixture.index_fault.arm_after_commit();
    adopt(
        &fixture,
        &bearer,
        &drop_id,
        &bytes,
        &manifest["manifest_envelope"],
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    assert_eq!(fixture.index_fault.fired(), 1, "the fault fired");
    assert_drop_kept(&fixture, &bearer).await;
    let committed = adopted_receipts(&fixture).await;
    assert_eq!(
        committed.len(),
        1,
        "the lost answer still committed the rows and their receipt together"
    );

    adopt(
        &fixture,
        &bearer,
        &drop_id,
        &bytes,
        &manifest["manifest_envelope"],
        StatusCode::OK,
    )
    .await;

    assert_eq!(
        adopted_receipts(&fixture).await,
        committed,
        "the retry returned the receipt that committed rather than minting another"
    );
}

#[tokio::test]
async fn a_retried_adoption_naming_another_asset_is_refused_and_indexes_nothing() {
    // The receipt is filed under the drop id, which names no asset. A retry after a lost
    // answer that signs a fresh manifest (another `file_id`) must not index the blob under a
    // second asset while the log hands back the first asset's receipt: that second asset would
    // be indexed with no receipt (invariant 33). It is refused, names the asset the drop was
    // adopted as, and a retry naming that asset then settles the drop.
    const RETRIED: &str = "018f3f1e-4b7a-7c9d-8e2f-00000000cafe";
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (drop_id, bytes, manifest) = pending_drop(&fixture, &bearer).await;

    fixture.index_fault.arm_after_commit();
    adopt(
        &fixture,
        &bearer,
        &drop_id,
        &bytes,
        &manifest["manifest_envelope"],
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    assert_eq!(fixture.index_fault.fired(), 1, "the fault fired");
    let committed = adopted_receipts(&fixture).await;
    assert_eq!(committed.len(), 1, "the first adoption committed");

    let mut retried = manifest["manifest_envelope"].clone();
    retried["file_id"] = RETRIED.into();
    let problem = adopt_as(
        &fixture,
        &bearer,
        &drop_id,
        RETRIED,
        &bytes,
        &retried,
        StatusCode::BAD_REQUEST,
    )
    .await;

    assert_eq!(problem["code"], "error.drop.adoption_refused");
    assert!(
        problem["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains(ADOPTED)),
        "the refusal names the asset the drop was adopted as: {problem}"
    );
    assert_drop_kept(&fixture, &bearer).await;
    assert!(
        fixture
            .index
            .read(&AssetId::new(RETRIED))
            .await
            .expect("the index answers")
            .is_none(),
        "no second asset was reserved or indexed for the drop's blob"
    );
    {
        use capsule_server::attestation::ReceiptLog as _;
        assert!(
            fixture
                .receipts
                .for_asset(&AssetId::new(RETRIED))
                .await
                .expect("the log answers")
                .is_empty(),
            "and no receipt names it"
        );
    }

    adopt(
        &fixture,
        &bearer,
        &drop_id,
        &bytes,
        &manifest["manifest_envelope"],
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        adopted_receipts(&fixture).await,
        committed,
        "the retry naming the committed asset settles on the receipt that committed"
    );
}

#[tokio::test]
async fn an_adoption_attributed_to_another_account_is_refused_and_keeps_the_drop() {
    // Invariant 7's account half, as the upload and ops surfaces enforce it: the adopter is the
    // author, so a manifest naming somebody else is refused before anything is reserved, and
    // the claim is released.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (drop_id, bytes, mut manifest) = pending_drop(&fixture, &bearer).await;
    manifest["manifest_envelope"]["created_by_user"] =
        "01937b7c-0000-7000-8000-0000000000ff".into();

    let problem = adopt(
        &fixture,
        &bearer,
        &drop_id,
        &bytes,
        &manifest["manifest_envelope"],
        StatusCode::BAD_REQUEST,
    )
    .await;

    assert_eq!(problem["code"], "error.drop.adoption_refused");
    assert_drop_kept(&fixture, &bearer).await;
    assert!(
        fixture
            .index
            .read(&AssetId::new(ADOPTED))
            .await
            .expect("the index answers")
            .is_none(),
        "no asset was reserved for it"
    );
}

#[tokio::test]
async fn an_adoption_whose_asset_id_is_not_the_signed_file_id_is_refused_and_keeps_the_drop() {
    // The signed manifest's `file_id` is the asset id; `asset_id` only restates it. An adoption
    // whose two disagree would reserve under an id the owner's signature does not cover, so it
    // is refused before anything is claimed or reserved, under either id.
    const SIGNED: &str = "018f3f1e-4b7a-7c9d-8e2f-00000000beef";
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (drop_id, bytes, mut manifest) = pending_drop(&fixture, &bearer).await;
    manifest["manifest_envelope"]["file_id"] = SIGNED.into();

    let problem = adopt(
        &fixture,
        &bearer,
        &drop_id,
        &bytes,
        &manifest["manifest_envelope"],
        StatusCode::BAD_REQUEST,
    )
    .await;

    assert_eq!(problem["code"], "error.drop.adoption_refused");
    assert_drop_kept(&fixture, &bearer).await;
    for id in [ADOPTED, SIGNED] {
        assert!(
            fixture
                .index
                .read(&AssetId::new(id))
                .await
                .expect("the index answers")
                .is_none(),
            "no asset was reserved under {id}"
        );
    }
}

#[tokio::test]
async fn the_owner_operations_need_a_credential_and_the_guest_path_does_not() {
    let fixture = Fixture::working();
    for (method, path) in [("POST", "/v1/drops/links"), ("GET", "/v1/drops")] {
        let request = if method == "GET" {
            fixture.client.get(path)
        } else {
            fixture.client.post(path)
        };
        request
            .json(&json!({}))
            .send()
            .await
            .assert_status(StatusCode::UNAUTHORIZED);
    }

    // And the guest path answers on its own merits with no credential in sight.
    create_drop(
        &fixture,
        &opaque(9),
        &payload(b'g', 8),
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn drop_session_creation_is_rate_limited_per_link() {
    // Invariant 31's per-link half. The caps bound how much a link can ever deposit; this
    // bounds how fast it can be hammered, and the two are not substitutes — a link with a
    // generous byte cap and no limiter is a free amplifier.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let id = opaque(1);
    provision(
        &fixture,
        &bearer,
        &link_body(&id, json!({ "max_file_count": 1000 })),
        StatusCode::CREATED,
    )
    .await;

    let bytes = payload(b'g', 64);
    for _ in 0..30 {
        fixture
            .client
            .post(&format!("/d/{id}"))
            .json(&json!({
                "content_type": "image/jpeg",
                "size": bytes.len(),
                "ciphertext_hash": checksum(&bytes),
                "kem_ct": BASE64.encode([9_u8; 64]),
            }))
            .send()
            .await;
    }

    let problem = create_drop(&fixture, &id, &bytes, StatusCode::TOO_MANY_REQUESTS).await;
    assert_eq!(problem["code"], "error.drop.rate_limited");

    // A second link has its own budget: one hammered link must not close every other.
    let other = opaque(2);
    provision(
        &fixture,
        &bearer,
        &link_body(&other, json!({})),
        StatusCode::CREATED,
    )
    .await;
    create_drop(&fixture, &other, &bytes, StatusCode::CREATED).await;
}

#[tokio::test]
async fn probing_a_link_that_does_not_exist_still_costs_the_prober() {
    // Charged before the link is resolved, so a caller walking the id space pays for it — the
    // same discipline the share path applies, and for the same reason.
    let fixture = Fixture::working();
    let bytes = payload(b'g', 64);
    let unknown = opaque(9);

    for _ in 0..30 {
        fixture
            .client
            .post(&format!("/d/{unknown}"))
            .json(&json!({
                "content_type": "image/jpeg",
                "size": bytes.len(),
                "ciphertext_hash": checksum(&bytes),
                "kem_ct": BASE64.encode([9_u8; 64]),
            }))
            .send()
            .await;
    }
    create_drop(&fixture, &unknown, &bytes, StatusCode::TOO_MANY_REQUESTS).await;
}

// ===========================================================================================
// The passphrase abuse gate
// ===========================================================================================

#[tokio::test]
async fn a_gated_link_refuses_a_drop_with_no_proof() {
    // The case this test exists for. The gate was provisioned and never checked, so a
    // passphrase-gated link spent the owner's quota for anyone holding the opaque id.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (verifier, _) = gate();
    let id = opaque(60);
    provision(
        &fixture,
        &bearer,
        &link_body(&id, json!({ "passphrase_verifier": verifier })),
        StatusCode::CREATED,
    )
    .await;

    let bytes = payload(b'p', 4096);
    let body = create_drop_with(&fixture, &id, &bytes, None, StatusCode::FORBIDDEN).await;
    assert_eq!(body["code"], "error.drop.passphrase_required");
}

#[tokio::test]
async fn a_wrong_proof_is_refused_and_spends_no_cap() {
    // A guess that burned a cap would let a guesser take the link down, which is the one thing
    // the passphrase exists to stop — so the refusal has to land before the reservation.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (verifier, proof) = gate();
    let id = opaque(61);
    provision(
        &fixture,
        &bearer,
        &link_body(
            &id,
            json!({ "passphrase_verifier": verifier, "max_file_count": 1 }),
        ),
        StatusCode::CREATED,
    )
    .await;

    let bytes = payload(b'q', 4096);
    for _ in 0..3 {
        create_drop_with(
            &fixture,
            &id,
            &bytes,
            Some(&BASE64.encode([0x5A_u8; 32])),
            StatusCode::FORBIDDEN,
        )
        .await;
    }

    // The link's single file cap is intact: the right proof still opens a session.
    create_drop_with(&fixture, &id, &bytes, Some(&proof), StatusCode::CREATED).await;
}

#[tokio::test]
async fn the_right_proof_opens_a_session() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (verifier, proof) = gate();
    let id = opaque(62);
    provision(
        &fixture,
        &bearer,
        &link_body(&id, json!({ "passphrase_verifier": verifier })),
        StatusCode::CREATED,
    )
    .await;

    let bytes = payload(b'r', 4096);
    let opened = create_drop_with(&fixture, &id, &bytes, Some(&proof), StatusCode::CREATED).await;
    let upload_id = opened["upload_id"].as_str().expect("an upload id");
    send(&fixture, &id, upload_id, &bytes, StatusCode::NO_CONTENT).await;

    let listing = inbox(&fixture, &bearer).await;
    assert_eq!(listing["drops"].as_array().expect("an array").len(), 1);
}

#[tokio::test]
async fn a_proof_presented_to_an_ungated_link_is_ignored() {
    // An ungated link has nothing to compare against, and refusing a caller who sent one anyway
    // would break a guest who typed a passphrase into a link that never needed it.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let id = opaque(63);
    provision(
        &fixture,
        &bearer,
        &link_body(&id, json!({})),
        StatusCode::CREATED,
    )
    .await;

    let bytes = payload(b's', 4096);
    create_drop_with(
        &fixture,
        &id,
        &bytes,
        Some(&BASE64.encode([0x11_u8; 32])),
        StatusCode::CREATED,
    )
    .await;
}

#[tokio::test]
async fn a_gated_link_that_does_not_exist_is_still_indistinguishable() {
    // The 403 must not become an oracle over which opaque ids name a real link: an id that
    // resolves to nothing answers the same 404 it always did, proof or no proof.
    let fixture = Fixture::working();
    let (_, proof) = gate();
    let bytes = payload(b't', 4096);

    create_drop_with(
        &fixture,
        &opaque(64),
        &bytes,
        Some(&proof),
        StatusCode::NOT_FOUND,
    )
    .await;
    create_drop_with(&fixture, &opaque(65), &bytes, None, StatusCode::NOT_FOUND).await;
}

/// A saturated limiter partition must not impersonate an outage.
///
/// The drop path charges a caller-supplied opaque id before it resolves the link, and its window
/// is an hour long — the cheapest partition on the surface to hold saturated, at under six
/// fabricated ids a second. Partitioning (decision 22) keeps that off the other surfaces; it
/// does not keep it off this one, so every first-time visitor to *any* drop link is refused while
/// the flood runs. They are told `429 error.drop.at_capacity` with a retry hint rather than the
/// `500 error.drop.unavailable` this used to render, which is the difference between a client
/// backing off and an operator being paged for an outage that is not happening.
#[tokio::test]
async fn a_saturated_limiter_partition_is_a_429_and_not_an_outage() {
    let fixture = Fixture::with_counter_ceiling(1);
    let bearer = fixture.bearer().await;
    let bytes = payload(b'h', 64);

    let id = opaque(1);
    provision(
        &fixture,
        &bearer,
        &link_body(&id, json!({ "max_file_count": 1000 })),
        StatusCode::CREATED,
    )
    .await;
    let other = opaque(2);
    provision(
        &fixture,
        &bearer,
        &link_body(&other, json!({ "max_file_count": 1000 })),
        StatusCode::CREATED,
    )
    .await;

    // One link fills the partition.
    create_drop(&fixture, &id, &bytes, StatusCode::CREATED).await;

    // A guest arriving at a different link finds no room, and is told to wait rather than told
    // the server is broken.
    let problem = create_drop(&fixture, &other, &bytes, StatusCode::TOO_MANY_REQUESTS).await;
    assert_eq!(
        problem["code"], "error.drop.at_capacity",
        "a full partition has a code of its own, distinct from a spent budget and from an outage"
    );
    assert!(problem["retry_after"].as_u64().is_some_and(|s| s > 0));

    // The link already being counted keeps working, to its own budget, and the spent-budget
    // refusal keeps its own code — the two causes stay distinguishable on the wire.
    create_drop(&fixture, &id, &bytes, StatusCode::CREATED).await;
    for _ in 0..30 {
        fixture
            .client
            .post(&format!("/d/{id}"))
            .json(&json!({
                "content_type": "image/jpeg",
                "size": bytes.len(),
                "ciphertext_hash": checksum(&bytes),
                "kem_ct": BASE64.encode([9_u8; 64]),
            }))
            .send()
            .await;
    }
    let problem = create_drop(&fixture, &id, &bytes, StatusCode::TOO_MANY_REQUESTS).await;
    assert_eq!(problem["code"], "error.drop.rate_limited");
    assert!(problem["retry_after"].as_u64().is_some_and(|s| s > 0));
}

/// Open a drop session from `peer`, returning the status.
async fn create_drop_from(fixture: &Fixture, id: &str, bytes: &[u8], peer: &str) -> StatusCode {
    fixture
        .client
        .post(&format!("/d/{id}"))
        .header("accept", "application/json")
        .peer(SocketAddr::new(
            peer.parse().expect("a test address"),
            40_000,
        ))
        .json(&json!({
            "content_type": "image/jpeg",
            "size": bytes.len(),
            "ciphertext_hash": checksum(bytes),
            "kem_ct": BASE64.encode([9_u8; 64]),
        }))
        .send()
        .await
        .status()
}

#[tokio::test]
async fn drop_session_creation_is_rate_limited_per_source_independently_of_the_link() {
    // Invariant 31's per-source half (#704). An address spraying fabricated ids never spends
    // any one link's budget, so only a per-source key sees it — and once it is spent, the
    // sprayer cannot drain the budget of a real link a guest was sent.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let id = opaque(1);
    provision(
        &fixture,
        &bearer,
        &link_body(&id, json!({ "max_file_count": 1000 })),
        StatusCode::CREATED,
    )
    .await;
    let bytes = payload(b'h', 64);

    // `DROP_SOURCE` is sixty an hour, each against a different unknown link.
    for tag in 0..60_u8 {
        let status = create_drop_from(&fixture, &opaque(100 + tag), &bytes, "203.0.113.9").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    assert_eq!(
        create_drop_from(&fixture, &id, &bytes, "203.0.113.9").await,
        StatusCode::TOO_MANY_REQUESTS,
        "the sprayer is refused even on a live link"
    );
    assert_eq!(
        create_drop_from(&fixture, &id, &bytes, "198.51.100.7").await,
        StatusCode::CREATED,
        "the link's own budget is untouched, so a guest elsewhere still deposits"
    );
}

// ===========================================================================================
// The owner ledger (#753)
// ===========================================================================================

/// An adoption ledgers its custody before the index write, and the receipt it issued after, as
/// a finalization does — under the drop id.
#[tokio::test]
async fn an_adoption_ledgers_custody_then_the_index_write() {
    use capsule_server::ledger::LedgerEvent;

    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (drop_id, bytes, manifest) = pending_drop(&fixture, &bearer).await;
    assert!(
        fixture.verified_ledger(&owner()).await.is_empty(),
        "a deposit indexes nothing, so it ledgers nothing"
    );

    adopt(
        &fixture,
        &bearer,
        &drop_id,
        &bytes,
        &manifest["manifest_envelope"],
        StatusCode::OK,
    )
    .await;

    let entries = fixture.verified_ledger(&owner()).await;
    let [custody, indexed] = entries.as_slice() else {
        panic!("one custody and one indexed entry, got {entries:?}");
    };
    let LedgerEvent::Custody {
        upload_id,
        asset_id,
        content_hash,
        size,
        upload_user_id,
        ..
    } = &custody.core.event
    else {
        panic!("the first entry is the custody: {custody:?}");
    };
    assert_eq!(upload_id, &drop_id);
    assert_eq!(asset_id, ADOPTED);
    assert_eq!(content_hash, &hash_bytes(&bytes));
    assert_eq!(*size, bytes.len() as u64);
    assert_eq!(upload_user_id, user().as_str());

    let LedgerEvent::Indexed {
        upload_id, receipt, ..
    } = &indexed.core.event
    else {
        panic!("the second entry is the index write: {indexed:?}");
    };
    assert_eq!(upload_id, &drop_id);
    assert_eq!(
        receipt.as_deref().map(|receipt| vec![receipt.clone()]),
        Some(adopted_receipts(&fixture).await),
        "the ledger holds the receipt the index issued"
    );
}

/// A ledger that cannot append refuses the adoption before the index, and keeps the drop.
#[tokio::test]
async fn a_ledger_that_cannot_append_refuses_the_adoption_and_keeps_the_drop() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (drop_id, bytes, manifest) = pending_drop(&fixture, &bearer).await;
    fixture.ledger.set_unavailable(true);

    let problem = adopt(
        &fixture,
        &bearer,
        &drop_id,
        &bytes,
        &manifest["manifest_envelope"],
        StatusCode::INTERNAL_SERVER_ERROR,
    )
    .await;
    assert_eq!(problem["code"], "error.drop.unavailable");
    assert_drop_kept(&fixture, &bearer).await;
    assert!(
        adopted_receipts(&fixture).await.is_empty(),
        "nothing was indexed, so nothing was receipted"
    );
    let row = fixture
        .index
        .read(&AssetId::new(ADOPTED))
        .await
        .expect("the index answers")
        .expect("the adoption reserved a row");
    assert!(row.blobs.is_empty(), "no row without a ledger entry");
}

/// An adoption the index refuses resolves its custody entry with a refused one.
#[tokio::test]
async fn a_refused_adoption_resolves_its_custody_entry() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let (drop_id, bytes, manifest) = pending_drop(&fixture, &bearer).await;
    reserve_adopted(&fixture, owner()).await;
    let elsewhere = payload(b'o', 4096);
    fixture
        .index
        .record_blob(
            &AssetId::new(ADOPTED),
            BlobRecord {
                role: BlobRole::Original,
                address: ContentAddress::parse(&checksum(&elsewhere)).expect("a content address"),
                size: elsewhere.len() as u64,
                manifest_sha256: None,
                amk_version: None,
                finalized_at: Timestamp::UNIX_EPOCH,
            },
        )
        .await
        .expect("the index records");

    adopt(
        &fixture,
        &bearer,
        &drop_id,
        &bytes,
        &manifest["manifest_envelope"],
        StatusCode::BAD_REQUEST,
    )
    .await;

    let kinds: Vec<_> = fixture
        .verified_ledger(&owner())
        .await
        .iter()
        .map(|entry| entry.core.event.kind())
        .collect();
    assert_eq!(
        kinds,
        ["custody", "refused"],
        "a rebuild must not replay a write the server refused"
    );
}

// ===========================================================================================
// A full drop reaches the inbox whatever happens to its request (#818)
// ===========================================================================================

/// Wait, boundedly, for `upload_id`'s session to reach a terminal status, and return it.
///
/// The deposit is its own task once the request that started it is gone, so a case that dropped
/// that request has nothing to await but the store.
async fn terminal_status(fixture: &Fixture, upload_id: &str) -> &'static str {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let record = fixture
                .uploads
                .read_for_test(upload_id)
                .await
                .expect("the session survives as a receipt");
            if record.status.is_terminal() {
                return record.status.as_str();
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("a full drop session reaches a terminal status")
}

/// The owner's inbox holds exactly one drop: `bytes`, at their address, with the guest's
/// encapsulated key.
async fn assert_one_drop(fixture: &Fixture, bearer: &str, bytes: &[u8]) {
    let body = inbox(fixture, bearer).await;
    let drops = body["drops"].as_array().expect("drops");
    assert_eq!(drops.len(), 1, "exactly one drop is filed: {body}");
    assert_eq!(drops[0]["ciphertext_hash"], checksum(bytes));
    assert_eq!(drops[0]["size"], bytes.len());
    assert_eq!(drops[0]["kem_ct"], BASE64.encode([9_u8; 64]));
    let address = ContentAddress::parse(&checksum(bytes)).expect("an address");
    assert!(
        fixture.blobs.stat(&address).await.expect("stat").is_some(),
        "the drop's bytes are committed at their address"
    );
}

/// Record `last` at `offset` the way the completing request does, and then lose the request
/// before it claims the deposit: the bytes staged and counted, the session full and unclaimed.
async fn record_without_depositing(fixture: &Fixture, upload_id: &str, offset: u64, last: &[u8]) {
    use capsule_server::store::{AcceptedChunk, Clock as _, UploadId, UploadSessionStore as _};

    let upload = UploadId::new(upload_id);
    fixture
        .blobs
        .append(&upload, offset, last)
        .await
        .expect("the stage appends");
    let recorded = fixture
        .uploads
        .record_progress(
            &upload,
            AcceptedChunk {
                offset,
                chunk_hash: checksum(last),
                next_offset: offset + last.len() as u64,
                accepted_at: fixture.clock.now(),
            },
        )
        .await
        .expect("the store records progress")
        .expect("the session is live");
    assert_eq!(
        recorded.received_bytes, recorded.total_size,
        "the session is full"
    );
    assert_eq!(
        recorded.status.as_str(),
        "uploading",
        "and nobody has claimed it"
    );
}

/// Run boot recovery over the fixture's stores, as a restarted process would before it binds:
/// the album recovery and the drop recovery, over the one listing of claims.
async fn recover_at_boot(
    fixture: &Fixture,
) -> (
    capsule_server::upload::finalize::Recovery,
    capsule_server::upload::finalize::Recovery,
) {
    use capsule_server::attestation::AttestationContext;
    use capsule_server::drop::DropContext;
    use capsule_server::upload::UploadContext;
    use kynos::di::Provides as _;

    let app = fixture.app();
    let upload: UploadContext = app.provide();
    let attestation: AttestationContext = app.provide();
    let drops: DropContext = app.provide();
    let stranded = upload
        .sessions()
        .claimed()
        .await
        .expect("the store lists its claims");
    let albums =
        capsule_server::upload::finalize::recover(&upload, &attestation, stranded.clone()).await;
    let deposits = capsule_server::drop::deposit::recover(&drops, stranded).await;
    (albums, deposits)
}

#[tokio::test]
async fn a_guest_who_disconnects_after_the_commit_still_has_their_drop_filed() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'd', 4096);
    let (id, upload_id) = open_drop(&fixture, &bearer, 60, &bytes).await;

    // The only chunk completes the drop and claims its deposit; park the deposit after its
    // commit has landed, then drop the request there — the guest's browser went away between
    // the commit and the inbox row.
    let hold = fixture.blobs.hold_commits();
    {
        let completing = chunk_at(&fixture, &id, &upload_id, 0, &bytes);
        tokio::pin!(completing);
        tokio::select! {
            response = &mut completing => {
                panic!("the chunk answered before its commit was parked: {}", response.status());
            }
            () = hold.entered.notified() => {}
        }
    }
    assert_eq!(
        fixture.blobs.staged_len_for_test(&upload_id).await,
        None,
        "the commit landed before the request went away"
    );
    hold.release.add_permits(1);

    // The deposit the request started finishes without it.
    assert_eq!(terminal_status(&fixture, &upload_id).await, "completed");
    assert_one_drop(&fixture, &bearer, &bytes).await;

    // The guest's resend finds a finished drop and files nothing twice.
    chunk_at(&fixture, &id, &upload_id, 0, &bytes)
        .await
        .assert_status(StatusCode::CONFLICT);
    assert_one_drop(&fixture, &bearer, &bytes).await;
}

#[tokio::test]
async fn a_full_drop_nobody_deposited_is_deposited_by_the_replay_of_its_last_chunk() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let first = payload(b'r', 4096);
    let last = payload(b's', 4096);
    let both: Vec<u8> = first.iter().chain(last.iter()).copied().collect();
    let (id, upload_id) = open_drop(&fixture, &bearer, 61, &both).await;
    chunk_at(&fixture, &id, &upload_id, 0, &first)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    record_without_depositing(&fixture, &upload_id, 4096, &last).await;
    assert!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .is_empty(),
        "nothing filed the drop"
    );

    let replay = chunk_at(&fixture, &id, &upload_id, 4096, &last).await;
    replay.assert_status(StatusCode::NO_CONTENT);
    assert_eq!(
        replay.header("x-capsule-offset"),
        Some("8192"),
        "the replay is answered with the offset its chunk reached"
    );
    assert_eq!(terminal_status(&fixture, &upload_id).await, "completed");
    assert_one_drop(&fixture, &bearer, &both).await;
}

/// Run the boot pass over full, unclaimed sessions, as a restarted process does: one listing,
/// the album finisher and the drop finisher.
async fn finish_unclaimed_at_boot(
    fixture: &Fixture,
) -> (
    capsule_server::upload::finalize::Recovery,
    capsule_server::upload::finalize::Recovery,
) {
    use capsule_server::attestation::AttestationContext;
    use capsule_server::drop::DropContext;
    use capsule_server::upload::UploadContext;
    use kynos::di::Provides as _;

    let app = fixture.app();
    let upload: UploadContext = app.provide();
    let attestation: AttestationContext = app.provide();
    let drops: DropContext = app.provide();
    let full = capsule_server::upload::finalize::full_unclaimed(upload.sessions())
        .await
        .expect("the store lists its sessions");
    let albums =
        capsule_server::upload::finalize::finish_unclaimed(&upload, &attestation, full.clone())
            .await;
    let deposits = capsule_server::drop::deposit::finish_unclaimed(&drops, full).await;
    (albums, deposits)
}

#[tokio::test]
async fn a_full_drop_whose_process_died_before_its_claim_is_deposited_at_boot() {
    use capsule_server::store::UploadId;

    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let first = payload(b'u', 4096);
    let last = payload(b'v', 4096);
    let both: Vec<u8> = first.iter().chain(last.iter()).copied().collect();
    let (id, upload_id) = open_drop(&fixture, &bearer, 63, &both).await;
    chunk_at(&fixture, &id, &upload_id, 0, &first)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    record_without_depositing(&fixture, &upload_id, 4096, &last).await;
    // A second drop still short of its size: a guest's transfer in progress, not a finish.
    let pending = payload(b'w', 8192);
    let (_, in_progress) = open_drop(&fixture, &bearer, 64, &pending).await;

    // No guest replays a chunk: the next boot is what files the drop.
    let (albums, deposits) = finish_unclaimed_at_boot(&fixture).await;
    assert_eq!(
        albums,
        capsule_server::upload::finalize::Recovery::default(),
        "the album finisher leaves a drop alone"
    );
    assert_eq!(
        deposits
            .completed
            .iter()
            .map(UploadId::as_str)
            .collect::<Vec<_>>(),
        vec![upload_id.as_str()],
        "{deposits:?}"
    );
    assert!(deposits.failed.is_empty(), "{deposits:?}");
    assert_eq!(terminal_status(&fixture, &upload_id).await, "completed");
    assert_one_drop(&fixture, &bearer, &both).await;
    assert!(
        !fixture
            .uploads
            .read_for_test(&in_progress)
            .await
            .expect("the transfer is live")
            .status
            .is_terminal(),
        "a drop still short of its size is left to its guest"
    );

    // A second boot finds nothing to finish, and files nothing twice.
    let (albums, deposits) = finish_unclaimed_at_boot(&fixture).await;
    assert_eq!(
        (albums, deposits),
        Default::default(),
        "nothing is left full and unclaimed"
    );
    assert_one_drop(&fixture, &bearer, &both).await;
}

#[tokio::test]
async fn a_drop_deposit_whose_process_died_is_recovered_at_boot_and_never_finalized_as_an_album_upload()
 {
    use capsule_server::blob::BlobStore as _;
    use capsule_server::store::UploadId;

    for committed in [false, true] {
        let fixture = Fixture::working();
        let bearer = fixture.bearer().await;
        let first = payload(b'b', 4096);
        let last = payload(b'c', 4096);
        let both: Vec<u8> = first.iter().chain(last.iter()).copied().collect();
        let (id, upload_id) = open_drop(&fixture, &bearer, 62, &both).await;
        chunk_at(&fixture, &id, &upload_id, 0, &first)
            .await
            .assert_status(StatusCode::NO_CONTENT);
        // Full and claimed, and — when `committed` — already at its address: the session a
        // process that died mid-deposit leaves.
        record_without_depositing(&fixture, &upload_id, 4096, &last).await;
        fixture.uploads.claim_for_test(&upload_id).await;
        if committed {
            let address = ContentAddress::parse(&checksum(&both)).expect("an address");
            fixture
                .blobs
                .commit(&UploadId::new(&upload_id), &address)
                .await
                .expect("the stage commits");
        }

        let (albums, deposits) = recover_at_boot(&fixture).await;
        assert_eq!(
            albums,
            capsule_server::upload::finalize::Recovery::default(),
            "the album recovery leaves a drop's claim alone (committed: {committed})"
        );
        assert_eq!(
            deposits
                .completed
                .iter()
                .map(UploadId::as_str)
                .collect::<Vec<_>>(),
            vec![upload_id.as_str()],
            "the drop recovery files it (committed: {committed}): {deposits:?}"
        );
        assert!(deposits.failed.is_empty(), "{deposits:?}");
        assert_eq!(terminal_status(&fixture, &upload_id).await, "completed");
        assert_one_drop(&fixture, &bearer, &both).await;

        // A second boot finds nothing left to recover.
        let (albums, deposits) = recover_at_boot(&fixture).await;
        assert_eq!(
            albums,
            capsule_server::upload::finalize::Recovery::default()
        );
        assert_eq!(
            deposits,
            capsule_server::upload::finalize::Recovery::default()
        );
    }
}

/// A deposit that filed its drop and then lost the status write is not failed: the drop landed.
/// The claim is left for boot recovery, which finds the drop filed and completes the session
/// without filing it twice.
#[tokio::test]
async fn a_filed_drop_whose_status_write_was_lost_is_completed_at_boot_not_failed() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'w', 4096);
    let (id, upload_id) = open_drop(&fixture, &bearer, 65, &bytes).await;

    fixture.uploads.refuse_status_writes(true);
    chunk_at(&fixture, &id, &upload_id, 0, &bytes)
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    fixture.uploads.refuse_status_writes(false);
    assert_eq!(
        fixture
            .uploads
            .read_for_test(&upload_id)
            .await
            .expect("the session is live")
            .status
            .as_str(),
        "waiting_for_processing",
        "the claim outlived its depositor"
    );
    assert_one_drop(&fixture, &bearer, &bytes).await;

    let (albums, deposits) = recover_at_boot(&fixture).await;
    assert_eq!(
        albums,
        capsule_server::upload::finalize::Recovery::default()
    );
    assert_eq!(deposits.completed.len(), 1, "{deposits:?}");
    assert!(deposits.failed.is_empty(), "{deposits:?}");
    assert_eq!(terminal_status(&fixture, &upload_id).await, "completed");
    assert_one_drop(&fixture, &bearer, &bytes).await;
}

/// The link's file and byte totals.
async fn link_totals(fixture: &Fixture, id: &str) -> (u32, u64) {
    use capsule_server::drop::DropStore as _;

    let link = fixture
        .dropstore
        .resolve(id)
        .await
        .expect("the drop store answers")
        .expect("the link is provisioned");
    (link.used_files, link.used_bytes)
}

/// A filing whose statement committed but whose answer was lost is a filed drop: the guest is
/// told it landed, the session completes, and the link keeps the reservation the drop spent —
/// a `500` here would send the guest to upload it a second time.
#[tokio::test]
async fn a_filing_whose_answer_was_lost_is_a_filed_drop_not_a_failed_one() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'l', 4096);
    let (id, upload_id) = open_drop(&fixture, &bearer, 66, &bytes).await;

    fixture
        .dropstore
        .fault_next_file(support::FileFault::AnswerLost);
    chunk_at(&fixture, &id, &upload_id, 0, &bytes)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    assert_eq!(terminal_status(&fixture, &upload_id).await, "completed");
    assert_one_drop(&fixture, &bearer, &bytes).await;
    assert_eq!(
        link_totals(&fixture, &id).await,
        (1, 4096),
        "a drop that landed keeps its reservation"
    );
}

/// A filing that really did not happen is the failure it reports: the session is terminal, the
/// inbox is empty, and the link gets its reservation back.
#[tokio::test]
async fn a_filing_that_did_not_happen_fails_the_drop_and_gives_the_link_back() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'n', 4096);
    let (id, upload_id) = open_drop(&fixture, &bearer, 67, &bytes).await;

    fixture
        .dropstore
        .fault_next_file(support::FileFault::Refused);
    chunk_at(&fixture, &id, &upload_id, 0, &bytes)
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        terminal_status(&fixture, &upload_id).await,
        "failed_processing"
    );
    assert!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .is_empty()
    );
    assert_eq!(link_totals(&fixture, &id).await, (0, 0));
}

/// The link owner holds a drop's session as its uploader, but the album surface is not where a
/// drop finishes: a chunk sent there is answered as an unknown session, writes nothing, and the
/// guest's own chunk still deposits the drop.
#[tokio::test]
async fn a_drop_session_is_unknown_to_the_album_chunk_path() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'a', 4096);
    let (id, upload_id) = open_drop(&fixture, &bearer, 64, &bytes).await;

    let refused = fixture.chunk(&upload_id, 0, &bytes, &bearer).send().await;
    refused.assert_status(StatusCode::NOT_FOUND);
    assert_eq!(
        refused.json::<Value>()["code"],
        "error.upload.session_not_found"
    );
    assert_eq!(
        fixture.blobs.staged_len_for_test(&upload_id).await,
        Some(0),
        "nothing landed on the drop's stage"
    );

    chunk_at(&fixture, &id, &upload_id, 0, &bytes)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    assert_eq!(terminal_status(&fixture, &upload_id).await, "completed");
    assert_one_drop(&fixture, &bearer, &bytes).await;
}

#[tokio::test]
async fn a_drop_whose_bytes_are_not_its_declared_hash_is_refused_and_gives_the_link_back() {
    use capsule_server::drop::DropStore as _;

    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let declared = payload(b'h', 4096);
    let sent = payload(b'x', 4096);
    let (id, upload_id) = open_drop(&fixture, &bearer, 63, &declared).await;
    let charged = fixture
        .dropstore
        .resolve(&id)
        .await
        .expect("the drop store answers")
        .expect("the link is provisioned");
    assert_eq!(charged.used_files, 1, "opening the drop reserved a file");

    // Each chunk is its own checksum, so the chunk lands; the whole is not the declared blob.
    chunk_at(&fixture, &id, &upload_id, 0, &sent)
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        terminal_status(&fixture, &upload_id).await,
        "failed_processing"
    );

    let declared_address = ContentAddress::parse(&checksum(&declared)).expect("an address");
    let sent_address = ContentAddress::parse(&checksum(&sent)).expect("an address");
    assert!(
        fixture
            .blobs
            .stat(&declared_address)
            .await
            .expect("stat")
            .is_none(),
        "nothing was placed at the address the guest declared"
    );
    assert!(
        fixture
            .blobs
            .stat(&sent_address)
            .await
            .expect("stat")
            .is_none(),
        "nor anywhere else"
    );
    assert_eq!(fixture.blobs.staged_len_for_test(&upload_id).await, None);
    assert!(
        inbox(&fixture, &bearer).await["drops"]
            .as_array()
            .expect("drops")
            .is_empty(),
        "a refused drop files nothing"
    );
    let refunded = fixture
        .dropstore
        .resolve(&id)
        .await
        .expect("the drop store answers")
        .expect("the link is provisioned");
    assert_eq!(
        (refunded.used_files, refunded.used_bytes),
        (0, 0),
        "a drop that never landed does not spend the link"
    );
}
