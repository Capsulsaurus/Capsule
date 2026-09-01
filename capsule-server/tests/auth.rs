//! The auth surface, driven end to end over HTTP.
//!
//! Every case ends in `assert_conformance()`, which is the assertion that the response the
//! server just sent is one its own description predicts. The other direction — every response
//! the description promises has been produced by *some* test — is
//! `every_declared_response_is_exercised` in `conformance.rs`, and it is why the unhappy paths
//! below are as thorough as the happy ones: a 423 nothing produces is exactly the `S-C28`
//! defect pointing the other way.
//!
//! Nothing here needs a container. The session store is `S-C29`'s deterministic in-memory
//! adapter, the account directory is a double, and the token signer is the **real** one over a
//! generated key — so a 401 in this file is a token that genuinely does not verify.

mod support;

use capsule_server::auth::{ACCESS_TOKEN_TTL, TokenKind};
use capsule_server::routes::auth::TokenResponse;
use capsule_server::store::{AuthStateStore, Clock, SessionId};
use jiff::SignedDuration;
use kynos::http::StatusCode;
use serde_json::json;
use support::{EMAIL, Fixture, PASSWORD, SESSION_TTL, user};

/// Post a JSON body to `path`, as a well-formed client would.
macro_rules! post_json {
    ($client:expr, $path:literal, $body:expr) => {
        $client
            .post($path)
            .header("accept", "application/json")
            .json(&$body)
            .send()
            .await
    };
}

/// The `error.*` code an RFC 9457 problem body publishes as its `code` extension member.
fn code_of(body: &serde_json::Value) -> &str {
    body.get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<no code member>")
}

// ===========================================================================================
// POST /v1/auth/login
// ===========================================================================================

