//! The album-upgrade ceremony's server halves (slice `S-C24`), end to end.
//!
//! The ceremony itself is a client ceremony carried on MLS application messages the server
//! cannot read. Four of its steps are the server's, and each one is the server's *because the
//! clients cannot do it themselves*: the deadline is evaluated on a clock no member can skew, a
//! v_old client that never saw the proposal is exactly the party that will not stop writing on
//! its own, only the server knows how many sessions are still in flight, and lineage rides a
//! manifest the server stores.
//!
//! What the server must **not** do is adjudicate the `frozen_state_hash`. There is no surface
//! here that could carry one, and that absence is deliberate: the ceremony's hostile-member
//! defence is that every member checks it independently.

mod support;

use capsule_core::crypto::keys::HybridSigningKey;
use capsule_server::store::Clock as _;
use jiff::SignedDuration;
use kynos::http::StatusCode;
use serde_json::Value;
use support::{
    Fixture, PROTOCOL_VERSION, album, device, identity_header, identity_key, payload,
    signed_directory_with_device, signed_upgrade_intent,
};
use uuid::Uuid;

/// The ceremony this file proposes.
fn intent_id() -> Uuid {
    Uuid::parse_str("019a0000-0000-7000-8000-00000000cafe").expect("a uuid")
}

/// Publish a directory holding the seeded device with `dsk` as its signing key.
async fn anchor(fixture: &Fixture, bearer: &str, ik: &HybridSigningKey, dsk: &HybridSigningKey) {
    fixture
        .client
        .post("/v1/auth/devices/directory")
        .header("authorization", bearer)
        .header("x-capsule-identity-key", &identity_header(ik))
        .body(
            "application/cbor",
            signed_directory_with_device(ik, 1, device(), dsk, "1970-01-01T00:00:00Z"),
        )
        .send()
        .await
        .assert_status(StatusCode::OK);
}

/// Provision the seeded album so it exists to upgrade.
async fn provision(fixture: &Fixture, bearer: &str) {
    fixture
        .client
        .post("/v1/albums")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(&serde_json::json!({ "album_id": album().as_str() }))
        .send()
        .await
        .assert_status(StatusCode::CREATED);
}

/// Propose an upgrade and return the response.
async fn propose(
    fixture: &Fixture,
    bearer: &str,
    dsk: &HybridSigningKey,
    intent: Uuid,
    deadline_secs: u64,
) -> kynos::test::TestResponse {
    fixture
        .client
        .post(&format!("/v1/albums/{}/upgrade", album()))
        .header("authorization", bearer)
        .header("accept", "application/json")
        .body(
            "application/cbor",
            signed_upgrade_intent(dsk, device(), intent, "2030-01-01", deadline_secs),
        )
        .send()
        .await
}

/// Read the ceremony's phase.
async fn phase(fixture: &Fixture, bearer: &str) -> Value {
    fixture
        .client
        .get(&format!("/v1/albums/{}/upgrade", album()))
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json()
}

// ===========================================================================================

#[tokio::test]
async fn a_signed_proposal_quiesces_the_album_and_the_deadline_is_the_servers() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let ik = identity_key();
    let dsk = identity_key();
    anchor(&fixture, &bearer, &ik, &dsk).await;
    provision(&fixture, &bearer).await;

    let began = fixture.clock.now();
    let body: Value = propose(&fixture, &bearer, &dsk, intent_id(), 300)
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(body["intent_id"], intent_id().to_string());
    assert_eq!(body["to_protocol_version"], "2030-01-01");
    assert_eq!(
        body["expires_at"],
        began
            .checked_add(SignedDuration::from_secs(300))
            .expect("a representable deadline")
            .to_string(),
        "the window is `received_at + deadline` on the server's own clock, which is the entire \
         reason the deadline is a duration rather than an instant"
    );
    assert_eq!(body["in_flight"], 0);

    // The window closes on its own. Nothing sweeps it: an expired ceremony is *absent*
    // everywhere, which is what stops a proposer who vanished from freezing an album forever.
    // Just past the window, and deliberately *inside* the access token's own fifteen minutes:
    // this case is about the ceremony expiring, not about the credential.
    fixture.clock.advance(SignedDuration::from_secs(301));
    let expired = phase(&fixture, &bearer).await;
    assert!(
        expired.get("intent_id").is_none(),
        "an expired ceremony reports as no ceremony: {expired}"
    );
}

