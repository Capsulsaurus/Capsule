//! The `.well-known/capsule/*` registry, end to end (slices `S-C15`, `S-C18`).
//!
//! Every record here is fetched **with no credential**, and that omission is the assertion: a
//! client deciding whether it can talk to this server at all has no token yet, and a peer
//! checking whether a capability token it holds is still good is by construction not
//! authenticated here. If any of these grew an `Auth` extractor, these cases would stop
//! compiling their way to a `200` and start returning `401`.
//!
//! The case that carries `S-C18` is `a_peer_refuses_a_token_the_published_list_revokes`: it
//! drives the *verifying* half of design/federation.md's revocation rule over the list this
//! server actually published, rather than over a hand-built record. The two halves only work
//! together, and a published list nobody consumes proves nothing about revocation.

mod support;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_server::discovery::revocation::{
    MAX_STALENESS, PublishedRevocations, RevocationList, RevocationVerdict, RevokedToken,
    check_revocation,
};
use capsule_server::store::Clock;
use jiff::{SignedDuration, Timestamp};
use kynos::http::StatusCode;
use serde_json::Value;
use support::{Fixture, PROTOCOL_VERSION, SERVER_ORIGIN};

/// Fetch a registry record with no credential and assert it is served.
async fn record(fixture: &Fixture, path: &str) -> Value {
    fixture
        .client
        .get(path)
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json()
}

#[tokio::test]
async fn server_info_is_public_and_names_this_server() {
    let fixture = Fixture::working();
    let body = record(&fixture, "/.well-known/capsule/server-info").await;

    assert_eq!(body["server_id"], SERVER_ORIGIN);
    assert_eq!(body["api_base_url"], "https://capsule.test/v1");
    assert_eq!(body["protocol_version"]["max"], PROTOCOL_VERSION);
    assert_eq!(body["signing_algorithm"], "ed25519");
}

#[tokio::test]
async fn server_info_publishes_the_key_its_own_tokens_verify_under() {
    // The invariant `ServerInfo` exists to hold. A published key that is only *usually* the
    // signing key fails silently on this side and totally on the peer's: every token this
    // server ever minted becomes unverifiable at once, which from outside is indistinguishable
    // from a compromise. So the record is derived from the signer, and this asserts the bytes.
    let fixture = Fixture::working();
    let body = record(&fixture, "/.well-known/capsule/server-info").await;

    let published = BASE64
        .decode(
            body["signing_key"]
                .as_str()
                .expect("the signing key is a string"),
        )
        .expect("the published key is base64");

    assert_eq!(published, fixture.tokens.public_key());
    assert_eq!(published.len(), 32, "a raw Ed25519 public key");
}

#[tokio::test]
async fn server_info_never_names_a_user() {
    // The registry's one rule, asserted against the served bytes rather than against the type:
    // a federated peer that can enumerate accounts has an abuse surface no rate limit takes
    // back, and this record is the obvious place for one to be added by accident.
    let fixture = Fixture::working();
    let body = record(&fixture, "/.well-known/capsule/server-info").await;

    let rendered = body.to_string();
    assert!(
        !rendered.contains(support::EMAIL),
        "the discovery record leaked the seeded account: {rendered}"
    );
    let object = body.as_object().expect("the record is an object");
    for field in ["users", "accounts", "user_count", "handles"] {
        assert!(
            !object.contains_key(field),
            "the discovery record grew a `{field}` field"
        );
    }
}

#[tokio::test]
async fn a_server_that_does_not_federate_publishes_no_federation_endpoint() {
    // Absence, not a null or a placeholder: a published endpoint that answers nothing invites
    // every peer to fail against it.
    let fixture = Fixture::working();
    let body = record(&fixture, "/.well-known/capsule/server-info").await;

    assert!(
        body.as_object()
            .expect("the record is an object")
            .get("federation_url")
            .is_none()
    );
}

#[tokio::test]
async fn the_deprecation_record_is_served_and_empty_when_nothing_is_announced() {
    // Empty rather than 404. A client polling for a cutoff has to be able to tell "no
    // deprecation is pending" from "this server does not publish deprecations", and only one of
    // those is a reason to stop asking.
    let fixture = Fixture::working();
    let body = record(&fixture, "/.well-known/capsule/deprecation").await;

    assert_eq!(
        body["announcements"]
            .as_array()
            .expect("announcements is an array")
            .len(),
        0
    );
}

#[tokio::test]
async fn the_revocation_list_publishes_what_was_revoked_and_says_how_stale_it_may_be() {
    let fixture = Fixture::working();
    let expires_at = fixture.clock.now() + SignedDuration::from_hours(2);
    fixture
        .revocations
        .revoke(RevokedToken {
            jti: "01937b7c-0000-7000-8000-0000000000aa".to_owned(),
            expires_at,
        })
        .await
        .expect("the revocation is recorded");

    let body = record(&fixture, "/.well-known/capsule/revoked-jti").await;

    assert_eq!(
        body["max_staleness_seconds"],
        MAX_STALENESS.as_secs(),
        "the bound is published rather than left to every peer to have read the same document"
    );
    let revoked = body["revoked"].as_array().expect("revoked is an array");
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0]["jti"], "01937b7c-0000-7000-8000-0000000000aa");
    assert_eq!(revoked[0]["expires_at"], expires_at.to_string());
}

