//! Guest drops, end to end (slice `S-C5`).
//!
//! Three cases carry the slice. `a_guest_drop_lands_in_the_owners_inbox_and_never_in_an_album`
//! is invariant 30's absence clause: nothing a guest sends can put bytes into an album.
//! `a_links_caps_are_spent_once_per_drop_and_refunded_when_one_is_refused` is invariant 26 —
//! the caps are the whole authorization model on a path with no credential.
//! `an_adoption_that_is_refused_returns_the_drop_to_the_inbox` is invariant 32's honest half: a
//! failed promotion must lose nothing and duplicate nothing.

mod support;

use std::net::SocketAddr;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
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

/// A full disk under a guest's chunk keeps the guest surface's one storage answer, `500
/// error.drop.unavailable`, rather than the album path's `507` (the drop wire contract is
/// unchanged). Nothing of the refused chunk was kept, so the same chunk at the same offset lands.
#[tokio::test]
async fn a_full_disk_under_a_guest_chunk_is_unavailable_and_the_retry_lands() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let bytes = payload(b'f', 4096);
    let id = opaque(40);
    provision(
        &fixture,
        &bearer,
        &link_body(&id, json!({})),
        StatusCode::CREATED,
    )
    .await;
    let opened = create_drop(&fixture, &id, &bytes, StatusCode::CREATED).await;
    let upload_id = opened["upload_id"].as_str().expect("a session id");

    fixture.blobs.fill_disk_on_next_append();
    let problem: Value = fixture
        .client
        .patch(&format!("/d/{id}/{upload_id}"))
        .header("x-capsule-offset", "0")
        .header("x-capsule-checksum", &checksum(&bytes))
        .body("application/octet-stream", bytes.clone())
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .json();
    assert_eq!(problem["code"], "error.drop.unavailable");
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
    fixture
        .client
        .post(&format!("/v1/drops/{drop_id}/adopt"))
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(&json!({
            "album_id": album().as_str(),
            "asset_id": ADOPTED,
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
// The passphrase abuse gate (slice `S-C61`)
// ===========================================================================================

#[tokio::test]
async fn a_gated_link_refuses_a_drop_with_no_proof() {
    // The case the slice exists for. The gate was provisioned and never checked, so a
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