#[tokio::test]
async fn a_quiescing_album_refuses_a_write_that_does_not_name_the_ceremony() {
    // Versioning step 2, and the whole reason the server is in this ceremony at all: a v_old
    // client that never received the `UpgradeIntent` is exactly the party that will keep writing.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let ik = identity_key();
    let dsk = identity_key();
    anchor(&fixture, &bearer, &ik, &dsk).await;
    provision(&fixture, &bearer).await;
    fixture
        .authority
        .quiesce_album(&support::owner(), &album(), intent_id());

    let bytes = payload(b'q', 4096);
    let mut body = support::create_request(&fixture.clock, &bytes, "original");
    let refused = fixture
        .client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("accept", "application/problem+json")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&body)
        .send()
        .await;
    refused.assert_status(StatusCode::CONFLICT);
    let problem: Value = refused.json();
    assert_eq!(problem["code"], "error.upload.album_quiescing");
    assert_eq!(
        problem["intent_id"],
        intent_id().to_string(),
        "a client that *is* participating has to be able to tell 'wrong ticket' from 'somebody \
         else's upgrade', which are different bugs"
    );

    // The ceremony's own writes go through, which is what makes quiescence a filter rather than
    // a freeze: in-flight work reaches a terminal state instead of being abandoned.
    body["intent_id"] = intent_id().to_string().into();
    fixture
        .client
        .post("/v1/upload")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&body)
        .send()
        .await
        .assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn the_drain_count_is_what_the_proposer_waits_on() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let ik = identity_key();
    let dsk = identity_key();
    anchor(&fixture, &bearer, &ik, &dsk).await;
    provision(&fixture, &bearer).await;

    // A session opened before the ceremony — the in-flight write step 3 waits for.
    let bytes = payload(b'd', 8192);
    let upload = fixture.open_session(&bytes, "original", &bearer).await;

    propose(&fixture, &bearer, &dsk, intent_id(), 300)
        .await
        .assert_status(StatusCode::OK);
    assert_eq!(
        phase(&fixture, &bearer).await["in_flight"],
        1,
        "the upgrade cannot proceed while a session for this album is still in flight"
    );

    // Draining it is finishing it, not abandoning it: the ceremony lets in-flight uploads reach
    // a terminal state.
    fixture
        .chunk(&upload, 0, &bytes, &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    assert_eq!(phase(&fixture, &bearer).await["in_flight"], 0);
}

#[tokio::test]
async fn only_one_ceremony_may_be_in_flight_and_the_same_one_is_idempotent() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let ik = identity_key();
    let dsk = identity_key();
    anchor(&fixture, &bearer, &ik, &dsk).await;
    provision(&fixture, &bearer).await;

    propose(&fixture, &bearer, &dsk, intent_id(), 300)
        .await
        .assert_status(StatusCode::OK);

    // The same intent again is the proposer retrying a lost acknowledgement. Versioning is
    // explicit that the same `UpgradeIntent` never produces two forks.
    propose(&fixture, &bearer, &dsk, intent_id(), 300)
        .await
        .assert_status(StatusCode::OK);

    let other = Uuid::parse_str("019a0000-0000-7000-8000-0000000000ff").expect("a uuid");
    let conflict = propose(&fixture, &bearer, &dsk, other, 300).await;
    conflict.assert_status(StatusCode::CONFLICT);
    let problem: Value = conflict.json();
    assert_eq!(problem["code"], "error.album.upgrade_in_flight");
    assert_eq!(problem["intent_id"], intent_id().to_string());

    // Once the window closes the album is free again, and a fresh proposal simply replaces the
    // abandoned one rather than conflicting with it.
    // Just past the window, and deliberately *inside* the access token's own fifteen minutes:
    // this case is about the ceremony expiring, not about the credential.
    fixture.clock.advance(SignedDuration::from_secs(301));
    propose(&fixture, &bearer, &dsk, other, 300)
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn an_intent_no_admin_device_signed_is_refused() {
    // Without this check anyone holding an access token could freeze an album by posting a
    // struct, which is the opposite of a ceremony keyed to an admin device.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let ik = identity_key();
    let dsk = identity_key();
    anchor(&fixture, &bearer, &ik, &dsk).await;
    provision(&fixture, &bearer).await;

    let impostor = identity_key();
    let refused = propose(&fixture, &bearer, &impostor, intent_id(), 300).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let problem: Value = refused.json();
    assert_eq!(problem["code"], "error.album.upgrade_proposer");
    assert!(
        phase(&fixture, &bearer).await.get("intent_id").is_none(),
        "a refused proposal leaves the album in normal operation"
    );
}

#[tokio::test]
async fn a_ceremony_can_be_aborted_by_the_id_that_holds_it_and_not_by_another() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let ik = identity_key();
    let dsk = identity_key();
    anchor(&fixture, &bearer, &ik, &dsk).await;
    provision(&fixture, &bearer).await;
    propose(&fixture, &bearer, &dsk, intent_id(), 300)
        .await
        .assert_status(StatusCode::OK);

    let stranger = Uuid::parse_str("019a0000-0000-7000-8000-0000000000aa").expect("a uuid");
    fixture
        .client
        .delete(&format!(
            "/v1/albums/{}/upgrade?intent_id={stranger}",
            album()
        ))
        .header("authorization", &bearer)
        .header("accept", "application/problem+json")
        .send()
        .await
        .assert_status(StatusCode::CONFLICT);

    fixture
        .client
        .delete(&format!(
            "/v1/albums/{}/upgrade?intent_id={}",
            album(),
            intent_id()
        ))
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK);
    assert!(phase(&fixture, &bearer).await.get("intent_id").is_none());
}
