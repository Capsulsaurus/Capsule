//! Subscription intake: a member posts the capability an album's home server minted for this
//! server, and this server records it only once it verifies under the issuer's pinned key and
//! names this server as its recipient (#1035).
//!
//! Run with `cargo nextest run -p capsule-server`.

mod support;

use capsule_server::federation::pull::{JobKey, JobSource as _};
use capsule_server::federation::subscriptions::MAX_SUBSCRIPTIONS_PER_ACCOUNT;
use capsule_server::federation::{
    CapabilityCodec, CapabilityToken, MintRequest, PeerId, PeerStore as _, Scope, Subscription,
    SubscriptionJobs, SubscriptionStore as _,
};
use capsule_server::store::{AlbumId, UserId};
use jiff::SignedDuration;
use kynos::http::StatusCode;
use serde_json::{Value, json};
use support::{Fixture, PROTOCOL_VERSION, SERVER_ORIGIN};

/// The album's home server in every case.
const HOME: &str = "home.peer.test";

/// The album every case subscribes to, as its home server names it.
const ALBUM: &str = "01937b7c-0000-7000-8000-00000000c0de";

/// A second local account, for the isolation cases.
const OTHER_MEMBER: &str = "01937b7c-0000-7000-8000-0000000000e1";

/// A home server: its codec, signing as `origin`.
fn home(fixture: &Fixture, origin: &str) -> CapabilityCodec {
    let der = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .expect("the platform generates a key");
    CapabilityCodec::from_pkcs8(der.as_ref(), origin, fixture.clock.clone())
        .expect("a fresh key parses")
}

/// Pin `codec`'s key as its server's, as an operator's `capsule-server peer pin` does.
async fn pin(fixture: &Fixture, codec: &CapabilityCodec) {
    let key: [u8; 32] = codec.public_key().try_into().expect("thirty-two bytes");
    fixture
        .peers
        .pin(&PeerId::new(codec.server_id()), key, fixture.now())
        .await
        .expect("the operator pins");
}

/// A capability `codec` minted for `recipient` over [`ALBUM`], good for `hours`.
fn invite(codec: &CapabilityCodec, recipient: &str, hours: i64) -> String {
    codec
        .mint(&MintRequest {
            peer: PeerId::new(recipient),
            album: AlbumId::new(ALBUM),
            scope: Scope::Read,
            min_protocol_version: PROTOCOL_VERSION.to_owned(),
            ttl: SignedDuration::from_hours(hours),
        })
        .expect("it mints")
        .token
}

/// `POST /v1/federation/subscriptions` as `bearer`.
async fn subscribe(
    fixture: &Fixture,
    bearer: &str,
    issuer: &str,
    token: &str,
) -> kynos::test::TestResponse {
    fixture
        .client
        .post("/v1/federation/subscriptions")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .json(&json!({ "issuer": issuer, "token": token }))
        .send()
        .await
}

/// The coded refusal `response` carries, asserting its status.
fn code(response: kynos::test::TestResponse, status: StatusCode) -> String {
    let body: Value = response.assert_status(status).json();
    body["code"].as_str().expect("a coded refusal").to_owned()
}

/// The caller's subscriptions, as the list answers them.
async fn listed(fixture: &Fixture, bearer: &str) -> Vec<Value> {
    let body: Value = fixture
        .client
        .get("/v1/federation/subscriptions")
        .header("authorization", bearer)
        .header("accept", "application/json")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    body["subscriptions"].as_array().expect("a list").to_owned()
}

#[tokio::test]
async fn a_verified_capability_is_recorded_and_feeds_one_job() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let codec = home(&fixture, HOME);
    pin(&fixture, &codec).await;

    let token = invite(&codec, SERVER_ORIGIN, 6);
    let created: Value = subscribe(&fixture, &bearer, "Home.Peer.Test.", &token)
        .await
        .assert_status(StatusCode::CREATED)
        .json();
    assert_eq!(created["peer"], HOME, "the issuer is folded");
    assert_eq!(created["album_id"], ALBUM);
    assert_eq!(created["scope"], "read");
    assert_eq!(created["live"], true);
    assert!(
        !created.to_string().contains(&token),
        "the token is never answered back"
    );

    assert_eq!(listed(&fixture, &bearer).await, vec![created]);
    let jobs = SubscriptionJobs::new(fixture.subscriptions.clone(), fixture.clock.clone());
    assert_eq!(
        jobs.jobs().await.expect("the source lists"),
        vec![JobKey::new(PeerId::new(HOME), AlbumId::new(ALBUM))]
    );
}

