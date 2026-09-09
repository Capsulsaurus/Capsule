//! Federation (`S-E2`, `S-E5`, `S-C49`), end to end: a peer server pulls a shared album through
//! the **existing** read primitives with a capability, and the lifecycle around that grant.
//!
//! The peer is stood up as what a peer is here — a holder of a capability this server minted,
//! presenting it on `GET /v1/sync?album_id=` and `GET /v1/blob/{hash}` — rather than as a whole
//! second deployment: every rule under test is a property of the credential and the records
//! behind it. What is asserted against the *stores* is asserted against the stores the server
//! actually read, never against a second reading of the response body.
//!
//! The cases named `E2E case 4` are the server half of module-map.md's fourth case; the client
//! half (Bob's client renders) is `capsule-e2e`'s.

mod support;

use capsule_server::blob::{BlobStore, ContentAddress};
use capsule_server::counter::{CounterKey, CounterStore as _, budgets};
use capsule_server::federation::{
    CapabilityCodec, CapabilityRecord, CapabilityStore as _, MintRequest, PeerId, PeerStore as _,
    Scope,
};
use capsule_server::index::{AssetIndex, BlobRecord, PendingAsset};
use capsule_server::membership::{MemberRole, MembershipStore as _, RosterRecord};
use capsule_server::store::{AlbumId, AssetId, BlobRole, Clock as _, UserId};
use capsule_server::sync::CursorScope;
use jiff::{SignedDuration, Timestamp};
use kynos::http::StatusCode;
use serde_json::Value;
use support::{Fixture, PROTOCOL_VERSION, SERVER_ORIGIN, album, owner, second_album};

/// The peer server Bob's account lives on.
const PEER: &str = "other.test";

/// Bob, the member the owner shares with, as the owner lists him on the roster.
const BOB: &str = "01937b7c-0000-7000-8000-0000000000b0";

/// Put `bytes` in the blob store at their own address and return it.
async fn store_blob(fixture: &Fixture, bytes: &[u8]) -> ContentAddress {
    let address = ContentAddress::parse(&support::checksum(bytes)).expect("a content address");
    fixture
        .blobs
        .put(&address, bytes)
        .await
        .expect("the in-memory store accepts");
    address
}

/// Record one finalized blob against `asset`.
async fn record(fixture: &Fixture, asset: &AssetId, role: BlobRole, address: &ContentAddress) {
    fixture
        .index
        .record_blob(
            asset,
            BlobRecord {
                role,
                address: address.clone(),
                size: 32,
                manifest_sha256: None,
                finalized_at: Timestamp::UNIX_EPOCH,
            },
        )
        .await
        .expect("the index records");
}

/// Publish `asset` into `into` with a provenance blob behind it; returns the sequence number.
async fn publish_into(fixture: &Fixture, asset: &str, into: &AlbumId) -> u64 {
    let id = AssetId::new(asset);
    fixture
        .index
        .reserve(PendingAsset {
            asset_id: id.clone(),
            owner_id: owner(),
            album_id: into.clone(),
            protocol_version: PROTOCOL_VERSION.to_owned(),
            crypto_suite_id: 1,
            created_at: Timestamp::UNIX_EPOCH,
        })
        .await
        .expect("the index reserves");
    let provenance = store_blob(fixture, format!("manifest-{asset}").as_bytes()).await;
    record(fixture, &id, BlobRole::Provenance, &provenance).await;
    let metadata = store_blob(fixture, format!("metadata-{asset}").as_bytes()).await;
    match fixture
        .index
        .record_blob(
            &id,
            BlobRecord {
                role: BlobRole::Metadata,
                address: metadata,
                size: 32,
                manifest_sha256: None,
                finalized_at: Timestamp::UNIX_EPOCH,
            },
        )
        .await
        .expect("the index records")
    {
        capsule_server::index::BlobOutcome::Recorded {
            minted: Some(seq), ..
        } => seq,
        other => panic!("landing the index tier answered {other:?}"),
    }
}

/// Provision the seeded album to the seeded account.
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

/// The seeded album's roster at `version` (and epoch `version`), naming `members`.
async fn roster(fixture: &Fixture, version: u64, members: &[(&str, MemberRole)]) {
    fixture
        .members
        .apply_roster(
            RosterRecord {
                album_id: album(),
                roster_version: version,
                amk_epoch: version,
                attested_by_device: support::device(),
                received_at: Timestamp::UNIX_EPOCH,
                document: format!("federation-test-v{version}").into_bytes(),
            },
            members
                .iter()
                .map(|(user, role)| (UserId::new(*user), *role))
                .collect(),
        )
        .await
        .expect("the store applies");
}

