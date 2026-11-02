//! `POST /v1/federation/hints` over the wire: the low-trust channel a home server tells this
//! server an album has news (#1158).
//!
//! What is under test is the channel's contract (`capsule_server::federation::hints`): only a
//! pinned peer's signature admits a hint, a hint is accepted once, inside minutes, addressed
//! here; nothing is charged to a peer before its signature verifies; and a verified hint is
//! answered the same whatever became of it, so it is never a way to learn which albums this
//! server pulls. What a hint does to a job is the pull worker's, and its own suite covers it;
//! here the fixture's recording receiver stands in for the worker.

mod support;

use capsule_server::counter::{CounterKey, CounterStore as _, Verdict, budgets};
use capsule_server::federation::pull::{HintOutcome, JobKey};
use capsule_server::federation::{HintClaim, PeerId, PeerStore as _};
use capsule_server::store::{AlbumId, Clock as _};
use jiff::SignedDuration;
use kynos::http::StatusCode;
use serde_json::Value;
use support::{Fixture, REPORTED_AT};

/// The home server the suite's hints come from.
const PEER: &str = "home.test";

/// The album the suite's hints name, as its home server names it.
const ALBUM: &str = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e60";

/// POST a hint body.
async fn hint(fixture: &Fixture, body: &Value) -> kynos::test::TestResponse {
    fixture
        .client
        .post("/v1/federation/hints")
        .header("accept", "application/json")
        .json(body)
        .send()
        .await
}

/// A fixture with `PEER` pinned under a fresh key, and that key's signer.
async fn pinned() -> (Fixture, ring::signature::Ed25519KeyPair) {
    let fixture = Fixture::working();
    let (pair, public) = support::peer_keypair();
    fixture
        .peers
        .pin(&PeerId::new(PEER), public, fixture.clock.now())
        .await
        .expect("the operator pins");
    (fixture, pair)
}

fn signed(pair: &ring::signature::Ed25519KeyPair) -> Value {
    support::signed_hint(pair, PEER, ALBUM, REPORTED_AT)
}

fn job() -> JobKey {
    JobKey::new(PeerId::new(PEER), AlbumId::new(ALBUM))
}

fn assert_refused(response: &kynos::test::TestResponse, status: StatusCode, code: &str) {
    assert_eq!(response.status(), status, "expected {code}");
    let body: Value = response.json();
    assert_eq!(body["code"], code);
}

/// How much of `PEER`'s hint budget is left in the open window.
async fn hints_left(fixture: &Fixture) -> u32 {
    match fixture
        .counters
        .peek(
            &CounterKey::FederatedHints(PEER.to_owned()),
            budgets::FEDERATED_HINTS,
            fixture.clock.now(),
        )
        .await
        .expect("the counter answers")
    {
        Verdict::Admitted { remaining } => remaining,
        Verdict::Limited { .. } => 0,
    }
}

#[tokio::test]
async fn a_signed_hint_from_a_pinned_home_server_reaches_the_worker_for_that_peers_album() {
    let (fixture, pair) = pinned().await;
    let accepted = hint(&fixture, &signed(&pair)).await;
    accepted.assert_status(StatusCode::ACCEPTED);
    let body: Value = accepted.json();
    let fields: Vec<&String> = body.as_object().expect("an object").keys().collect();
    assert_eq!(
        fields,
        ["received_at"],
        "the answer names nothing about the album"
    );
    assert!(
        body["received_at"]
            .as_str()
            .expect("an instant")
            .parse::<jiff::Timestamp>()
            .is_ok()
    );

    // The job is the signer's: a peer can only ever hint about what this server pulls from it.
    assert_eq!(fixture.hints.received(), vec![job()]);
    assert_eq!(
        hints_left(&fixture).await,
        budgets::FEDERATED_HINTS.limit - 1
    );
}