#[tokio::test]
async fn a_fresher_invite_replaces_the_token_and_a_replayed_one_changes_nothing() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let codec = home(&fixture, HOME);
    pin(&fixture, &codec).await;

    let first = invite(&codec, SERVER_ORIGIN, 1);
    let created: Value = subscribe(&fixture, &bearer, HOME, &first)
        .await
        .assert_status(StatusCode::CREATED)
        .json();

    fixture.clock.advance(SignedDuration::from_mins(30));
    // A fresh session token: the first one's life is shorter than the walk the clock took.
    let bearer = fixture.bearer().await;
    let fresher = invite(&codec, SERVER_ORIGIN, 6);
    let replaced: Value = subscribe(&fixture, &bearer, HOME, &fresher)
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_ne!(
        replaced["jti"], created["jti"],
        "the longer-lived grant won"
    );
    assert_eq!(
        replaced["subscribed_at"], created["subscribed_at"],
        "the subscription's age is kept"
    );

    let kept: Value = subscribe(&fixture, &bearer, HOME, &first)
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(kept, replaced, "a replayed older invite changes nothing");

    let held = fixture
        .subscriptions
        .of_subscriber(&support::user())
        .await
        .expect("the store answers");
    assert_eq!(held.len(), 1);
    assert_eq!(
        held[0].token.expose(),
        fresher,
        "the stored token is the fresher one"
    );
}

#[tokio::test]
async fn a_capability_this_server_cannot_present_is_refused_and_records_nothing() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let codec = home(&fixture, HOME);

    // An issuer nobody pinned.
    let token = invite(&codec, SERVER_ORIGIN, 6);
    assert_eq!(
        code(
            subscribe(&fixture, &bearer, HOME, &token).await,
            StatusCode::FORBIDDEN
        ),
        "error.federation.peer_unknown"
    );
    pin(&fixture, &codec).await;

    // Issued to another server.
    let elsewhere = invite(&codec, "third.test", 6);
    assert_eq!(
        code(
            subscribe(&fixture, &bearer, HOME, &elsewhere).await,
            StatusCode::FORBIDDEN
        ),
        "error.federation.capability_invalid"
    );

    // Signed by a key that is not the pinned one, under the pinned issuer's name.
    let impostor = home(&fixture, HOME);
    let forged = invite(&impostor, SERVER_ORIGIN, 6);
    assert_eq!(
        code(
            subscribe(&fixture, &bearer, HOME, &forged).await,
            StatusCode::FORBIDDEN
        ),
        "error.federation.capability_invalid"
    );

    // Named under another issuer than the one that signed it.
    let other = home(&fixture, "other.peer.test");
    pin(&fixture, &other).await;
    assert_eq!(
        code(
            subscribe(&fixture, &bearer, "other.peer.test", &token).await,
            StatusCode::FORBIDDEN
        ),
        "error.federation.capability_invalid"
    );

    // Expired by the time it is posted.
    let short = invite(&codec, SERVER_ORIGIN, 1);
    fixture.clock.advance(SignedDuration::from_hours(2));
    let bearer = fixture.bearer().await;
    assert_eq!(
        code(
            subscribe(&fixture, &bearer, HOME, &short).await,
            StatusCode::FORBIDDEN
        ),
        "error.federation.capability_expired"
    );

    // Not a token at all, an issuer that is not a host name, this server as the issuer, and a
    // token past the bound.
    for (issuer, token) in [
        (HOME, "not-a-jwt".to_owned()),
        ("https://home.peer.test/", token.clone()),
        (SERVER_ORIGIN, token.clone()),
        (HOME, "a".repeat(4097)),
        (HOME, String::new()),
    ] {
        let response = subscribe(&fixture, &bearer, issuer, &token).await;
        let status = response.status();
        let body: Value = response.json();
        if token == "not-a-jwt" {
            assert_eq!(status, StatusCode::FORBIDDEN, "{issuer}: {body}");
            assert_eq!(body["code"], "error.federation.capability_invalid");
        } else {
            assert_eq!(status, StatusCode::BAD_REQUEST, "{issuer}: {body}");
            assert_eq!(body["code"], "error.federation.capability_malformed");
        }
    }

    // A blocked issuer, whose token would otherwise verify.
    fixture
        .peers
        .block(&PeerId::new(HOME), fixture.now(), None)
        .await
        .expect("the operator blocks");
    let fresh = invite(&codec, SERVER_ORIGIN, 6);
    assert_eq!(
        code(
            subscribe(&fixture, &bearer, HOME, &fresh).await,
            StatusCode::FORBIDDEN
        ),
        "error.moderation.server_blocked"
    );

    assert!(
        listed(&fixture, &bearer).await.is_empty(),
        "no refusal recorded anything"
    );
}

