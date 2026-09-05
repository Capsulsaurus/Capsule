//! The OIDC relying party (slice `S-N1`), over the real wire.
//!
//! `HttpIdentityProvider` is driven against the in-process mock provider in
//! `support::idp`, which serves discovery JSON, a JWK Set and a form-decoding token endpoint
//! that mints EdDSA-signed ID tokens. No network beyond loopback; no container.

mod support;

use std::sync::Arc;

use capsule_server::auth::oidc::{
    AuthorizationRequest, ClaimRejection, HttpIdentityProvider, IdentityProvider, OidcSettings,
    ProviderError, Redemption, RedirectPolicy, code_challenge, fresh_nonce, fresh_state,
    fresh_verifier,
};
use capsule_server::store::memory::ManualClock;
use capsule_server::store::{AuthorizationCode, OidcNonce, PkceVerifier};
use jiff::SignedDuration;
use support::idp::{CLIENT_ID, Grant, MockIdp, Tamper};

const REDIRECT: &str = "http://127.0.0.1:4242/callback";

/// A clock the test drives, started at the wall clock: the mock provider mints `iat`/`exp` from
/// real time, and a relying party judging them from the epoch would refuse every token as not
/// yet valid.
fn wall_clock() -> Arc<ManualClock> {
    Arc::new(ManualClock::new(jiff::Timestamp::now()))
}

/// A relying party over `idp`, on a clock the test drives.
fn relying_party(idp: &MockIdp, clock: Arc<ManualClock>) -> HttpIdentityProvider {
    HttpIdentityProvider::new(
        OidcSettings {
            issuer: idp.issuer(),
            client_id: CLIENT_ID.to_owned(),
            client_secret: None,
            redirects: RedirectPolicy::new(None, true),
        },
        HttpIdentityProvider::http_client().expect("the client builds"),
        clock,
    )
}

/// One begun ceremony: what the relying party sent, and what it holds.
struct Ceremony {
    url: reqwest::Url,
    nonce: OidcNonce,
    verifier: PkceVerifier,
}

async fn begin(rp: &HttpIdentityProvider) -> Ceremony {
    let state = fresh_state();
    let nonce = fresh_nonce();
    let verifier = fresh_verifier();
    let challenge = code_challenge(&verifier);
    let url = rp
        .authorization_url(&AuthorizationRequest {
            redirect_uri: REDIRECT,
            state: &state,
            nonce: &nonce,
            code_challenge: &challenge,
        })
        .await
        .expect("the provider answers");
    Ceremony {
        url: reqwest::Url::parse(&url).expect("a URL"),
        nonce,
        verifier,
    }
}

fn query(url: &reqwest::Url, name: &str) -> String {
    url.query_pairs().find(|(k, _)| k == name).map_or_else(
        || panic!("the authorization URL carries {name}"),
        |(_, v)| v.into_owned(),
    )
}

/// A grant the provider will mint a conforming token for, from what the ceremony sent it.
fn grant_for(ceremony: &Ceremony, tamper: Tamper) -> Grant {
    Grant {
        code_challenge: query(&ceremony.url, "code_challenge"),
        nonce: query(&ceremony.url, "nonce"),
        redirect_uri: query(&ceremony.url, "redirect_uri"),
        subject: "subject-1".to_owned(),
        email: Some("somebody@example.test".to_owned()),
        tamper,
    }
}

async fn redeem(
    rp: &HttpIdentityProvider,
    ceremony: &Ceremony,
    code: &str,
) -> Result<capsule_server::auth::oidc::VerifiedIdentity, ProviderError> {
    let code = AuthorizationCode::new(code);
    rp.redeem(&Redemption {
        code: &code,
        verifier: &ceremony.verifier,
        redirect_uri: REDIRECT,
        nonce: &ceremony.nonce,
    })
    .await
}

#[tokio::test]
async fn the_authorization_url_carries_the_whole_request_and_discovery_is_fetched_once() {
    let idp = MockIdp::start().await;
    let rp = relying_party(&idp, wall_clock());

    let first = begin(&rp).await;
    assert_eq!(first.url.path(), "/idp/authorize");
    assert_eq!(query(&first.url, "response_type"), "code");
    assert_eq!(query(&first.url, "client_id"), CLIENT_ID);
    assert_eq!(query(&first.url, "redirect_uri"), REDIRECT);
    assert_eq!(query(&first.url, "scope"), "openid email");
    assert_eq!(query(&first.url, "code_challenge_method"), "S256");
    assert_eq!(
        query(&first.url, "code_challenge"),
        code_challenge(&first.verifier)
    );
    assert_eq!(query(&first.url, "nonce"), first.nonce.as_str());
    assert!(!query(&first.url, "state").is_empty());

    let _second = begin(&rp).await;
    assert_eq!(
        idp.discovery_hits(),
        1,
        "the discovery document is cached across ceremonies"
    );
}