#[tokio::test]
async fn an_expired_entry_leaves_the_published_list() {
    // What keeps the record bounded by 24 hours of revocations without anything sweeping it: an
    // expired token is refused whether or not it appears here, so the entry carries nothing.
    let fixture = Fixture::working();
    fixture
        .revocations
        .revoke(RevokedToken {
            jti: "expiring".to_owned(),
            expires_at: fixture.clock.now() + SignedDuration::from_hours(1),
        })
        .await
        .expect("the revocation is recorded");

    assert_eq!(
        record(&fixture, "/.well-known/capsule/revoked-jti").await["revoked"]
            .as_array()
            .expect("revoked is an array")
            .len(),
        1
    );

    fixture.clock.advance(SignedDuration::from_hours(2));
    let body = record(&fixture, "/.well-known/capsule/revoked-jti").await;
    assert!(
        body["revoked"]
            .as_array()
            .expect("revoked is an array")
            .is_empty()
    );
}

#[tokio::test]
async fn a_peer_refuses_a_token_the_published_list_revokes() {
    // The slice's own criterion: a second server's revocation check consuming *this* server's
    // published list. The peer is stood up as what a peer actually is here — a verifier holding
    // a token and a fetched copy of the issuer's list — rather than as a whole second
    // deployment, because the rule under test is entirely a property of those two things.
    let fixture = Fixture::working();
    let now = fixture.clock.now();
    let expires_at = now + SignedDuration::from_hours(6);
    fixture
        .revocations
        .revoke(RevokedToken {
            jti: "revoked-capability".to_owned(),
            expires_at,
        })
        .await
        .expect("the revocation is recorded");

    let fetched = fetch_list(&fixture).await;

    assert_eq!(
        check_revocation(&fetched, "revoked-capability", expires_at, now),
        RevocationVerdict::Revoked
    );
    assert_eq!(
        check_revocation(&fetched, "some-other-capability", expires_at, now),
        RevocationVerdict::Honored,
        "a peer must not refuse everything because it refused one thing"
    );
}

#[tokio::test]
async fn a_peer_that_cannot_refresh_the_list_stops_honoring_tokens() {
    // The rule the whole scheme rests on. Without it, revocation is defeated by making the list
    // unreachable — which is a capability any network position between two servers has, and a
    // capability the *revoked* peer is the most motivated to use.
    let fixture = Fixture::working();
    let now = fixture.clock.now();
    let expires_at = now + SignedDuration::from_hours(6);

    let fetched = fetch_list(&fixture).await;

    // The peer keeps holding its cached copy while the issuer becomes unreachable.
    let past_the_bound = now + MAX_STALENESS + SignedDuration::from_secs(1);
    assert_eq!(
        check_revocation(&fetched, "never-revoked", expires_at, past_the_bound),
        RevocationVerdict::Stale
    );
    assert_eq!(
        check_revocation(
            &fetched,
            "never-revoked",
            expires_at,
            now + MAX_STALENESS - SignedDuration::from_secs(1)
        ),
        RevocationVerdict::Honored,
        "the fifteen minutes are a permitted latency, not a margin to be conservative inside"
    );
}

/// Fetch the published list and read it back as the record a peer decides against.
///
/// Goes through the served JSON rather than through the port, so what the peer consumes is what
/// the wire actually carries — a shape the endpoint could break without any in-process test of
/// the store noticing.
async fn fetch_list(fixture: &Fixture) -> PublishedRevocations {
    let body = record(fixture, "/.well-known/capsule/revoked-jti").await;
    PublishedRevocations {
        generated_at: body["generated_at"]
            .as_str()
            .expect("generated_at is a string")
            .parse::<Timestamp>()
            .expect("generated_at is RFC 3339"),
        revoked: body["revoked"]
            .as_array()
            .expect("revoked is an array")
            .iter()
            .map(|entry| RevokedToken {
                jti: entry["jti"].as_str().expect("jti is a string").to_owned(),
                expires_at: entry["expires_at"]
                    .as_str()
                    .expect("expires_at is a string")
                    .parse::<Timestamp>()
                    .expect("expires_at is RFC 3339"),
            })
            .collect(),
    }
}

#[tokio::test]
async fn a_list_that_cannot_be_read_is_a_refusal_not_an_empty_list() {
    // The `503` is a claim about what the record means. An empty list is the strongest possible
    // statement this endpoint can make — *nothing is revoked* — so serving one during a storage
    // outage would turn an outage into a silent un-revocation of every token a peer holds. The
    // peer's own fail-closed rule cannot save it: a fresh, well-formed, empty list is exactly
    // what that rule tells it to believe.
    let fixture = Fixture::working();
    fixture
        .revocations
        .revoke(RevokedToken {
            jti: "still-revoked".to_owned(),
            expires_at: fixture.clock.now() + SignedDuration::from_hours(2),
        })
        .await
        .expect("the revocation is recorded");

    fixture.revocations.set_unavailable(true);

    let body: Value = fixture
        .client
        .get("/.well-known/capsule/revoked-jti")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::SERVICE_UNAVAILABLE)
        .json();

    assert_eq!(body["code"], "error.federation.revocations_unavailable");
}

#[tokio::test]
async fn the_published_login_endpoint_is_one_the_server_actually_serves() {
    // The record names three URLs that are literals here and literals again in the route
    // attributes, with no type binding the two. This is what keeps that duplication honest: it
    // posts to the *published* login URL and asserts the server answers it. A renamed or moved
    // route turns this into a 404 rather than into a discovery record that quietly sends every
    // new client somewhere that does not exist.
    let fixture = Fixture::working();
    let body = record(&fixture, "/.well-known/capsule/server-info").await;

    let login = body["auth"]["login"]
        .as_str()
        .expect("the login endpoint is a string")
        .to_owned();
    assert_eq!(login, "https://capsule.test/v1/auth/login");

    let path = login
        .strip_prefix("https://capsule.test")
        .expect("the published URL is under the advertised base");
    fixture
        .client
        .post(path)
        .header("accept", "application/json")
        .json(&serde_json::json!({ "email": support::EMAIL, "password": support::PASSWORD }))
        .send()
        .await
        .assert_status(StatusCode::OK);
}