#[tokio::test]
async fn a_lapsed_subscription_is_listed_and_is_no_job() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let codec = home(&fixture, HOME);
    pin(&fixture, &codec).await;
    subscribe(&fixture, &bearer, HOME, &invite(&codec, SERVER_ORIGIN, 1))
        .await
        .assert_status(StatusCode::CREATED);

    fixture.clock.advance(SignedDuration::from_hours(1));
    let bearer = fixture.bearer().await;
    let held = listed(&fixture, &bearer).await;
    assert_eq!(held.len(), 1);
    assert_eq!(
        held[0]["live"], false,
        "a lapsed capability is shown as one"
    );
    let jobs = SubscriptionJobs::new(fixture.subscriptions.clone(), fixture.clock.clone());
    assert!(jobs.jobs().await.expect("the source lists").is_empty());

    // A fresh invite revives it.
    subscribe(&fixture, &bearer, HOME, &invite(&codec, SERVER_ORIGIN, 6))
        .await
        .assert_status(StatusCode::OK);
    assert_eq!(jobs.jobs().await.expect("the source lists").len(), 1);
}

#[tokio::test]
async fn members_subscribe_and_unsubscribe_apart() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let other = fixture.other_bearer(OTHER_MEMBER).await;
    let codec = home(&fixture, HOME);
    pin(&fixture, &codec).await;
    for caller in [&bearer, &other] {
        subscribe(&fixture, caller, HOME, &invite(&codec, SERVER_ORIGIN, 6))
            .await
            .assert_status(StatusCode::CREATED);
    }
    let jobs = SubscriptionJobs::new(fixture.subscriptions.clone(), fixture.clock.clone());
    assert_eq!(
        jobs.jobs().await.expect("the source lists").len(),
        1,
        "two members of one album are one job"
    );

    let path = format!("/v1/federation/subscriptions/Home.Peer.Test/{ALBUM}");
    for _ in 0..2 {
        fixture
            .client
            .delete(&path)
            .header("authorization", &bearer)
            .header("x-capsule-protocol", PROTOCOL_VERSION)
            .send()
            .await
            .assert_status(StatusCode::NO_CONTENT);
    }
    assert!(listed(&fixture, &bearer).await.is_empty());
    assert_eq!(
        listed(&fixture, &other).await.len(),
        1,
        "another member's subscription to the same album stays"
    );
    assert_eq!(
        jobs.jobs().await.expect("the source lists").len(),
        1,
        "the album is still pulled for the member who stayed"
    );
    let held = fixture
        .subscriptions
        .of_subscriber(&UserId::new(OTHER_MEMBER))
        .await
        .expect("the store answers");
    assert_eq!(held.len(), 1);
}