#[tokio::test]
async fn login_issues_a_pair_and_opens_the_session_it_names() {
    let fixture = Fixture::working();

    let body: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::OK)
    .json();

    assert_eq!(body.token_type, "Bearer");
    assert_eq!(
        body.expires_by,
        u64::try_from((fixture.clock.now() + ACCESS_TOKEN_TTL).as_second()).expect("positive"),
        "`expires_by` is the absolute instant the access token dies, which is what the SDK reads"
    );

    // The token names a session, and the session exists — asserted against the store the server
    // wrote to rather than by reading the response body a second time.
    let verified = fixture
        .tokens
        .verify(&body.access_token, TokenKind::Access)
        .expect("the server's own signer reads the token it just minted");
    assert_eq!(verified.user, user());

    let open = fixture
        .sessions
        .sessions_for_user(&user())
        .await
        .expect("store answers");
    assert_eq!(open.len(), 1, "one login opens exactly one session");
    assert_eq!(open[0].session_id, verified.session);
    assert_eq!(open[0].created_at, fixture.clock.now());
    assert_eq!(open[0].last_active_at, fixture.clock.now());

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn the_refresh_token_never_outlives_the_session_it_names() {
    // The two lifetimes are one fact: the route issues against `AuthStateStore::ttl()`, so a
    // deployment cannot configure a refresh token that verifies past its own session record.
    let fixture = Fixture::working();

    let body: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::OK)
    .json();

    fixture
        .clock
        .advance(SESSION_TTL - SignedDuration::from_secs(1));
    assert!(
        fixture
            .tokens
            .verify(&body.refresh_token, TokenKind::Refresh)
            .is_ok(),
        "a second before the session expires the refresh token is still live"
    );

    fixture.clock.advance(SignedDuration::from_secs(2));
    assert!(
        fixture
            .tokens
            .verify(&body.refresh_token, TokenKind::Refresh)
            .is_err(),
        "and it dies with the record, not after it"
    );

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn login_records_the_advisory_provenance_a_client_volunteered() {
    let fixture = Fixture::working();
    let device = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f";

    post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({
            "email": EMAIL,
            "password": PASSWORD,
            "cohort_hash": "  a-physical-phone  ",
            "device_id": device,
        })
    )
    .assert_status(StatusCode::OK);

    let open = fixture
        .sessions
        .sessions_for_user(&user())
        .await
        .expect("store answers");
    assert_eq!(open[0].cohort_hash.as_deref(), Some("a-physical-phone"));
    assert_eq!(
        open[0].device_id.map(|id| id.to_string()).as_deref(),
        Some(device),
        "the device id is parsed once, above the port, and stored as a `Uuid`"
    );

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn an_unusable_advisory_field_is_dropped_rather_than_refusing_the_sign_in() {
    // Neither field gates anything (`S-C13`, `S-N3`), so failing a sign-in over one would let
    // legibility metadata take down a security operation.
    let fixture = Fixture::working();

    post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({
            "email": EMAIL,
            "password": PASSWORD,
            "cohort_hash": "x".repeat(129),
            "device_id": "definitely-not-a-uuid",
        })
    )
    .assert_status(StatusCode::OK);

    let open = fixture
        .sessions
        .sessions_for_user(&user())
        .await
        .expect("store answers");
    assert_eq!(open[0].cohort_hash, None);
    assert_eq!(open[0].device_id, None);

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_wrong_password_and_an_unknown_account_are_the_same_answer() {
    // The Salvo service went to the trouble of verifying a dummy hash so the two paths took the
    // same time. Here they are the same *value* — `Authentication::Refused` — so no caller can
    // tell them apart even in principle.
    let fixture = Fixture::working();

    let wrong: serde_json::Value = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": "not the password" })
    )
    .assert_status(StatusCode::UNAUTHORIZED)
    .json();

    let unknown: serde_json::Value = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": "nobody@example.test", "password": PASSWORD })
    )
    .assert_status(StatusCode::UNAUTHORIZED)
    .json();

    assert_eq!(
        wrong, unknown,
        "the endpoint must not be an enumeration oracle"
    );
    assert_eq!(code_of(&wrong), "error.auth.invalid_credentials");

    assert!(
        fixture
            .sessions
            .sessions_for_user(&user())
            .await
            .expect("store answers")
            .is_empty(),
        "a refused sign-in opens nothing"
    );

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_locked_account_is_refused_with_423_and_says_so() {
    // The status `S-C28` found rendered and never declared. It is reachable — lockout is
    // account state the directory owns, not a counter — so it is documented rather than deleted,
    // and a correct password still gets it, which is exactly why it must be distinguishable
    // from a 401.
    let fixture = Fixture::working();
    fixture.accounts.lock(EMAIL);

    let body: serde_json::Value = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::LOCKED)
    .json();

    assert_eq!(code_of(&body), "error.auth.account_locked");

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn login_answers_500_when_the_account_directory_cannot_answer() {
    let fixture = Fixture::working();
    fixture.accounts.set_unavailable(true);

    let body: serde_json::Value = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
    .json();

    assert_eq!(code_of(&body), "error.auth.unavailable");
    assert!(
        !serde_json::to_string(&body)
            .unwrap_or_default()
            .contains("refuses on purpose"),
        "the backend's own words stay in the log. The Salvo 500 rendered \
         `format!(\"DEBUG: {{:?}}\")` of the underlying error straight to the client in debug \
         builds, in text/plain, on an endpoint whose every other status was JSON"
    );

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn login_answers_500_when_the_session_cannot_be_opened() {
    // The other half of login's 500: the credentials were fine and the *store* could not take
    // the session. A caller cannot tell the two apart, and should not have to.
    let fixture = Fixture::working();
    fixture.sessions.set_unavailable(true);

    post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::INTERNAL_SERVER_ERROR);

    fixture.client.assert_conformance();
}

// ===========================================================================================
// POST /v1/auth/refresh
// ===========================================================================================

