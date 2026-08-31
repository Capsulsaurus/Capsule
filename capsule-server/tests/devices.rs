//! The session ledger (slices `S-C13`, `S-N3`), end to end.
//!
//! Two cases carry the slices. `a_reinstall_groups_with_the_device_it_replaced` is the whole
//! point of cohorts: reinstalling re-enrolls with a **new** `device_id` by design, so one
//! physical phone accumulates ledger entries the user cannot tell apart, and the cohort is what
//! groups them. `the_cohort_map_outlives_the_sessions_that_carried_it` is why the map is durable
//! — a session store forgets a cohort exactly when "have I seen this device before?" starts
//! being worth asking.

mod support;

use jiff::SignedDuration;
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{EMAIL, Fixture, PASSWORD};

/// Sign in, asserting the status, with optional advisory identifiers.
async fn login(
    fixture: &Fixture,
    cohort: Option<&str>,
    device: Option<&str>,
) -> capsule_server::routes::auth::TokenResponse {
    let mut body = json!({ "email": EMAIL, "password": PASSWORD });
    if let Some(cohort) = cohort {
        body["cohort_hash"] = json!(cohort);
    }
    if let Some(device) = device {
        body["device_id"] = json!(device);
    }
    fixture
        .client
        .post("/v1/auth/login")
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json()
}

/// The ledger as the caller sees it.
async fn ledger(fixture: &Fixture, bearer: &str) -> Value {
    fixture
        .client
        .get("/v1/auth/devices")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json()
}

fn bearer(issued: &capsule_server::routes::auth::TokenResponse) -> String {
    format!("Bearer {}", issued.access_token)
}

#[tokio::test]
async fn the_ledger_lists_every_live_session_and_marks_the_current_one() {
    let fixture = Fixture::working();
    let first = login(&fixture, None, None).await;
    let second = login(&fixture, None, None).await;

    let body = ledger(&fixture, &bearer(&second)).await;
    let sessions = body["sessions"].as_array().expect("a sessions array");
    assert_eq!(sessions.len(), 2);

    let current: Vec<bool> = sessions
        .iter()
        .map(|s| s["current"].as_bool().expect("a current flag"))
        .collect();
    assert_eq!(
        current.iter().filter(|c| **c).count(),
        1,
        "exactly one session is the caller's own: {current:?}"
    );

    // Asserted through the *other* token, so "current" tracks the credential rather than
    // whichever session happens to be newest.
    let body = ledger(&fixture, &bearer(&first)).await;
    let sessions = body["sessions"].as_array().expect("a sessions array");
    let marked = sessions
        .iter()
        .find(|s| s["current"] == json!(true))
        .expect("one current session");
    assert_ne!(
        marked["session_id"],
        ledger(&fixture, &bearer(&second)).await["sessions"]
            .as_array()
            .expect("sessions")
            .iter()
            .find(|s| s["current"] == json!(true))
            .expect("one current session")["session_id"]
    );
}

#[tokio::test]
async fn a_reinstall_groups_with_the_device_it_replaced() {
    // The reason cohorts exist. A reinstall re-enrolls with a new `device_id` — device keys are
    // hardware-bound and non-exportable — so the ledger shows two entries for one phone, and
    // only the cohort says they are the same physical device.
    let fixture = Fixture::working();
    let before = login(
        &fixture,
        Some("cohort-phone"),
        Some("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f"),
    )
    .await;
    let after = login(
        &fixture,
        Some("cohort-phone"),
        Some("018f3f1e-4b7a-7c9d-8e2f-000000000002"),
    )
    .await;

    let body = ledger(&fixture, &bearer(&after)).await;
    let sessions = body["sessions"].as_array().expect("a sessions array");
    let devices: Vec<&str> = sessions
        .iter()
        .filter_map(|s| s["device_id"].as_str())
        .collect();
    assert_eq!(
        devices.len(),
        2,
        "two ledger entries, because the reinstall enrolled a new directory device"
    );
    assert_ne!(devices[0], devices[1]);

    let cohorts: Vec<&str> = sessions
        .iter()
        .filter_map(|s| s["cohort_hash"].as_str())
        .collect();
    assert_eq!(
        cohorts,
        ["cohort-phone", "cohort-phone"],
        "and one cohort, which is the only thing that says they are one phone"
    );

    // One row in the durable map, not two — a cohort is a fact about a device, not an event.
    let map = body["cohorts"].as_array().expect("a cohorts array");
    assert_eq!(map.len(), 1);
    assert_eq!(map[0]["cohort_hash"], "cohort-phone");
    let _ = before;
}