#[tokio::test]
async fn an_account_at_its_limit_is_refused_a_new_album_but_renews_a_held_one() {
    let fixture = Fixture::working();
    let member = fixture.other_bearer(OTHER_MEMBER).await;
    let codec = home(&fixture, HOME);
    pin(&fixture, &codec).await;
    let now = fixture.now();
    for index in 0..MAX_SUBSCRIPTIONS_PER_ACCOUNT {
        fixture
            .subscriptions
            .put(Subscription {
                subscriber: UserId::new(OTHER_MEMBER),
                peer: PeerId::new(HOME),
                album: AlbumId::new(format!("held-{index:04}")),
                scope: Scope::Read,
                jti: format!("held-{index:04}"),
                token: CapabilityToken::new("header.payload.signature"),
                issued_at: now,
                expires_at: jiff::Timestamp::MAX,
                min_protocol_version: PROTOCOL_VERSION.to_owned(),
                subscribed_at: now,
                updated_at: now,
            })
            .await
            .expect("the store records");
    }

    assert_eq!(
        code(
            subscribe(&fixture, &member, HOME, &invite(&codec, SERVER_ORIGIN, 6)).await,
            StatusCode::FORBIDDEN
        ),
        "error.federation.subscription_limit"
    );
    assert_eq!(
        listed(&fixture, &member).await.len(),
        MAX_SUBSCRIPTIONS_PER_ACCOUNT as usize,
        "a refusal records nothing"
    );

    // Unsubscribing frees a place, and the album it takes is then renewed at the limit.
    fixture
        .client
        .delete("/v1/federation/subscriptions/home.peer.test/held-0000")
        .header("authorization", &member)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    subscribe(&fixture, &member, HOME, &invite(&codec, SERVER_ORIGIN, 1))
        .await
        .assert_status(StatusCode::CREATED);
    subscribe(&fixture, &member, HOME, &invite(&codec, SERVER_ORIGIN, 6))
        .await
        .assert_status(StatusCode::OK);

    // Another account's limit is its own.
    let bearer = fixture.bearer().await;
    subscribe(&fixture, &bearer, HOME, &invite(&codec, SERVER_ORIGIN, 6))
        .await
        .assert_status(StatusCode::CREATED);
}

#[tokio::test]
async fn a_deployment_that_does_not_federate_takes_no_subscription() {
    let fixture = Fixture::without_federation();
    let bearer = fixture.bearer().await;
    let codec = home(&fixture, HOME);
    pin(&fixture, &codec).await;
    assert_eq!(
        code(
            subscribe(&fixture, &bearer, HOME, &invite(&codec, SERVER_ORIGIN, 6)).await,
            StatusCode::NOT_FOUND
        ),
        "error.federation.not_configured"
    );
    let jobs = SubscriptionJobs::new(fixture.subscriptions.clone(), fixture.clock.clone());
    assert!(jobs.jobs().await.expect("the source lists").is_empty());
    for path in [
        format!("/v1/federation/subscriptions/{HOME}/{ALBUM}/assets"),
        format!("/v1/federation/subscriptions/{HOME}/{ALBUM}/assets/a/metadata"),
    ] {
        assert_eq!(
            code(read(&fixture, &bearer, &path).await, StatusCode::NOT_FOUND),
            "error.federation.not_configured",
            "{path}"
        );
    }
}

// ── What a subscriber reads of a pulled album (#1160) ───────────────────────────────────

/// The pulled album's asset ids, in the byte order the listing answers them in.
const PULLED: [&str; 3] = [
    "01937b7c-0000-7000-8000-00000000a001",
    "01937b7c-0000-7000-8000-00000000a002",
    "01937b7c-0000-7000-8000-00000000a003",
];

/// The metadata blob [`PULLED`]`[index]` carries.
fn metadata(index: usize) -> Vec<u8> {
    format!("sealed metadata of asset {index}").into_bytes()
}

/// `GET path` as `bearer`.
async fn read(fixture: &Fixture, bearer: &str, path: &str) -> kynos::test::TestResponse {
    fixture
        .client
        .get(path)
        .header("authorization", bearer)
        .header("accept", "application/json, application/octet-stream")
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
}

