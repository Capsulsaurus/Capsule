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