/// A capability this server minted for `peer` over the seeded album, carrying Bob's membership
/// at `granted_epoch`, recorded exactly as the mint route records one.
///
/// Returns the bearer header value and the `jti`.
async fn capability(
    fixture: &Fixture,
    peer: &str,
    scope: Scope,
    granted_epoch: u64,
) -> (String, String) {
    capability_over(
        fixture,
        &fixture.codec,
        peer,
        &album(),
        scope,
        granted_epoch,
    )
    .await
}

/// As [`capability`], over `codec` and `album`.
async fn capability_over(
    fixture: &Fixture,
    codec: &CapabilityCodec,
    peer: &str,
    album: &AlbumId,
    scope: Scope,
    granted_epoch: u64,
) -> (String, String) {
    let minted = codec
        .mint(&MintRequest {
            peer: PeerId::new(peer),
            album: album.clone(),
            scope,
            min_protocol_version: PROTOCOL_VERSION.to_owned(),
            ttl: SignedDuration::from_hours(6),
        })
        .expect("it mints");
    fixture
        .revocations
        .issue(CapabilityRecord {
            jti: minted.grant.jti.clone(),
            album_id: album.clone(),
            peer_id: PeerId::new(peer),
            member: UserId::new(BOB),
            scope,
            granted_epoch,
            min_protocol_version: PROTOCOL_VERSION.to_owned(),
            issued_at: minted.grant.issued_at,
            expires_at: minted.grant.expires_at,
            revoked_at: None,
            refreshed_to: None,
        })
        .await
        .expect("the store records");
    (format!("Bearer {}", minted.token), minted.grant.jti)
}

/// A fixture with the album provisioned, Bob on its roster at epoch 1, two assets in the
/// album and one elsewhere; returns the fixture and the album's two sequence numbers.
async fn shared() -> (Fixture, Vec<u64>) {
    let fixture = Fixture::working();
    let owner_bearer = fixture.bearer().await;
    provision(&fixture, &owner_bearer).await;
    let first = publish_into(&fixture, "shared-1", &album()).await;
    publish_into(&fixture, "private-1", &second_album()).await;
    let third = publish_into(&fixture, "shared-2", &album()).await;
    roster(&fixture, 1, &[(BOB, MemberRole::Reader)]).await;
    (fixture, vec![first, third])
}

/// Ask for a page as `bearer` and return the raw response.
async fn page(fixture: &Fixture, bearer: &str, query: &str) -> kynos::test::TestResponse {
    let path = if query.is_empty() {
        "/v1/sync".to_owned()
    } else {
        format!("/v1/sync?{query}")
    };
    fixture
        .client
        .get(&path)
        .header("authorization", bearer)
        .header("accept", "application/json")
        .send()
        .await
}

/// The seeded album's page query.
fn album_query() -> String {
    format!("album_id={}", album())
}

// ===========================================================================================
// The pull path: the sync feed under a capability (S-E5)
// ===========================================================================================

/// E2E case 4 (server half): a peer holding a capability pulls the album's page.
#[tokio::test]
async fn a_peer_pulls_the_albums_page_with_a_capability() {
    let (fixture, expected) = shared().await;
    let (bearer, _) = capability(&fixture, PEER, Scope::Read, 1).await;

    let response = page(&fixture, &bearer, &album_query()).await;
    response.assert_status(StatusCode::OK);
    let body: Value = response.json();
    let entries = body["entries"].as_array().expect("an array");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry["sync_seq"].as_u64().expect("a position"))
            .collect::<Vec<_>>(),
        expected,
        "the owner's sequence, filtered to the album, in order"
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry["album_id"] == album().as_str()),
        "nothing from the owner's other albums"
    );
    assert_eq!(body["has_more"], false);

    // The cursor is the peer's own: it decodes for `(peer, album)` and resumes there.
    let cursor = body["next_cursor"].as_str().expect("a cursor").to_owned();
    assert_eq!(
        fixture.cursors.decode(
            &CursorScope::peer(&PeerId::new(PEER), &album()),
            Some(&cursor)
        ),
        Ok(expected[1])
    );
    let resumed = page(
        &fixture,
        &bearer,
        &format!("{}&cursor={cursor}", album_query()),
    )
    .await;
    resumed.assert_status(StatusCode::OK);
    let resumed: Value = resumed.json();
    assert!(resumed["entries"].as_array().expect("an array").is_empty());

    // The peer's cursor is nobody else's: not Bob's own album page, and not another peer's.
    let bob = fixture.other_bearer(BOB).await;
    let crossed = page(
        &fixture,
        &bob,
        &format!("{}&cursor={cursor}", album_query()),
    )
    .await;
    crossed.assert_status(StatusCode::BAD_REQUEST);
    let crossed: Value = crossed.json();
    assert_eq!(crossed["code"], "error.sync.cursor_invalid");
    let (other_peer, _) = capability(&fixture, "third.test", Scope::Read, 1).await;
    let crossed = page(
        &fixture,
        &other_peer,
        &format!("{}&cursor={cursor}", album_query()),
    )
    .await;
    crossed.assert_status(StatusCode::BAD_REQUEST);
    let crossed: Value = crossed.json();
    assert_eq!(crossed["code"], "error.sync.cursor_invalid");
}

