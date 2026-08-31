//! Guest drops, end to end (slice `S-C5`).
//!
//! Three cases carry the slice. `a_guest_drop_lands_in_the_owners_inbox_and_never_in_an_album`
//! is invariant 30's absence clause: nothing a guest sends can put bytes into an album.
//! `a_links_caps_are_spent_once_per_drop_and_refunded_when_one_is_refused` is invariant 26 —
//! the caps are the whole authorization model on a path with no credential.
//! `an_adoption_that_is_refused_returns_the_drop_to_the_inbox` is invariant 32's honest half: a
//! failed promotion must lose nothing and duplicate nothing.

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_server::blob::{BlobStore, ContentAddress};
use jiff::SignedDuration;
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{Fixture, PROTOCOL_VERSION, album, checksum, payload, user};

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
        "crypto_suite_id": capsule_core::crypto::CRYPTO_SUITE_ID,
    });
    for (key, value) in caps.as_object().expect("caps is an object") {
        body[key] = value.clone();
    }
    body
}

/// Open a drop session through a link.
async fn create_drop(fixture: &Fixture, id: &str, bytes: &[u8], expect: StatusCode) -> Value {
    fixture
        .client
        .post(&format!("/d/{id}"))
        .header("accept", "application/json")
        .json(&json!({
            "content_type": "image/jpeg",
            "size": bytes.len(),
            "ciphertext_hash": checksum(bytes),
            "kem_ct": BASE64.encode([9_u8; 64]),
            "suggested_filename": "from-a-guest.jpg",
        }))
        .send()
        .await
        .assert_status(expect)
        .json()
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

    let stranger = fixture.other_bearer("01937b7c-0000-7000-8000-0000000000ff");
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
    let stranger = fixture.other_bearer("01937b7c-0000-7000-8000-0000000000ff");

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
    let stranger = fixture.other_bearer("01937b7c-0000-7000-8000-0000000000ff");

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
            "asset_id": "adopted-asset-1",
            "size": bytes.len(),
            "hash": checksum(&bytes),
            "content_type": "image/jpeg",
            "crypto_suite_id": capsule_core::crypto::CRYPTO_SUITE_ID,
            "protocol_version": PROTOCOL_VERSION,
            "key_mode": "wrapped",
            "manifest_envelope": manifest["manifest_envelope"],
        }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();

    assert_eq!(body["asset_id"], "adopted-asset-1");
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
            "asset_id": "adopted-asset-1",
            "size": bytes.len(),
            "hash": checksum(&bytes),
            "content_type": "image/jpeg",
            "crypto_suite_id": capsule_core::crypto::CRYPTO_SUITE_ID,
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
            "asset_id": "adopted-asset-1",
            "size": other.len(),
            "hash": checksum(&other),
            "content_type": "image/jpeg",
            "crypto_suite_id": capsule_core::crypto::CRYPTO_SUITE_ID,
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
    let stranger = fixture.other_bearer("01937b7c-0000-7000-8000-0000000000ff");
    let manifest = support::create_request(&fixture.clock, &bytes, "original");

    fixture
        .client
        .post(&format!("/v1/drops/{drop_id}/adopt"))
        .header("authorization", &stranger)
        .header("accept", "application/json")
        .json(&json!({
            "album_id": album().as_str(),
            "asset_id": "adopted-asset-1",
            "size": bytes.len(),
            "hash": checksum(&bytes),
            "content_type": "image/jpeg",
            "crypto_suite_id": capsule_core::crypto::CRYPTO_SUITE_ID,
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