/// The listing of [`ALBUM`]'s pulled assets with `query`, as `bearer`.
async fn pulled_assets(fixture: &Fixture, bearer: &str, query: &str) -> Value {
    read(
        fixture,
        bearer,
        &format!("/v1/federation/subscriptions/{HOME}/{ALBUM}/assets{query}"),
    )
    .await
    .assert_status(StatusCode::OK)
    .json()
}

/// The asset ids a listing answered.
fn ids(listing: &Value) -> Vec<&str> {
    listing["assets"]
        .as_array()
        .expect("a list")
        .iter()
        .map(|asset| asset["asset_id"].as_str().expect("an id"))
        .collect()
}

/// Commit one page pulling [`ALBUM`] from [`HOME`]: every [`PULLED`] asset created with its
/// metadata blob, and then the last one deleted, as the pull worker commits what it validated.
async fn pull_album(fixture: &Fixture) {
    use capsule_core::crypto::hash::hash_bytes;
    use capsule_server::federation::pull::PulledAlbums as _;
    use capsule_server::federation::pull::pulled::{
        AcceptedEntry, Change, Committed, PulledAlbum, ValidatedPage,
    };

    let entry = |index: usize, sync_seq: u64| {
        let asset = uuid::Uuid::parse_str(PULLED[index]).expect("a uuid");
        let manifest = support::signed_create(
            &fixture.clock,
            asset,
            format!("original {index}").as_bytes(),
            &metadata(index),
        );
        AcceptedEntry {
            asset_id: PULLED[index].to_owned(),
            sync_seq,
            change: Change::Created,
            changed_at: fixture.now(),
            head: Some(hash_bytes(&manifest)),
            manifest: Some(manifest),
            metadata_blob: Some(hash_bytes(&metadata(index))),
            blobs: Vec::new(),
            derivative_manifests: Vec::new(),
            original_held: true,
            purged: false,
        }
    };
    let mut entries: Vec<AcceptedEntry> = (0..PULLED.len())
        .map(|index| entry(index, index as u64 + 1))
        .collect();
    let mut deleted = entry(2, 4);
    deleted.change = Change::Deleted;
    entries.push(deleted);
    let page = ValidatedPage {
        entries,
        metadata_blobs: (0..PULLED.len())
            .map(|index| (hash_bytes(&metadata(index)), metadata(index)))
            .collect(),
        state: PulledAlbum {
            cursor: Some("c4".to_owned()),
            high_water: Some(4),
            ..PulledAlbum::default()
        },
    };
    let job = JobKey::new(PeerId::new(HOME), AlbumId::new(ALBUM));
    assert_eq!(
        fixture
            .pulled
            .commit(&job, None, page)
            .await
            .expect("the store commits"),
        Committed::Written
    );
}