#[tokio::test]
async fn a_session_token_on_the_feed_is_unchanged_by_the_capability_arm() {
    // The account arm is the same code path it was: the owner's feed, and Bob's album page.
    let (fixture, expected) = shared().await;
    let owner_bearer = fixture.bearer().await;
    let own = page(&fixture, &owner_bearer, "").await;
    own.assert_status(StatusCode::OK);
    let own: Value = own.json();
    assert_eq!(own["entries"].as_array().expect("an array").len(), 3);

    let bob = fixture.other_bearer(BOB).await;
    let body = page(&fixture, &bob, &album_query()).await;
    body.assert_status(StatusCode::OK);
    let body: Value = body.json();
    assert_eq!(
        body["entries"]
            .as_array()
            .expect("an array")
            .iter()
            .map(|entry| entry["sync_seq"].as_u64().expect("a position"))
            .collect::<Vec<_>>(),
        expected
    );
}

#[tokio::test]
async fn a_capability_for_another_album_or_no_album_is_an_audience_mismatch() {
    let (fixture, _) = shared().await;
    let (bearer, _) = capability(&fixture, PEER, Scope::Read, 1).await;

    // A peer has no "own feed".
    let none = page(&fixture, &bearer, "").await;
    none.assert_status(StatusCode::FORBIDDEN);
    let none: Value = none.json();
    assert_eq!(none["code"], "error.federation.audience_mismatch");

    // And the capability is for one album only, whether or not the other exists.
    let other = page(&fixture, &bearer, &format!("album_id={}", second_album())).await;
    other.assert_status(StatusCode::FORBIDDEN);
    let other: Value = other.json();
    assert_eq!(other["code"], "error.federation.audience_mismatch");
}