#[tokio::test]
async fn a_verified_hint_is_answered_the_same_whatever_became_of_it() {
    // Moved, already due, a job this server does not hold, a peer on probation: one answer, so
    // a pinned peer walking album ids learns nothing about which ones are pulled here.
    let (fixture, pair) = pinned().await;
    let mut answers = Vec::new();
    for outcome in [
        HintOutcome::Moved(fixture.clock.now() + SignedDuration::from_mins(1)),
        HintOutcome::Unchanged,
        HintOutcome::Ignored,
    ] {
        fixture.hints.answer(outcome);
        let response = hint(&fixture, &signed(&pair)).await;
        assert_eq!(response.status(), StatusCode::ACCEPTED, "{outcome:?}");
        let body: Value = response.json();
        answers.push(body);
    }
    assert!(
        answers.windows(2).all(|pair| pair[0] == pair[1]),
        "every outcome got the same body: {answers:?}"
    );

    // Only a store that could not answer differs, and it says nothing about any album.
    fixture.hints.answer(HintOutcome::Unavailable);
    assert_refused(
        &hint(&fixture, &signed(&pair)).await,
        StatusCode::INTERNAL_SERVER_ERROR,
        "error.federation.unavailable",
    );
}

#[tokio::test]
async fn a_hint_from_a_peer_nobody_pinned_is_refused_before_anything_is_charged() {
    let fixture = Fixture::working();
    let (pair, _) = support::peer_keypair();
    assert_refused(
        &hint(&fixture, &signed(&pair)).await,
        StatusCode::FORBIDDEN,
        "error.federation.peer_unknown",
    );
    assert!(fixture.hints.received().is_empty());
    assert_eq!(hints_left(&fixture).await, budgets::FEDERATED_HINTS.limit);
}

#[tokio::test]
async fn a_hint_from_a_blocked_peer_is_refused() {
    let (fixture, pair) = pinned().await;
    fixture
        .peers
        .block(&PeerId::new(PEER), fixture.clock.now(), None)
        .await
        .expect("blocked");
    assert_refused(
        &hint(&fixture, &signed(&pair)).await,
        StatusCode::FORBIDDEN,
        "error.moderation.server_blocked",
    );
    assert!(fixture.hints.received().is_empty());
}

#[tokio::test]
async fn a_hint_that_does_not_verify_under_the_pinned_key_charges_and_schedules_nothing() {
    let (fixture, _) = pinned().await;
    let (impostor, _) = support::peer_keypair();
    assert_refused(
        &hint(&fixture, &signed(&impostor)).await,
        StatusCode::UNAUTHORIZED,
        "error.federation.hint_unsigned",
    );
    // A stranger sending garbage under the peer's name cannot spend the peer's budget.
    assert_eq!(hints_left(&fixture).await, budgets::FEDERATED_HINTS.limit);
    assert!(fixture.hints.received().is_empty());
}

#[tokio::test]
async fn a_hint_signed_for_another_server_or_outside_the_window_is_unsigned() {
    let (fixture, pair) = pinned().await;
    let elsewhere = support::sign_hint(
        &pair,
        &HintClaim {
            home_server: PEER.to_owned(),
            recipient_server: "third.test".to_owned(),
            nonce: uuid::Uuid::now_v7().to_string(),
            album_id: ALBUM.to_owned(),
            sent_at: REPORTED_AT.to_owned(),
        },
    );
    assert_refused(
        &hint(&fixture, &elsewhere).await,
        StatusCode::UNAUTHORIZED,
        "error.federation.hint_unsigned",
    );

    let body = signed(&pair);
    fixture
        .clock
        .advance(capsule_server::federation::hints::HINT_FRESHNESS + SignedDuration::from_secs(1));
    assert_refused(
        &hint(&fixture, &body).await,
        StatusCode::UNAUTHORIZED,
        "error.federation.hint_unsigned",
    );
    assert!(fixture.hints.received().is_empty());
    assert_eq!(hints_left(&fixture).await, budgets::FEDERATED_HINTS.limit);
}