#[tokio::test]
async fn a_redirect_the_policy_refuses_costs_no_round_trip() {
    let idp = MockIdp::start().await;
    let rp = relying_party(&idp, wall_clock());
    let state = fresh_state();
    let nonce = fresh_nonce();
    let error = rp
        .authorization_url(&AuthorizationRequest {
            redirect_uri: "https://evil.example.test/cb",
            state: &state,
            nonce: &nonce,
            code_challenge: "x",
        })
        .await
        .expect_err("refused");
    assert!(
        matches!(error, ProviderError::RedirectRefused { .. }),
        "{error:?}"
    );
    assert_eq!(idp.discovery_hits(), 0);
}

#[tokio::test]
async fn a_provider_that_is_down_is_unavailable_not_a_refusal() {
    let idp = MockIdp::start().await;
    idp.set_discovery_down(true);
    let rp = relying_party(&idp, wall_clock());
    let state = fresh_state();
    let nonce = fresh_nonce();
    let error = rp
        .authorization_url(&AuthorizationRequest {
            redirect_uri: REDIRECT,
            state: &state,
            nonce: &nonce,
            code_challenge: "x",
        })
        .await
        .expect_err("unavailable");
    assert!(
        matches!(error, ProviderError::Unavailable { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_conforming_exchange_yields_the_identity_over_a_real_form_post() {
    let idp = MockIdp::start().await;
    let rp = relying_party(&idp, wall_clock());
    let ceremony = begin(&rp).await;
    let code = idp.grant(grant_for(&ceremony, Tamper::None));

    let identity = redeem(&rp, &ceremony, &code).await.expect("verifies");
    assert_eq!(identity.issuer, idp.issuer());
    assert_eq!(identity.subject, "subject-1");
    assert_eq!(identity.email.as_deref(), Some("somebody@example.test"));
    assert!(identity.email_verified);
    assert_eq!(idp.token_hits(), 1);
    assert_eq!(idp.jwks_hits(), 1, "the key set is fetched on first use");

    // A second ceremony: the key set is reused, and the provider's single-use code is spent.
    let again = begin(&rp).await;
    let code = idp.grant(grant_for(&again, Tamper::None));
    redeem(&rp, &again, &code).await.expect("verifies");
    assert_eq!(idp.jwks_hits(), 1, "a known kid needs no refetch");
    let replay = redeem(&rp, &again, &code).await.expect_err("spent");
    assert!(
        matches!(replay, ProviderError::ExchangeRefused { .. }),
        "{replay:?}"
    );
}

#[tokio::test]
async fn a_wrong_verifier_is_refused_at_the_token_endpoint() {
    let idp = MockIdp::start().await;
    let rp = relying_party(&idp, wall_clock());
    let ceremony = begin(&rp).await;
    let code = idp.grant(grant_for(&ceremony, Tamper::None));

    // The relying party redeems with a verifier from *another* ceremony: the S256 challenge the
    // provider holds does not match, and the exchange — not the token — is what fails.
    let other = Ceremony {
        url: ceremony.url.clone(),
        nonce: ceremony.nonce.clone(),
        verifier: fresh_verifier(),
    };
    let error = redeem(&rp, &other, &code).await.expect_err("refused");
    match error {
        ProviderError::ExchangeRefused { detail } => assert!(detail.contains("PKCE"), "{detail}"),
        other => panic!("expected an exchange refusal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_rotated_key_is_fetched_once_and_then_trusted() {
    let idp = MockIdp::start().await;
    let clock = wall_clock();
    let rp = relying_party(&idp, clock.clone());

    let first = begin(&rp).await;
    let code = idp.grant(grant_for(&first, Tamper::None));
    redeem(&rp, &first, &code).await.expect("verifies under k1");
    assert_eq!(idp.jwks_hits(), 1);

    // A rotation happens minutes after the last fetch, not inside the refetch floor.
    clock.advance(SignedDuration::from_secs(61));
    idp.rotate("k2");
    let second = begin(&rp).await;
    let code = idp.grant(grant_for(&second, Tamper::None));
    redeem(&rp, &second, &code)
        .await
        .expect("an unknown kid triggers one refetch, and then verifies");
    assert_eq!(idp.jwks_hits(), 2);

    let third = begin(&rp).await;
    let code = idp.grant(grant_for(&third, Tamper::None));
    redeem(&rp, &third, &code).await.expect("k2 is now known");
    assert_eq!(idp.jwks_hits(), 2, "no refetch for a key already cached");
}

#[tokio::test]
async fn an_unpublished_key_is_refetched_once_refused_and_then_floored() {
    let idp = MockIdp::start().await;
    let clock = wall_clock();
    let rp = relying_party(&idp, clock.clone());

    let warm = begin(&rp).await;
    let code = idp.grant(grant_for(&warm, Tamper::None));
    redeem(&rp, &warm, &code).await.expect("verifies");
    assert_eq!(idp.jwks_hits(), 1);
    clock.advance(SignedDuration::from_secs(61));

    // A token signed by a key the provider never published: one refetch, still unknown, refused.
    let forged = begin(&rp).await;
    let code = idp.grant(grant_for(&forged, Tamper::UnpublishedKey));
    let error = redeem(&rp, &forged, &code).await.expect_err("refused");
    assert!(
        matches!(
            error,
            ProviderError::TokenRejected(ClaimRejection::UnknownKey { .. })
        ),
        "{error:?}"
    );
    assert_eq!(idp.jwks_hits(), 2);

    // Inside the floor, a second forged kid is refused **without** another fetch.
    let forged = begin(&rp).await;
    let code = idp.grant(grant_for(&forged, Tamper::UnpublishedKey));
    let error = redeem(&rp, &forged, &code).await.expect_err("refused");
    assert!(
        matches!(
            error,
            ProviderError::TokenRejected(ClaimRejection::UnknownKey { .. })
        ),
        "{error:?}"
    );
    assert_eq!(idp.jwks_hits(), 2, "the floor held");

    // Past the floor, the evidence is honoured again.
    clock.advance(SignedDuration::from_secs(61));
    let forged = begin(&rp).await;
    let code = idp.grant(grant_for(&forged, Tamper::UnpublishedKey));
    let _ = redeem(&rp, &forged, &code).await.expect_err("refused");
    assert_eq!(idp.jwks_hits(), 3);
}

#[tokio::test]
async fn every_tampered_token_is_refused_with_its_own_reason() {
    let idp = MockIdp::start().await;
    let rp = relying_party(&idp, wall_clock());

    /// Whether a rejection is the one a tamper should produce.
    type Expected = fn(&ClaimRejection) -> bool;
    let cases: [(Tamper, Expected); 5] = [
        (
            Tamper::Issuer("https://somebody-else.test".to_owned()),
            |r| matches!(r, ClaimRejection::Issuer { .. }),
        ),
        (Tamper::Audience("another-client".to_owned()), |r| {
            matches!(r, ClaimRejection::Audience)
        }),
        (Tamper::Expired, |r| {
            matches!(r, ClaimRejection::Expired { .. })
        }),
        (
            Tamper::Nonce("a-nonce-from-another-ceremony".to_owned()),
            |r| matches!(r, ClaimRejection::Nonce),
        ),
        (Tamper::AlgNone, |r| {
            matches!(
                r,
                ClaimRejection::Malformed { .. } | ClaimRejection::AlgorithmRefused { .. }
            )
        }),
    ];
    for (tamper, expected) in cases {
        let ceremony = begin(&rp).await;
        let code = idp.grant(grant_for(&ceremony, tamper.clone()));
        match redeem(&rp, &ceremony, &code).await {
            Err(ProviderError::TokenRejected(reason)) => {
                assert!(expected(&reason), "{tamper:?} was refused as {reason:?}");
            }
            other => panic!("{tamper:?} was not refused as a token rejection: {other:?}"),
        }
    }
}

#[tokio::test]
async fn a_nonce_from_another_ceremony_is_refused_even_when_the_provider_echoes_it() {
    // The provider echoes whatever nonce the authorization request carried; here the relying
    // party redeems with a *different* pending record's nonce, which is what a code stolen from
    // one ceremony and replayed into another looks like from the callback.
    let idp = MockIdp::start().await;
    let rp = relying_party(&idp, wall_clock());
    let ceremony = begin(&rp).await;
    let code = idp.grant(grant_for(&ceremony, Tamper::None));
    let other = Ceremony {
        url: ceremony.url.clone(),
        nonce: fresh_nonce(),
        verifier: ceremony.verifier.clone(),
    };
    let error = redeem(&rp, &other, &code).await.expect_err("refused");
    assert!(
        matches!(error, ProviderError::TokenRejected(ClaimRejection::Nonce)),
        "{error:?}"
    );
}

// ===========================================================================================
// The routes, over the fixture
// ===========================================================================================

use capsule_server::auth::oidc::VerifiedIdentity;
use capsule_server::routes::auth::TokenResponse;
use capsule_server::store::{AuthStateStore, Clock as _, OIDC_AUTHORIZATION_TTL};
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{Fixture, GOOD_CODE};

/// The `error.*` code an RFC 9457 problem body publishes as its `code` extension member.
fn code_of(body: &Value) -> &str {
    body.get("code")
        .and_then(Value::as_str)
        .unwrap_or("<no code member>")
}

/// Begin a ceremony through the route and return its body.
async fn authorize(fixture: &Fixture, redirect_uri: &str) -> Value {
    fixture
        .client
        .post("/v1/auth/oidc/authorize")
        .header("accept", "application/json")
        .json(&json!({ "redirect_uri": redirect_uri }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json()
}

/// Present `state` and `code` to the callback, with the advisory fields a client may add.
async fn callback(fixture: &Fixture, body: Value) -> kynos::test::TestResponse {
    fixture
        .client
        .post("/v1/auth/oidc/callback")
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await
}

#[tokio::test]
async fn a_begun_ceremony_publishes_the_url_the_state_and_its_deadline() {
    let fixture = Fixture::working();
    let body = authorize(&fixture, REDIRECT).await;

    let state = body["state"].as_str().expect("a state");
    assert!(!state.is_empty());
    let url = body["authorization_url"].as_str().expect("a URL");
    assert!(url.contains(state), "the URL carries the state: {url}");
    assert_eq!(
        body["expires_by"],
        u64::try_from((fixture.clock.now() + OIDC_AUTHORIZATION_TTL).as_second())
            .expect("positive"),
        "the deadline is the store's TTL from now, absolute"
    );

    // The double saw exactly one request, carrying the admitted redirect and an S256 challenge.
    let requests = fixture.idp.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].redirect_uri, REDIRECT);
    assert_eq!(requests[0].state, state);
    assert_eq!(requests[0].code_challenge.len(), 43);
    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_callback_opens_a_session_exactly_as_a_password_login_does() {
    let fixture = Fixture::working();
    let begun = authorize(&fixture, REDIRECT).await;

    let body: TokenResponse = callback(
        &fixture,
        json!({
            "state": begun["state"],
            "code": GOOD_CODE,
            "cohort_hash": "a-physical-phone",
            "device_id": "018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f",
        }),
    )
    .await
    .assert_status(StatusCode::OK)
    .json();
    assert_eq!(body.token_type, "Bearer");

    // The token names a session the store holds, with the advisory provenance recorded.
    let verified = fixture
        .tokens
        .verify(&body.access_token, capsule_server::auth::TokenKind::Access)
        .expect("the server's own signer reads it");
    let open = fixture
        .sessions
        .sessions_for_user(&verified.user)
        .await
        .expect("store answers");
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].cohort_hash.as_deref(), Some("a-physical-phone"));
    assert!(open[0].device_id.is_some());

    // A second sign-in for the same `(issuer, subject)` is the **same** account.
    let again = authorize(&fixture, REDIRECT).await;
    let second: TokenResponse = callback(
        &fixture,
        json!({ "state": again["state"], "code": GOOD_CODE }),
    )
    .await
    .assert_status(StatusCode::OK)
    .json();
    let second = fixture
        .tokens
        .verify(
            &second.access_token,
            capsule_server::auth::TokenKind::Access,
        )
        .expect("verifies");
    assert_eq!(second.user, verified.user);

    // And the pair refreshes, which is the acceptance test's "immediately usable".
    fixture
        .client
        .post("/v1/auth/refresh")
        .header("accept", "application/json")
        .json(&json!({ "refresh_token": body.refresh_token }))
        .send()
        .await
        .assert_status(StatusCode::OK);
    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_replayed_state_is_refused_and_so_is_an_expired_one() {
    let fixture = Fixture::working();
    let begun = authorize(&fixture, REDIRECT).await;
    callback(
        &fixture,
        json!({ "state": begun["state"], "code": GOOD_CODE }),
    )
    .await
    .assert_status(StatusCode::OK);

    let replayed: Value = callback(
        &fixture,
        json!({ "state": begun["state"], "code": GOOD_CODE }),
    )
    .await
    .assert_status(StatusCode::UNAUTHORIZED)
    .json();
    assert_eq!(code_of(&replayed), "error.auth.oidc_state_invalid");

    let stale = authorize(&fixture, REDIRECT).await;
    fixture.clock.advance(OIDC_AUTHORIZATION_TTL);
    let expired: Value = callback(
        &fixture,
        json!({ "state": stale["state"], "code": GOOD_CODE }),
    )
    .await
    .assert_status(StatusCode::UNAUTHORIZED)
    .json();
    assert_eq!(code_of(&expired), "error.auth.oidc_state_invalid");

    let unknown: Value = callback(
        &fixture,
        json!({ "state": "never-issued", "code": GOOD_CODE }),
    )
    .await
    .assert_status(StatusCode::UNAUTHORIZED)
    .json();
    assert_eq!(code_of(&unknown), "error.auth.oidc_state_invalid");
    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_failed_callback_burns_the_state_too() {
    // A ceremony that survived a failed callback would be one an attacker could retry a stolen
    // code against; the state is consumed whatever the provider says next.
    let fixture = Fixture::working();
    let begun = authorize(&fixture, REDIRECT).await;
    let refused: Value = callback(
        &fixture,
        json!({ "state": begun["state"], "code": "not-the-code" }),
    )
    .await
    .assert_status(StatusCode::UNAUTHORIZED)
    .json();
    assert_eq!(code_of(&refused), "error.auth.oidc_exchange_failed");

    let retried: Value = callback(
        &fixture,
        json!({ "state": begun["state"], "code": GOOD_CODE }),
    )
    .await
    .assert_status(StatusCode::UNAUTHORIZED)
    .json();
    assert_eq!(code_of(&retried), "error.auth.oidc_state_invalid");
    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_refused_token_is_one_code_on_the_wire() {
    let fixture = Fixture::working();
    fixture.idp.set_token_rejected(true);
    let begun = authorize(&fixture, REDIRECT).await;
    let body: Value = callback(
        &fixture,
        json!({ "state": begun["state"], "code": GOOD_CODE }),
    )
    .await
    .assert_status(StatusCode::UNAUTHORIZED)
    .json();
    assert_eq!(code_of(&body), "error.auth.oidc_token_invalid");
    fixture.client.assert_conformance();
}

#[tokio::test]
async fn an_address_another_account_holds_is_refused_never_linked() {
    let fixture = Fixture::working();
    // The seeded password account's address, asserted by a provider identity.
    fixture.idp.set_identity(VerifiedIdentity {
        issuer: "https://idp.test".to_owned(),
        subject: "impersonator".to_owned(),
        email: Some(support::EMAIL.to_owned()),
        email_verified: true,
    });
    let begun = authorize(&fixture, REDIRECT).await;
    let body: Value = callback(
        &fixture,
        json!({ "state": begun["state"], "code": GOOD_CODE }),
    )
    .await
    .assert_status(StatusCode::CONFLICT)
    .json();
    assert_eq!(code_of(&body), "error.auth.oidc_address_taken");

    // The password account's sessions are untouched: nothing was linked and nothing opened.
    let open = fixture
        .sessions
        .sessions_for_user(&support::user())
        .await
        .expect("store answers");
    assert!(open.is_empty());
    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_confirmed_second_factor_turns_the_callback_into_a_challenge() {
    let fixture = Fixture::working();
    // Sign in once so the federated account exists, then enroll a factor on it.
    let begun = authorize(&fixture, REDIRECT).await;
    let first: TokenResponse = callback(
        &fixture,
        json!({ "state": begun["state"], "code": GOOD_CODE }),
    )
    .await
    .assert_status(StatusCode::OK)
    .json();
    let user = fixture
        .tokens
        .verify(&first.access_token, capsule_server::auth::TokenKind::Access)
        .expect("verifies")
        .user;
    let bearer = format!("Bearer {}", first.access_token);
    fixture
        .client
        .post("/v1/auth/totp/enroll")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK);
    fixture
        .client
        .post("/v1/auth/totp/verify-enrollment")
        .header("authorization", &bearer)
        .header("accept", "application/json")
        .json(&json!({ "totp_code": support::totp_code(&fixture, &user) }))
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);

    // The next federated sign-in is a `202`, exactly as a password sign-in would be.
    let again = authorize(&fixture, REDIRECT).await;
    let challenge: Value = callback(
        &fixture,
        json!({ "state": again["state"], "code": GOOD_CODE }),
    )
    .await
    .assert_status(StatusCode::ACCEPTED)
    .json();
    assert!(challenge["mfa_token"].is_string());
    assert!(
        challenge.get("access_token").is_none(),
        "no session was opened"
    );

    // And the same completing request finishes it, with the cohort landing on the session.
    fixture.clock.advance(SignedDuration::from_secs(
        i64::try_from(capsule_server::auth::totp::STEP_SECONDS).expect("fits"),
    ));
    fixture
        .client
        .post("/v1/auth/login/verify-totp")
        .header("accept", "application/json")
        .json(&json!({
            "mfa_token": challenge["mfa_token"],
            "totp_code": support::totp_code(&fixture, &user),
            "cohort_hash": "the-phone",
        }))
        .send()
        .await
        .assert_status(StatusCode::OK);
    let open = fixture
        .sessions
        .sessions_for_user(&user)
        .await
        .expect("store answers");
    assert!(
        open.iter()
            .any(|s| s.cohort_hash.as_deref() == Some("the-phone"))
    );
    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_redirect_the_policy_refuses_is_a_400_and_writes_nothing() {
    let fixture = Fixture::working();
    let body: Value = fixture
        .client
        .post("/v1/auth/oidc/authorize")
        .header("accept", "application/json")
        .json(&json!({ "redirect_uri": "https://evil.example.test/cb" }))
        .send()
        .await
        .assert_status(StatusCode::BAD_REQUEST)
        .json();
    assert_eq!(code_of(&body), "error.auth.oidc_redirect_invalid");
    assert!(fixture.idp.requests().is_empty());
    fixture.client.assert_conformance();
}

#[tokio::test]
async fn an_unconfigured_deployment_answers_404_and_publishes_no_endpoints() {
    let fixture = Fixture::working();
    fixture.idp.set_configured(false);
    let body: Value = fixture
        .client
        .post("/v1/auth/oidc/authorize")
        .header("accept", "application/json")
        .json(&json!({ "redirect_uri": REDIRECT }))
        .send()
        .await
        .assert_status(StatusCode::NOT_FOUND)
        .json();
    assert_eq!(code_of(&body), "error.auth.oidc_not_configured");

    // A callback on such a deployment finds no ceremony, and says so with the one code.
    let body: Value = callback(&fixture, json!({ "state": "anything", "code": GOOD_CODE }))
        .await
        .assert_status(StatusCode::UNAUTHORIZED)
        .json();
    assert_eq!(code_of(&body), "error.auth.oidc_state_invalid");
    fixture.client.assert_conformance();

    // The record, built without `with_oidc`, publishes `null`.
    let info = capsule_server::discovery::ServerInfo::new(
        "capsule.test",
        "https://capsule.test/v1",
        capsule_server::discovery::ProtocolWindow {
            min: "2026-01-01".to_owned(),
            max: "2026-01-01".to_owned(),
        },
        Vec::new(),
    );
    assert!(info.auth().oidc.is_none());
}

#[tokio::test]
async fn the_published_oidc_endpoints_are_the_ones_the_server_serves() {
    let fixture = Fixture::working();
    let record: Value = fixture
        .client
        .raw()
        .get("/.well-known/capsule/server-info")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let authorize_url = record["auth"]["oidc"]["authorize"]
        .as_str()
        .expect("published")
        .to_owned();
    let callback_url = record["auth"]["oidc"]["callback"]
        .as_str()
        .expect("published")
        .to_owned();
    assert_eq!(authorize_url, "https://capsule.test/v1/auth/oidc/authorize");
    assert_eq!(callback_url, "https://capsule.test/v1/auth/oidc/callback");

    let path = authorize_url
        .strip_prefix("https://capsule.test")
        .expect("under the base");
    let begun: Value = fixture
        .client
        .post(path)
        .header("accept", "application/json")
        .json(&json!({ "redirect_uri": REDIRECT }))
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let path = callback_url
        .strip_prefix("https://capsule.test")
        .expect("under the base");
    fixture
        .client
        .post(path)
        .header("accept", "application/json")
        .json(&json!({ "state": begun["state"], "code": GOOD_CODE }))
        .send()
        .await
        .assert_status(StatusCode::OK);
    fixture.client.assert_conformance();
}

#[tokio::test]
async fn a_provider_or_store_outage_is_a_500_with_the_code_that_names_it() {
    let fixture = Fixture::working();

    fixture.idp.set_unavailable(true);
    let body: Value = fixture
        .client
        .post("/v1/auth/oidc/authorize")
        .header("accept", "application/json")
        .json(&json!({ "redirect_uri": REDIRECT }))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .json();
    assert_eq!(code_of(&body), "error.auth.oidc_unavailable");
    fixture.idp.set_unavailable(false);

    fixture.oidc_authorizations.set_unavailable(true);
    let body: Value = fixture
        .client
        .post("/v1/auth/oidc/authorize")
        .header("accept", "application/json")
        .json(&json!({ "redirect_uri": REDIRECT }))
        .send()
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .json();
    assert_eq!(code_of(&body), "error.auth.unavailable");
    let body: Value = callback(&fixture, json!({ "state": "x", "code": GOOD_CODE }))
        .await
        .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
        .json();
    assert_eq!(code_of(&body), "error.auth.unavailable");
    fixture.oidc_authorizations.set_unavailable(false);

    let begun = authorize(&fixture, REDIRECT).await;
    fixture.idp.set_unavailable(true);
    let body: Value = callback(
        &fixture,
        json!({ "state": begun["state"], "code": GOOD_CODE }),
    )
    .await
    .assert_status(StatusCode::INTERNAL_SERVER_ERROR)
    .json();
    assert_eq!(code_of(&body), "error.auth.oidc_unavailable");
    fixture.idp.set_unavailable(false);
    fixture.client.assert_conformance();
}

// ===========================================================================================
// The routes, over the real adapter and the mock provider
// ===========================================================================================

#[tokio::test]
async fn the_whole_handshake_round_trips_over_the_wire() {
    let idp = MockIdp::start().await;
    let provider = Arc::new(relying_party(&idp, wall_clock()));
    let fixture = Fixture::with_identity_provider(provider);

    let begun = authorize(&fixture, REDIRECT).await;
    let url =
        reqwest::Url::parse(begun["authorization_url"].as_str().expect("a URL")).expect("parses");
    // The provider's redirect carries the code; the client posts it with the state.
    let code = idp.grant(Grant {
        code_challenge: query(&url, "code_challenge"),
        nonce: query(&url, "nonce"),
        redirect_uri: query(&url, "redirect_uri"),
        subject: "wire-subject".to_owned(),
        email: Some("wire@example.test".to_owned()),
        tamper: Tamper::None,
    });
    let body: TokenResponse = callback(&fixture, json!({ "state": begun["state"], "code": code }))
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert!(!body.access_token.is_empty());
    assert_eq!(idp.token_hits(), 1);

    // Tampered tokens through the route: each is the one wire code.
    for tamper in [
        Tamper::Issuer("https://somebody-else.test".to_owned()),
        Tamper::Audience("another-client".to_owned()),
        Tamper::Expired,
        Tamper::Nonce("another-nonce".to_owned()),
        Tamper::AlgNone,
    ] {
        let begun = authorize(&fixture, REDIRECT).await;
        let url = reqwest::Url::parse(begun["authorization_url"].as_str().expect("a URL"))
            .expect("parses");
        let code = idp.grant(Grant {
            code_challenge: query(&url, "code_challenge"),
            nonce: query(&url, "nonce"),
            redirect_uri: query(&url, "redirect_uri"),
            subject: "wire-subject".to_owned(),
            email: None,
            tamper: tamper.clone(),
        });
        let body: Value = callback(&fixture, json!({ "state": begun["state"], "code": code }))
            .await
            .assert_status(StatusCode::UNAUTHORIZED)
            .json();
        assert_eq!(
            code_of(&body),
            "error.auth.oidc_token_invalid",
            "{tamper:?}"
        );
    }
    fixture.client.assert_conformance();
}