#[tokio::test]
async fn refresh_rotates_the_session_and_closes_the_one_it_was_given() {
    let fixture = Fixture::working();

    let first: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::OK)
    .json();
    let original = fixture
        .tokens
        .verify(&first.refresh_token, TokenKind::Refresh)
        .expect("the pair verifies")
        .session;

    let second: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/refresh",
        json!({ "refresh_token": first.refresh_token })
    )
    .assert_status(StatusCode::OK)
    .json();
    let rotated = fixture
        .tokens
        .verify(&second.refresh_token, TokenKind::Refresh)
        .expect("the new pair verifies")
        .session;

    assert_ne!(original, rotated, "a refresh mints a new session id");
    assert!(
        fixture
            .sessions
            .read_session(&original)
            .await
            .expect("store answers")
            .is_none(),
        "the presented session is closed, which is what makes a refresh token single-use"
    );

    // One session, not two — and the count is the store's, so a record left behind would show
    // up here rather than being invisible the way the Salvo per-user index made it.
    let open = fixture
        .sessions
        .sessions_for_user(&user())
        .await
        .expect("store answers");
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].session_id, rotated);

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_refresh_token_is_spent_by_the_first_use() {
    let fixture = Fixture::working();

    let first: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::OK)
    .json();

    post_json!(
        fixture.client,
        "/v1/auth/refresh",
        json!({ "refresh_token": first.refresh_token })
    )
    .assert_status(StatusCode::OK);

    let replayed: serde_json::Value = post_json!(
        fixture.client,
        "/v1/auth/refresh",
        json!({ "refresh_token": first.refresh_token })
    )
    .assert_status(StatusCode::UNAUTHORIZED)
    .json();
    assert_eq!(code_of(&replayed), "error.auth.session_expired");

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn rotation_carries_the_session_provenance_across() {
    // Without this, the devices listing would lose track of a physical device every time its
    // tokens turned over — the grouping `S-C13` exists for, and the `(device_id, session_id)`
    // pair the support bundle carries (`S-N3`), both decay to nothing.
    let fixture = Fixture::working();
    let device = "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f";

    let first: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({
            "email": EMAIL,
            "password": PASSWORD,
            "cohort_hash": "a-physical-phone",
            "device_id": device,
        })
    )
    .assert_status(StatusCode::OK)
    .json();

    post_json!(
        fixture.client,
        "/v1/auth/refresh",
        json!({ "refresh_token": first.refresh_token })
    )
    .assert_status(StatusCode::OK);

    let open = fixture
        .sessions
        .sessions_for_user(&user())
        .await
        .expect("store answers");
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].cohort_hash.as_deref(), Some("a-physical-phone"));
    assert_eq!(
        open[0].device_id.map(|id| id.to_string()).as_deref(),
        Some(device)
    );

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn an_access_token_cannot_rotate_a_session() {
    // The live Salvo defect: `refresh_token` decoded the token and never inspected its scopes,
    // so a 15-minute access token bought a fresh 7-day pair. Here the kind is an argument to
    // `verify`, so the check is not something a handler can omit.
    let fixture = Fixture::working();

    let pair: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::OK)
    .json();

    let refused: serde_json::Value = post_json!(
        fixture.client,
        "/v1/auth/refresh",
        json!({ "refresh_token": pair.access_token })
    )
    .assert_status(StatusCode::UNAUTHORIZED)
    .json();
    assert_eq!(code_of(&refused), "error.auth.session_expired");

    let open = fixture
        .sessions
        .sessions_for_user(&user())
        .await
        .expect("store answers");
    assert_eq!(open.len(), 1, "and nothing was rotated");

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn refresh_refuses_an_expired_token_a_forgery_and_an_unknown_session_alike() {
    let fixture = Fixture::working();

    let pair: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::OK)
    .json();

    // A token this server never signed.
    let stranger = support::signer(fixture.clock.clone());
    let forged = stranger
        .issue(&user(), &SessionId::new("whatever"), SESSION_TTL)
        .expect("the stranger signs");

    // A correctly signed token naming a session the store does not hold.
    let orphan = fixture
        .tokens
        .issue(&user(), &SessionId::new("never-opened"), SESSION_TTL)
        .expect("the server's signer signs");

    for candidate in [forged.refresh_token, orphan.refresh_token] {
        let body: serde_json::Value = post_json!(
            fixture.client,
            "/v1/auth/refresh",
            json!({ "refresh_token": candidate })
        )
        .assert_status(StatusCode::UNAUTHORIZED)
        .json();
        assert_eq!(code_of(&body), "error.auth.session_expired");
    }

    // And an expired one, walked over deterministically rather than slept through.
    fixture
        .clock
        .advance(SESSION_TTL + SignedDuration::from_secs(1));
    post_json!(
        fixture.client,
        "/v1/auth/refresh",
        json!({ "refresh_token": pair.refresh_token })
    )
    .assert_status(StatusCode::UNAUTHORIZED);

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn refresh_refuses_a_token_whose_subject_does_not_own_the_session() {
    // Unreachable while the signing key is the server's alone, which is exactly why it is worth
    // a cheap check: the day it *is* reachable, it is a session takeover.
    let fixture = Fixture::working();

    let pair: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::OK)
    .json();
    let session = fixture
        .tokens
        .verify(&pair.refresh_token, TokenKind::Refresh)
        .expect("verifies")
        .session;

    // A genuine token, signed by this server, naming somebody else's live session.
    let impostor = fixture
        .tokens
        .issue(
            &capsule_server::store::UserId::new("somebody-else"),
            &session,
            SESSION_TTL,
        )
        .expect("the server's signer signs");

    post_json!(
        fixture.client,
        "/v1/auth/refresh",
        json!({ "refresh_token": impostor.refresh_token })
    )
    .assert_status(StatusCode::UNAUTHORIZED);

    assert!(
        fixture
            .sessions
            .read_session(&session)
            .await
            .expect("store answers")
            .is_some(),
        "and the refusal did not close the session it failed to claim"
    );

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn refresh_answers_500_when_the_session_store_cannot_answer() {
    let fixture = Fixture::working();
    let pair = fixture.login().await;
    fixture.sessions.set_unavailable(true);

    let body: serde_json::Value = post_json!(
        fixture.client,
        "/v1/auth/refresh",
        json!({ "refresh_token": pair.refresh_token })
    )
    .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
    .json();
    assert_eq!(code_of(&body), "error.auth.unavailable");

    fixture.client.assert_conformance();
}