#[tokio::test]
async fn the_cohort_map_outlives_the_sessions_that_carried_it() {
    // The reason the map is durable rather than a projection of the session store. The user
    // reinstalls months later; every session that named the old cohort has expired, and the
    // "you have used this device before" answer has to survive that.
    let fixture = Fixture::working();
    login(&fixture, Some("cohort-old"), None).await;

    // Past the session TTL, so nothing of the original sign-in is left in the session store.
    fixture.clock.advance(SignedDuration::from_hours(24 * 8));
    let fresh = login(&fixture, Some("cohort-old"), None).await;

    let body = ledger(&fixture, &bearer(&fresh)).await;
    assert_eq!(
        body["sessions"].as_array().expect("sessions").len(),
        1,
        "the old session expired"
    );

    let map = body["cohorts"].as_array().expect("cohorts");
    assert_eq!(map.len(), 1, "the cohort did not");
    assert_eq!(
        map[0]["first_seen"], "1970-01-01T00:00:00Z",
        "and it still remembers when this device was first seen, which is the whole answer"
    );
    assert_ne!(
        map[0]["last_seen"], map[0]["first_seen"],
        "while last_seen moved with the new sighting"
    );
}

#[tokio::test]
async fn an_absent_or_malformed_cohort_behaves_exactly_like_a_valid_one() {
    // Advisory-only, structurally. A server that received garbage must behave identically to one
    // that received a valid value — otherwise the field becomes something an attacker can act
    // through.
    let fixture = Fixture::working();
    let none = login(&fixture, None, None).await;
    let blank = login(&fixture, Some("   "), None).await;
    let long = login(&fixture, Some(&"x".repeat(4096)), None).await;

    for issued in [&none, &blank, &long] {
        let body = ledger(&fixture, &bearer(issued)).await;
        assert!(
            body["cohorts"].as_array().expect("cohorts").is_empty(),
            "nothing unusable reached the durable map"
        );
    }
    let body = ledger(&fixture, &bearer(&none)).await;
    for session in body["sessions"].as_array().expect("sessions") {
        assert!(
            session.get("cohort_hash").is_none(),
            "an absent cohort is absent on the wire, not a present null"
        );
    }
}

#[tokio::test]
async fn revoking_a_session_ends_it_and_leaves_the_others() {
    let fixture = Fixture::working();
    let keep = login(&fixture, None, None).await;
    let drop = login(&fixture, None, None).await;

    let target = ledger(&fixture, &bearer(&drop)).await["sessions"]
        .as_array()
        .expect("sessions")
        .iter()
        .find(|s| s["current"] == json!(true))
        .expect("the current session")["session_id"]
        .as_str()
        .expect("a session id")
        .to_owned();

    fixture
        .client
        .delete(&format!("/v1/auth/devices/{target}"))
        .header("authorization", &bearer(&keep))
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    let body = ledger(&fixture, &bearer(&keep)).await;
    assert_eq!(body["sessions"].as_array().expect("sessions").len(), 1);

    // The revoked session's refresh token is dead — that is the half a revoke is immediate
    // about, and the half `S-C48` does not cover.
    fixture
        .client
        .post("/v1/auth/refresh")
        .json(&json!({ "refresh_token": drop.refresh_token }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn another_accounts_session_is_refused_and_not_closed() {
    // The ownership check is against the record the store returns, so there is no window in
    // which a caller ends somebody else's session and is then told they were not allowed to.
    let fixture = Fixture::working();
    let mine = login(&fixture, None, None).await;
    let stranger = fixture.other_bearer("01937b7c-0000-7000-8000-0000000000ff");

    let target = ledger(&fixture, &bearer(&mine)).await["sessions"]
        .as_array()
        .expect("sessions")[0]["session_id"]
        .as_str()
        .expect("a session id")
        .to_owned();

    let problem: Value = fixture
        .client
        .delete(&format!("/v1/auth/devices/{target}"))
        .header("authorization", &stranger)
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .json();
    assert_eq!(
        problem["code"], "error.auth.session_not_found",
        "one answer for unknown and for somebody else's, so this is not an oracle"
    );

    assert_eq!(
        ledger(&fixture, &bearer(&mine)).await["sessions"]
            .as_array()
            .expect("sessions")
            .len(),
        1,
        "the refused revoke closed nothing"
    );
}

#[tokio::test]
async fn revoking_the_current_session_is_allowed() {
    // Signing this device out is a legitimate ask, and refusing it would only push a client
    // into calling `logout` and hoping the two behave the same.
    let fixture = Fixture::working();
    let only = login(&fixture, None, None).await;
    let target = ledger(&fixture, &bearer(&only)).await["sessions"]
        .as_array()
        .expect("sessions")[0]["session_id"]
        .as_str()
        .expect("a session id")
        .to_owned();

    fixture
        .client
        .delete(&format!("/v1/auth/devices/{target}"))
        .header("authorization", &bearer(&only))
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    fixture
        .client
        .post("/v1/auth/refresh")
        .json(&json!({ "refresh_token": only.refresh_token }))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn the_devices_surface_requires_a_credential() {
    let fixture = Fixture::working();
    fixture
        .client
        .get("/v1/auth/devices")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    fixture
        .client
        .delete("/v1/auth/devices/anything")
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}
