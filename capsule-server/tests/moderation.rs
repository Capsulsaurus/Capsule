//! Moderation, end to end (slice `S-C8`).
//!
//! The case that carries the slice is
//! `a_suspended_account_is_told_why_and_keeps_every_recovery_path`: design/moderation.md's
//! structural rule is that there are **no silent operations**, and its structural *limit* is
//! that moderation is access-level and never data-level. Both are asserted in one place because
//! a suspension that violated either would still look like it worked.

mod support;

use capsule_server::moderation::{ModerationAction, ModerationEvent, ModerationStore, Standing};
use capsule_server::store::{Clock, UserId};
use jiff::Timestamp;
use kynos::http::StatusCode;
use serde_json::Value;
use support::{Fixture, PROTOCOL_VERSION, checksum, payload, user};

/// Read the caller's own moderation record.
async fn record(fixture: &Fixture, bearer: &str, expect: StatusCode) -> Value {
    fixture
        .client
        .get("/v1/moderation/record")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(expect)
        .json()
}

/// Attempt an upload session.
async fn upload(fixture: &Fixture, bearer: &str, expect: StatusCode) -> kynos::test::TestResponse {
    let bytes = payload(b'a', 4096);
    let response = fixture
        .client
        .post("/v1/upload")
        .header("authorization", bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .header("accept", "application/json")
        .json(&support::create_request(&fixture.clock, &bytes, "original"))
        .send()
        .await;
    response.assert_status(expect);
    let _ = checksum(&bytes);
    response
}

/// Suspend the seeded account through the port, the way an operator binary would.
async fn suspend(fixture: &Fixture, reason: Option<&str>) {
    let since = fixture.clock.now();
    fixture
        .moderation
        .apply(
            ModerationEvent {
                user_id: UserId::new(user().as_str()),
                action: ModerationAction::Suspended,
                asset_id: None,
                at: since,
                reason: reason.map(str::to_owned),
            },
            Some(Standing::Suspended { since }),
        )
        .await
        .expect("the moderation store applies");
}

#[tokio::test]
async fn an_active_account_uploads_and_its_record_is_empty() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;

    upload(&fixture, &bearer, StatusCode::CREATED).await;

    let body = record(&fixture, &bearer, StatusCode::OK).await;
    assert_eq!(body["standing"], "active");
    assert!(body.get("suspended_since").is_none());
    assert_eq!(body["events"].as_array().expect("events").len(), 0);
}

#[tokio::test]
async fn a_suspended_account_is_told_why_and_keeps_every_recovery_path() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    suspend(&fixture, Some("verified report")).await;

    // Cannot upload, and the refusal is its own code — a client sends a suspended user to a
    // different screen than a quota-exceeded or permission-denied one.
    let problem: Value = upload(&fixture, &bearer, StatusCode::FORBIDDEN)
        .await
        .json();
    assert_eq!(problem["code"], "error.moderation.account_suspended");

    // And is never left to guess why.
    let body = record(&fixture, &bearer, StatusCode::OK).await;
    assert_eq!(body["standing"], "suspended");
    assert!(body["suspended_since"].is_string());
    let events = body["events"].as_array().expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["action"], "suspended");
    assert_eq!(events[0]["reason"], "verified report");

    // Access-level, never data-level. Reading the library still works — the user's data is
    // untouched, which is the limit the contract puts on what moderation may do.
    fixture
        .client
        .get("/v1/sync")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);
    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_suspended_user_can_still_sign_out_everywhere() {
    // Explicitly in the contract, and the reason is sharp: `revoke_all_sessions` is gated by
    // master-key proof rather than by account standing, and a suspended user whose account may
    // also be compromised needs it most. A suspension that took it away would hand an attacker
    // the account the moment an admin acted.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    suspend(&fixture, None).await;

    fixture
        .client
        .post("/v1/auth/logout/all/challenge")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_reinstatement_restores_writing_and_keeps_the_history() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    suspend(&fixture, Some("mistake")).await;
    upload(&fixture, &bearer, StatusCode::FORBIDDEN).await;

    fixture
        .moderation
        .apply(
            ModerationEvent {
                user_id: UserId::new(user().as_str()),
                action: ModerationAction::Reinstated,
                asset_id: None,
                at: fixture.clock.now(),
                reason: Some("appeal granted".to_owned()),
            },
            Some(Standing::Active),
        )
        .await
        .expect("the moderation store applies");

    upload(&fixture, &bearer, StatusCode::CREATED).await;

    let body = record(&fixture, &bearer, StatusCode::OK).await;
    assert_eq!(body["standing"], "active");
    let events = body["events"].as_array().expect("events");
    assert_eq!(
        events.len(),
        2,
        "a reinstatement lifts the suspension and does not erase it"
    );
    assert_eq!(events[1]["action"], "reinstated");
}

#[tokio::test]
async fn a_takedown_appears_in_the_owners_record_naming_the_asset() {
    // `S-C17` refuses the bytes; this is the half it owed — the user finds out which asset and
    // when, rather than discovering that a photo silently stopped loading.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture
        .moderation
        .apply(
            ModerationEvent {
                user_id: UserId::new(user().as_str()),
                action: ModerationAction::TakenDown,
                asset_id: Some(capsule_server::store::AssetId::new("asset-9")),
                at: Timestamp::UNIX_EPOCH,
                reason: None,
            },
            None,
        )
        .await
        .expect("the moderation store applies");

    let body = record(&fixture, &bearer, StatusCode::OK).await;
    assert_eq!(
        body["standing"], "active",
        "a takedown is about one asset and must not suspend the account"
    );
    let events = body["events"].as_array().expect("events");
    assert_eq!(events[0]["action"], "taken_down");
    assert_eq!(events[0]["asset_id"], "asset-9");
    assert!(
        events[0].get("reason").is_none(),
        "absent is a real answer: a legal hold may come with an obligation not to disclose it"
    );
}

#[tokio::test]
async fn one_accounts_record_is_not_another_s() {
    let fixture = Fixture::working();
    suspend(&fixture, Some("theirs")).await;
    let stranger = fixture.other_bearer("01937b7c-0000-7000-8000-0000000000ff");

    let body = record(&fixture, &stranger, StatusCode::OK).await;
    assert_eq!(body["standing"], "active");
    assert_eq!(body["events"].as_array().expect("events").len(), 0);
}

#[tokio::test]
async fn an_unreachable_moderation_store_refuses_the_upload_rather_than_admitting_it() {
    // Fail closed. A store that cannot answer "is this account suspended" must not be read as
    // "no", or an outage becomes a window in which every suspension is lifted.
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture.moderation.set_unavailable(true);

    upload(&fixture, &bearer, StatusCode::INTERNAL_SERVER_ERROR).await;
    record(&fixture, &bearer, StatusCode::INTERNAL_SERVER_ERROR).await;
}

#[tokio::test]
async fn the_moderation_record_requires_a_credential() {
    let fixture = Fixture::working();
    fixture
        .client
        .get("/v1/moderation/record")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