#[tokio::test]
async fn a_subscriber_lists_its_pulled_album_and_fetches_each_assets_metadata() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let codec = home(&fixture, HOME);
    pin(&fixture, &codec).await;
    subscribe(&fixture, &bearer, HOME, &invite(&codec, SERVER_ORIGIN, 6))
        .await
        .assert_status(StatusCode::CREATED);

    let empty = pulled_assets(&fixture, &bearer, "").await;
    assert_eq!(
        empty,
        json!({ "peer": HOME, "album_id": ALBUM, "assets": [], "next_after": null }),
        "a subscription nothing was pulled for yet is an empty album, not a refusal"
    );

    pull_album(&fixture).await;
    let listing = pulled_assets(&fixture, &bearer, "").await;
    assert_eq!(
        ids(&listing),
        PULLED[..2],
        "the deleted asset is not listed"
    );
    assert_eq!(listing["next_after"], Value::Null, "{listing:#}");
    let first = &listing["assets"][0];
    assert_eq!(first["size"], "original 0".len());
    assert_eq!(first["author"], support::user().as_str());
    assert_eq!(first["timestamp"], fixture.now().to_string());
    assert_eq!(first["head"].as_str().map(str::len), Some(64), "{first}");

    // A page at a time, resumed from the `next_after` each page answers.
    let page = pulled_assets(&fixture, &bearer, "?limit=1").await;
    assert_eq!(ids(&page), [PULLED[0]]);
    assert_eq!(page["next_after"], PULLED[0]);
    let page = pulled_assets(&fixture, &bearer, &format!("?limit=1&after={}", PULLED[0])).await;
    assert_eq!(ids(&page), [PULLED[1]]);
    let page = pulled_assets(&fixture, &bearer, &format!("?limit=1&after={}", PULLED[1])).await;
    assert!(ids(&page).is_empty());
    assert_eq!(page["next_after"], Value::Null, "the end of the album");

    for (index, asset) in PULLED[..2].iter().enumerate() {
        let path = format!("/v1/federation/subscriptions/{HOME}/{ALBUM}/assets/{asset}/metadata");
        let served = read(&fixture, &bearer, &path).await;
        served.assert_status(StatusCode::OK);
        assert_eq!(served.bytes().as_ref(), metadata(index).as_slice());
    }
    for asset in [PULLED[2], "never-pulled"] {
        let path = format!("/v1/federation/subscriptions/{HOME}/{ALBUM}/assets/{asset}/metadata");
        assert_eq!(
            code(read(&fixture, &bearer, &path).await, StatusCode::NOT_FOUND),
            "error.federation.album_not_found",
            "a deleted asset and one never pulled have no metadata: {asset}"
        );
    }
}

#[tokio::test]
async fn a_pulled_album_reads_only_for_its_subscribers_and_says_nothing_to_anyone_else() {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    let other = fixture.other_bearer(OTHER_MEMBER).await;
    let codec = home(&fixture, HOME);
    pin(&fixture, &codec).await;
    subscribe(&fixture, &bearer, HOME, &invite(&codec, SERVER_ORIGIN, 1))
        .await
        .assert_status(StatusCode::CREATED);
    pull_album(&fixture).await;

    let listing = format!("/v1/federation/subscriptions/{HOME}/{ALBUM}/assets");
    let blob = format!("{listing}/{}/metadata", PULLED[0]);
    let never =
        format!("/v1/federation/subscriptions/{HOME}/01937b7c-0000-7000-8000-00000000dead/assets");
    let refusal = |response: kynos::test::TestResponse| {
        let mut body: Value = response.assert_status(StatusCode::NOT_FOUND).json();
        body.as_object_mut().map(|body| body.remove("request_id"));
        body
    };
    // Another member: the album was pulled, but not for them. The same answer as an album
    // nobody ever pulled, so the read cannot tell them what this member subscribed to.
    let unsubscribed = refusal(read(&fixture, &other, &listing).await);
    assert_eq!(unsubscribed["code"], "error.federation.album_not_found");
    assert_eq!(unsubscribed, refusal(read(&fixture, &other, &never).await));
    assert_eq!(unsubscribed, refusal(read(&fixture, &other, &blob).await));
    assert_eq!(unsubscribed, refusal(read(&fixture, &bearer, &never).await));

    // A lapsed subscription still reads what its pulls committed.
    fixture.clock.advance(SignedDuration::from_hours(2));
    let bearer = fixture.bearer().await;
    assert_eq!(listed(&fixture, &bearer).await[0]["live"], false);
    assert_eq!(
        ids(&pulled_assets(&fixture, &bearer, "").await),
        PULLED[..2]
    );

    // Unsubscribing ends the reads; what was pulled is untouched for any other subscriber.
    fixture
        .client
        .delete(&format!("/v1/federation/subscriptions/{HOME}/{ALBUM}"))
        .header("authorization", &bearer)
        .header("x-capsule-protocol", PROTOCOL_VERSION)
        .send()
        .await
        .assert_status(StatusCode::NO_CONTENT);
    assert_eq!(
        unsubscribed,
        refusal(read(&fixture, &bearer, &listing).await)
    );
    assert_eq!(unsubscribed, refusal(read(&fixture, &bearer, &blob).await));
}