#[tokio::test]
async fn a_capability_whose_member_left_the_roster_is_refused_and_a_re_admission_needs_a_fresh_one()
{
    // The epoch is the server-side half of the grant. A member removed and re-admitted at a
    // later epoch gets a fresh membership; the old capability was minted for one that ended.
    let (fixture, _) = shared().await;
    let (old, _) = capability(&fixture, PEER, Scope::Read, 1).await;
    page(&fixture, &old, &album_query())
        .await
        .assert_status(StatusCode::OK);

    roster(&fixture, 2, &[]).await;
    let removed = page(&fixture, &old, &album_query()).await;
    removed.assert_status(StatusCode::FORBIDDEN);
    let removed: Value = removed.json();
    assert_eq!(removed["code"], "error.sync.album_access_denied");

    roster(&fixture, 3, &[(BOB, MemberRole::Reader)]).await;
    let stale = page(&fixture, &old, &album_query()).await;
    stale.assert_status(StatusCode::FORBIDDEN);
    let stale: Value = stale.json();
    assert_eq!(
        stale["code"], "error.sync.album_access_denied",
        "re-admitted at epoch 3, and the grant was for epoch 1"
    );

    let (fresh, _) = capability(&fixture, PEER, Scope::Read, 3).await;
    page(&fixture, &fresh, &album_query())
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_revoked_capability_is_refused_with_its_code_and_appears_on_the_list() {
    let (fixture, _) = shared().await;
    let (bearer, jti) = capability(&fixture, PEER, Scope::Read, 1).await;
    page(&fixture, &bearer, &album_query())
        .await
        .assert_status(StatusCode::OK);

    fixture
        .revocations
        .revoke_issued(&jti, fixture.clock.now())
        .await
        .expect("revokes");

    let refused = page(&fixture, &bearer, &album_query()).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.capability_revoked");
    assert_eq!(
        fixture
            .counters
            .peek(
                &CounterKey::PeerRequests(PEER.to_owned()),
                budgets::PEER_REQUESTS,
                fixture.clock.now(),
            )
            .await
            .expect("the counter answers"),
        capsule_server::counter::Verdict::Admitted {
            remaining: budgets::PEER_REQUESTS.limit - 1
        },
        "the admitted page was charged; the revoked presentation was not"
    );

    // And a peer polling the published list sees the same fact.
    let list: Value = fixture
        .client
        .get("/.well-known/capsule/revoked-jti")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert!(
        list["revoked"]
            .as_array()
            .expect("an array")
            .iter()
            .any(|token| token["jti"] == jti),
        "{list}"
    );
}

#[tokio::test]
async fn a_blocked_peer_is_refused_before_anything_is_charged() {
    let (fixture, _) = shared().await;
    let (bearer, _) = capability(&fixture, PEER, Scope::Read, 1).await;
    fixture
        .peers
        .block(&PeerId::new(PEER), fixture.clock.now(), None)
        .await
        .expect("blocks");

    let refused = page(&fixture, &bearer, &album_query()).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.moderation.server_blocked");
    assert!(
        fixture
            .counters
            .peek(
                &CounterKey::PeerRequests(PEER.to_owned()),
                budgets::PEER_REQUESTS,
                fixture.clock.now(),
            )
            .await
            .expect("the counter answers")
            == capsule_server::counter::Verdict::Admitted {
                remaining: budgets::PEER_REQUESTS.limit
            },
        "a blocked peer's request is not charged"
    );

    fixture
        .peers
        .unblock(&PeerId::new(PEER))
        .await
        .expect("unblocks");
    page(&fixture, &bearer, &album_query())
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_peers_budget_is_enforced_and_resets_with_the_window() {
    // Invariant 21. The budget is spent through the counter port directly up to its last hit —
    // ten thousand requests through the router would be a test of the router's speed — and the
    // last two are real requests, so the `429` is the route's.
    let (fixture, _) = shared().await;
    let (bearer, _) = capability(&fixture, PEER, Scope::Read, 1).await;
    let key = CounterKey::PeerRequests(PEER.to_owned());
    for _ in 1..budgets::PEER_REQUESTS.limit {
        fixture
            .counters
            .hit(&key, budgets::PEER_REQUESTS, fixture.clock.now())
            .await
            .expect("the counter answers");
    }

    page(&fixture, &bearer, &album_query())
        .await
        .assert_status(StatusCode::OK);
    let refused = page(&fixture, &bearer, &album_query()).await;
    refused.assert_status(StatusCode::TOO_MANY_REQUESTS);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.rate_budget_exceeded");

    // Another peer is its own boundary.
    let (other, _) = capability(&fixture, "third.test", Scope::Read, 1).await;
    page(&fixture, &other, &album_query())
        .await
        .assert_status(StatusCode::OK);

    fixture.clock.advance(budgets::PEER_REQUESTS.window);
    page(&fixture, &bearer, &album_query())
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_capability_this_server_did_not_issue_is_unauthenticated() {
    let (fixture, _) = shared().await;

    // Another server's key, claiming to be this one.
    let foreign = CapabilityCodec::from_pkcs8(
        &support::signing_key_der(),
        SERVER_ORIGIN,
        fixture.clock.clone(),
    )
    .expect("a key parses");
    let forged = foreign
        .mint(&MintRequest {
            peer: PeerId::new(PEER),
            album: album(),
            scope: Scope::Read,
            min_protocol_version: PROTOCOL_VERSION.to_owned(),
            ttl: SignedDuration::from_hours(1),
        })
        .expect("it mints");
    page(
        &fixture,
        &format!("Bearer {}", forged.token),
        &album_query(),
    )
    .await
    .assert_status(StatusCode::UNAUTHORIZED);

    // This server's key, but a jti the store never recorded: minted and thrown away.
    let unrecorded = fixture
        .codec
        .mint(&MintRequest {
            peer: PeerId::new(PEER),
            album: album(),
            scope: Scope::Read,
            min_protocol_version: PROTOCOL_VERSION.to_owned(),
            ttl: SignedDuration::from_hours(1),
        })
        .expect("it mints");
    page(
        &fixture,
        &format!("Bearer {}", unrecorded.token),
        &album_query(),
    )
    .await
    .assert_status(StatusCode::UNAUTHORIZED);

    // And an expired one, on the clock.
    let (bearer, _) = capability(&fixture, PEER, Scope::Read, 1).await;
    fixture.clock.advance(SignedDuration::from_hours(7));
    page(&fixture, &bearer, &album_query())
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_store_that_cannot_answer_a_peer_is_an_outage_never_an_admission() {
    let (fixture, _) = shared().await;
    let (bearer, _) = capability(&fixture, PEER, Scope::Read, 1).await;

    // The capability store: the authenticator can render only `401`, and does, closed.
    fixture.revocations.set_unavailable(true);
    page(&fixture, &bearer, &album_query())
        .await
        .assert_status(StatusCode::UNAUTHORIZED);
    fixture.revocations.set_unavailable(false);

    // The membership store, at the route: a coded `500`.
    fixture.members.set_unavailable(true);
    let refused = page(&fixture, &bearer, &album_query()).await;
    refused.assert_status(StatusCode::INTERNAL_SERVER_ERROR);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.sync.unavailable");
    fixture.members.set_unavailable(false);
}