// ===========================================================================================
// POST /v1/auth/logout
// ===========================================================================================

#[tokio::test]
async fn logout_closes_the_session_the_credential_names() {
    let fixture = Fixture::working();

    let pair: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::OK)
    .json();

    fixture
        .client
        .post("/v1/auth/logout")
        .header("authorization", &format!("Bearer {}", pair.access_token))
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    assert!(
        fixture
            .sessions
            .sessions_for_user(&user())
            .await
            .expect("store answers")
            .is_empty(),
        "the session is gone from the record set and from the user's listing at once"
    );

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_second_logout_is_refused_because_the_first_one_worked() {
    // This case used to assert 204 twice, on the reasoning that "there is no longer a session"
    // is what the caller asked for either way. `S-C48` changed the answer and the change is the
    // point: the credential the second attempt presents names a session the ledger no longer
    // holds, so it is refused at the door and the handler is never entered.
    //
    // The client is not worse off. It asked for the session to end; the 401 is proof that it
    // did, and it is the same 401 every other operation would now give that token. What would
    // be worse is the old answer — a 204 from a credential that is dead everywhere else, which
    // is precisely the fifteen-minute window `S-C48` closed, observed on the one operation
    // whose job is to close it.
    let fixture = Fixture::working();

    let pair: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::OK)
    .json();

    fixture
        .client
        .post("/v1/auth/logout")
        .header("authorization", &format!("Bearer {}", pair.access_token))
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    fixture
        .client
        .post("/v1/auth/logout")
        .header("authorization", &format!("Bearer {}", pair.access_token))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn logout_without_a_usable_credential_is_401_and_says_what_to_send() {
    let fixture = Fixture::working();

    // Absent, wrongly framed, and present-but-unreadable are all 401 — a client that sent a
    // broken token is not an anonymous client.
    let attempts: [Option<&str>; 4] = [
        None,
        Some("Basic aGk6dGhlcmU="),
        Some("Bearer"),
        Some("Bearer not-a-token"),
    ];

    for attempt in attempts {
        let mut request = fixture.client.post("/v1/auth/logout");
        if let Some(header) = attempt {
            request = request.header("authorization", header);
        }
        request
            .send()
            .await
            .assert_status(StatusCode::UNAUTHORIZED)
            // Declared as a *required* response header, and sent, because the scheme supplies
            // one string to both the wire and the document.
            .assert_header("www-authenticate", "Bearer");
    }

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn an_expired_access_token_is_401() {
    let fixture = Fixture::working();

    let pair: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::OK)
    .json();

    fixture
        .clock
        .advance(ACCESS_TOKEN_TTL + SignedDuration::from_secs(1));

    fixture
        .client
        .post("/v1/auth/logout")
        .header("authorization", &format!("Bearer {}", pair.access_token))
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_refresh_token_is_a_valid_credential_and_an_insufficient_one() {
    // 403, not 401. Salvo answered 401 with `"Invalid scopes. Expected [AccessToken], got
    // [RefreshToken]"`, which tells a client to re-authenticate when what it actually did was
    // present the wrong one of two tokens it already holds.
    let fixture = Fixture::working();

    let pair: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::OK)
    .json();

    fixture
        .client
        .post("/v1/auth/logout")
        .header("authorization", &format!("Bearer {}", pair.refresh_token))
        .send()
        .await
        .assert_status(StatusCode::FORBIDDEN);

    assert_eq!(
        fixture
            .sessions
            .sessions_for_user(&user())
            .await
            .expect("store answers")
            .len(),
        1,
        "and the session it named was not closed"
    );

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn logout_answers_500_when_the_session_store_cannot_answer() {
    // Reached *past* authentication, and since `S-C48` that phrase is load-bearing: the bearer
    // scheme reads the session ledger, so a store that fails *everything* refuses this request
    // at the door with a 401 and the handler's own 500 becomes unreachable. The partial outage
    // is the one this answer was written for — the read succeeds, the close does not.
    let fixture = Fixture::working();
    let pair = fixture.login().await;
    fixture.sessions.set_unavailable_after_authentication(true);

    let body: serde_json::Value = fixture
        .client
        .post("/v1/auth/logout")
        .header("authorization", &format!("Bearer {}", pair.access_token))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .json();
    assert_eq!(code_of(&body), "error.auth.unavailable");

    fixture.client.assert_conformance();
}

// ===========================================================================================
// Body rejections, which the `Json` extractor declares on both operations that take one
// ===========================================================================================

#[tokio::test]
async fn a_malformed_body_is_400_on_every_operation_that_takes_one() {
    let fixture = Fixture::working();

    for path in ["/v1/auth/login", "/v1/auth/refresh"] {
        fixture
            .client
            .post(path)
            .body("application/json", "{ this is not json")
            .send()
            .await
            .assert_status(StatusCode::BAD_REQUEST);
    }

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_body_of_the_wrong_shape_is_422_on_every_operation_that_takes_one() {
    let fixture = Fixture::working();

    // Well-formed JSON, wrong document: a bug in the client's model rather than its serializer,
    // which is the distinction 400 and 422 draw.
    fixture
        .client
        .post("/v1/auth/login")
        .json(&json!({ "email": EMAIL }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    fixture
        .client
        .post("/v1/auth/refresh")
        .json(&json!({ "refresh_token": 42 }))
        .send()
        .await
        .assert_status(StatusCode::UNPROCESSABLE_ENTITY);

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_body_in_the_wrong_media_type_is_415_on_every_operation_that_takes_one() {
    let fixture = Fixture::working();

    for path in ["/v1/auth/login", "/v1/auth/refresh"] {
        fixture
            .client
            .post(path)
            .body("text/plain", "{}")
            .send()
            .await
            .assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    fixture.client.assert_conformance();
}

// ===========================================================================================
// The session ledger on the request path (`S-C48`)
// ===========================================================================================

/// The `last_active_at` the devices listing publishes for the caller's own session.
async fn current_last_active(fixture: &Fixture, bearer: &str) -> String {
    let ledger: serde_json::Value = fixture
        .client
        .get("/v1/auth/devices")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    ledger["sessions"]
        .as_array()
        .expect("a sessions array")
        .iter()
        .find(|session| session["current"] == json!(true))
        .expect("the caller's own session")["last_active_at"]
        .as_str()
        .expect("a timestamp")
        .to_owned()
}

#[tokio::test]
async fn an_access_token_dies_with_its_session_and_not_with_its_deadline() {
    // The property `S-C48` bought. Before it, the bearer scheme checked a signature and a
    // deadline and never asked whether the session still existed, so closing a session left
    // every access token minted against it usable for the rest of its fifteen minutes.
    let fixture = Fixture::working();
    let pair = fixture.login().await;
    let bearer = format!("Bearer {}", pair.access_token);

    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);

    fixture
        .client
        .post("/v1/auth/logout")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // The token itself is still perfectly valid — same signature, same issuer, and its deadline
    // is a quarter of an hour away. It is refused because the session it names is gone.
    assert!(
        fixture.clock.now() < pair_deadline(&fixture),
        "the token has not expired, so a refusal here can only be the ledger's"
    );
    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    fixture.client.assert_conformance();
}

/// When the access token minted at the current instant would expire.
fn pair_deadline(fixture: &Fixture) -> jiff::Timestamp {
    fixture
        .clock
        .now()
        .checked_add(ACCESS_TOKEN_TTL)
        .expect("a representable deadline")
}

#[tokio::test]
async fn an_unreadable_session_ledger_refuses_rather_than_admits() {
    // Fail closed. The alternative is an authentication bypass that an attacker triggers by
    // loading the store — the ledger would stop being consulted at exactly the moment somebody
    // wanted it not to be.
    //
    // The status is `401` and it is knowingly the wrong one: the honest answer is `503`, and
    // `AuthRejection` is Kynos's type with 401 and 403 and nothing else. What keeps it from
    // misleading a client is the operation below it — a client that answers this by refreshing
    // gets `error.auth.unavailable`, which is the truth.
    let fixture = Fixture::working();
    let pair = fixture.login().await;
    let bearer = format!("Bearer {}", pair.access_token);

    fixture.sessions.set_unavailable(true);
    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    let problem: serde_json::Value = fixture
        .client
        .post("/v1/auth/refresh")
        .header("accept", "application/json")
        .json(&json!({ "refresh_token": pair.refresh_token }))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .json();
    assert_eq!(
        code_of(&problem),
        "error.auth.unavailable",
        "the refusal at the door says nothing, so the operation a client retries on has to say \
         it is an outage and not an expiry"
    );

    // And it recovers on its own the moment the store does.
    fixture.sessions.set_unavailable(false);
    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn activity_moves_the_listing_and_is_coalesced_to_one_write_a_minute() {
    // `touch_session` had no production caller at all before `S-C48` — the port declared it, the
    // conformance suite exercised it, and no request path used it — so this field was the
    // sign-in time forever while the listing called it "last used".
    //
    // It moves now, and it is deliberately *coarse*: at most one write per minute per session,
    // because a touch on every request is a store write on every request. The staleness that
    // buys is asserted here rather than left to be discovered, since it is the visible half of
    // the trade.
    let fixture = Fixture::working();
    let signed_in_at = fixture.clock.now();
    let bearer = fixture.bearer().await;

    assert_eq!(
        current_last_active(&fixture, &bearer).await,
        signed_in_at.to_string(),
        "a fresh session's activity is its sign-in"
    );

    // Thirty seconds of traffic writes nothing: inside the coalescing window.
    fixture.clock.advance(SignedDuration::from_secs(30));
    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);
    assert_eq!(
        current_last_active(&fixture, &bearer).await,
        signed_in_at.to_string(),
        "a busy session inside the window is indistinguishable from an idle one, which is what \
         being coalesced means"
    );

    // Past the window, the next request writes it forward.
    fixture.clock.advance(SignedDuration::from_secs(31));
    let active_at = fixture.clock.now();
    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::OK);
    assert_eq!(
        current_last_active(&fixture, &bearer).await,
        active_at.to_string(),
        "the listing shows when the session was last used"
    );

    // And activity is not a lifetime extension. The store's TTL is absolute, so a session that
    // has been touched every minute for a day is still closed on schedule — a sliding lifetime
    // would be the caller-supplied TTL `S-C29` deleted, wearing activity as a disguise.
    fixture.clock.advance(SESSION_TTL);
    fixture
        .client
        .get("/v1/quota")
        .header("authorization", &bearer)
        .send()
        .await
        .assert_status(StatusCode::UNAUTHORIZED);

    fixture.client.assert_conformance();
}

// ===========================================================================================
// Registration (`S-C53`)
// ===========================================================================================

#[tokio::test]
async fn registering_creates_an_account_and_signs_it_in() {
    // The operation the rebuilt server did not have. Without it a fresh deployment has no first
    // user, and the plan's own acceptance round trip — register, init, import, push, sync, list —
    // cannot start.
    let fixture = Fixture::working();

    let pair: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/register",
        json!({ "email": "new@example.test", "password": "correct horse battery staple" })
    )
    .assert_status(StatusCode::OK)
    .json();

    // Signed in, not merely created: the alternative is a `201` and a client that immediately
    // posts the same credentials to `/login` for one more chance to fail.
    let verified = fixture
        .tokens
        .verify(&pair.access_token, TokenKind::Access)
        .expect("the pair the registration issued verifies");
    let open = fixture
        .sessions
        .sessions_for_user(&verified.user)
        .await
        .expect("the store answers");
    assert_eq!(open.len(), 1, "registration opened exactly one session");
    assert_eq!(
        open[0].authenticated_at,
        fixture.clock.now(),
        "registering *is* a credential presentation, so a freshness gate has something real to \
         measure from"
    );

    // And the credentials work, which is the property that makes the account real rather than a
    // row somebody wrote.
    let again: TokenResponse = post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": "new@example.test", "password": "correct horse battery staple" })
    )
    .assert_status(StatusCode::OK)
    .json();
    assert_ne!(
        again.access_token, pair.access_token,
        "a second sign-in is a second session"
    );

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_taken_address_is_refused_and_writes_nothing() {
    let fixture = Fixture::working();
    let body = json!({ "email": EMAIL, "password": "correct horse battery staple" });

    let problem: serde_json::Value = post_json!(fixture.client, "/v1/auth/register", body)
        .assert_status(StatusCode::CONFLICT)
        .json();
    assert_eq!(code_of(&problem), "error.auth.user_already_exists");

    // The seeded account's own password is untouched — a refused registration must not be a way
    // to overwrite somebody's credentials.
    post_json!(
        fixture.client,
        "/v1/auth/login",
        json!({ "email": EMAIL, "password": PASSWORD })
    )
    .assert_status(StatusCode::OK);

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_password_under_the_floor_is_refused_before_anything_is_written() {
    // A length floor and no composition rule. The password authenticates a *session* — the
    // master key never derives from it — so this is ordinary sign-in security, and composition
    // rules measurably push people towards shorter, more guessable passwords.
    let fixture = Fixture::working();

    for (name, body) in [
        (
            "a password under the floor",
            json!({ "email": "short@example.test", "password": "elevenchar" }),
        ),
        (
            "an address that is not one",
            json!({ "email": "  ", "password": "correct horse battery staple" }),
        ),
    ] {
        let problem: serde_json::Value = post_json!(fixture.client, "/v1/auth/register", body)
            .assert_status(StatusCode::BAD_REQUEST)
            .json();
        assert_eq!(
            code_of(&problem),
            "error.auth.registration_invalid",
            "{name}"
        );
    }

    // Nothing was created, so the address is still free.
    post_json!(
        fixture.client,
        "/v1/auth/register",
        json!({ "email": "short@example.test", "password": "correct horse battery staple" })
    )
    .assert_status(StatusCode::OK);

    fixture.client.assert_conformance();
}

#[tokio::test]
async fn registration_answers_500_when_the_registry_cannot_answer() {
    let fixture = Fixture::working();
    fixture.accounts.set_unavailable(true);

    let problem: serde_json::Value = post_json!(
        fixture.client,
        "/v1/auth/register",
        json!({ "email": "outage@example.test", "password": "correct horse battery staple" })
    )
    .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
    .json();
    assert_eq!(code_of(&problem), "error.auth.unavailable");

    fixture.client.assert_conformance();
}