#[tokio::test]
async fn a_hint_is_accepted_once_and_a_replay_costs_the_peer_nothing() {
    let (fixture, pair) = pinned().await;
    let body = signed(&pair);
    hint(&fixture, &body)
        .await
        .assert_status(StatusCode::ACCEPTED);
    assert_refused(
        &hint(&fixture, &body).await,
        StatusCode::UNAUTHORIZED,
        "error.federation.hint_unsigned",
    );
    assert_eq!(
        fixture.hints.received(),
        vec![job()],
        "the replay reached nothing"
    );
    assert_eq!(
        hints_left(&fixture).await,
        budgets::FEDERATED_HINTS.limit - 1,
        "and charged nothing"
    );
}

#[tokio::test]
async fn a_peer_past_its_hint_budget_is_throttled_and_schedules_nothing() {
    let (fixture, pair) = pinned().await;
    for _ in 0..budgets::FEDERATED_HINTS.limit {
        fixture
            .counters
            .hit(
                &CounterKey::FederatedHints(PEER.to_owned()),
                budgets::FEDERATED_HINTS,
                fixture.clock.now(),
            )
            .await
            .expect("the counter answers");
    }
    assert_refused(
        &hint(&fixture, &signed(&pair)).await,
        StatusCode::TOO_MANY_REQUESTS,
        "error.federation.hint_rate_limited",
    );
    assert!(fixture.hints.received().is_empty());
}

#[tokio::test]
async fn a_full_hint_counter_refuses_closed_with_the_at_capacity_code() {
    let fixture = Fixture::with_counter_ceiling(1);
    let (pair, public) = support::peer_keypair();
    fixture
        .peers
        .pin(&PeerId::new(PEER), public, fixture.clock.now())
        .await
        .expect("the operator pins");
    hint(&fixture, &signed(&pair))
        .await
        .assert_status(StatusCode::ACCEPTED);
    // A second nonce has no room in a one-key partition: a 429 that says the limiter is full,
    // never a 500 and never an admission.
    assert_refused(
        &hint(&fixture, &signed(&pair)).await,
        StatusCode::TOO_MANY_REQUESTS,
        "error.federation.hint_at_capacity",
    );
    assert_eq!(fixture.hints.received(), vec![job()]);
}

#[tokio::test]
async fn a_malformed_hint_is_refused_before_any_store_is_read() {
    let (fixture, pair) = pinned().await;
    let mutations: [fn(&mut Value); 6] = [
        |body| body["nonce"] = "short".into(),
        |body| body["sent_at"] = "yesterday".into(),
        |body| body["signature"] = "not base64!".into(),
        |body| body["home_server"] = "not a host/".into(),
        |body| body["album_id"] = "a".repeat(65).into(),
        |body| body["album_id"] = "  ".into(),
    ];
    for (index, mutate) in mutations.iter().enumerate() {
        let mut body = signed(&pair);
        mutate(&mut body);
        let refused = hint(&fixture, &body).await;
        assert_eq!(
            refused.status(),
            StatusCode::BAD_REQUEST,
            "mutation {index}"
        );
        let refused: Value = refused.json();
        assert_eq!(refused["code"], "error.federation.hint_malformed");
    }
    assert!(fixture.hints.received().is_empty());
}

#[tokio::test]
async fn a_deployment_that_does_not_federate_answers_404_before_any_work() {
    let fixture = Fixture::without_federation();
    let (pair, public) = support::peer_keypair();
    fixture
        .peers
        .pin(&PeerId::new(PEER), public, fixture.clock.now())
        .await
        .expect("the operator pins");
    assert_refused(
        &hint(&fixture, &signed(&pair)).await,
        StatusCode::NOT_FOUND,
        "error.federation.not_configured",
    );
    assert!(fixture.hints.received().is_empty());
    assert_eq!(hints_left(&fixture).await, budgets::FEDERATED_HINTS.limit);
}
