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

use capsule_core::crypto::keys::HybridSigningKey;
use capsule_server::blob::{BlobStore, ContentAddress};
use capsule_server::counter::{CounterKey, CounterStore as _, budgets};
use capsule_server::federation::{
    CapabilityCodec, CapabilityRecord, CapabilityStore as _, FederationCollaborators,
    FederationContext, MintRequest, PeerId, PeerStore as _, Scope,
};
use capsule_server::index::{AssetIndex, BlobRecord, PendingAsset, ServingHold};
use capsule_server::membership::{MemberRole, MembershipStore as _, RosterRecord};
use capsule_server::moderation::ModerationStore as _;
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

/// A second member, for the cases that need a roster to change without Bob leaving it.
const OTHER_MEMBER: &str = "01937b7c-0000-7000-8000-0000000000c0";

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
            not_after: minted.grant.expires_at,
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

// ===========================================================================================
// The pull path: blob bytes under a capability (S-E5)
// ===========================================================================================

/// Fetch `address` as `bearer` and return the raw response.
async fn fetch(
    fixture: &Fixture,
    bearer: &str,
    address: &ContentAddress,
) -> kynos::test::TestResponse {
    fixture
        .client
        .get(&format!("/v1/blob/{address}"))
        .header("authorization", bearer)
        .send()
        .await
}

/// Publish an asset into `into` carrying an original and a derivative, and return their
/// addresses.
///
/// Landed first — a reserved row is not a reference, so its blobs resolve to `404` for
/// everybody — and the two roles recorded onto it after.
async fn two_roles(
    fixture: &Fixture,
    asset: &str,
    into: &AlbumId,
) -> (ContentAddress, ContentAddress) {
    publish_into(fixture, asset, into).await;
    let id = AssetId::new(asset);
    let original = store_blob(fixture, format!("original-{asset}").as_bytes()).await;
    record(fixture, &id, BlobRole::Original, &original).await;
    let derivative = store_blob(fixture, format!("derivative-{asset}").as_bytes()).await;
    record(fixture, &id, BlobRole::Derivative, &derivative).await;
    (original, derivative)
}

/// E2E case 4 (server half): the peer fetches the bytes the page named.
#[tokio::test]
async fn a_peer_fetches_the_albums_blobs_and_a_derivative_only_grant_is_refused_the_original() {
    let (fixture, _) = shared().await;
    let (original, derivative) = two_roles(&fixture, "shared-3", &album()).await;

    // `read` covers both roles, and the bytes are the bytes.
    let (full, _) = capability(&fixture, PEER, Scope::Read, 1).await;
    let served = fetch(&fixture, &full, &original).await;
    served.assert_status(StatusCode::OK);
    assert_eq!(served.bytes().as_ref(), b"original-shared-3");
    fetch(&fixture, &full, &derivative)
        .await
        .assert_status(StatusCode::OK);

    // `read-derivative-only` is refused the original, by the blob's server-visible role and not
    // by anything the peer said it was fetching.
    let (thumbs, _) = capability(&fixture, PEER, Scope::ReadDerivativeOnly, 1).await;
    let refused = fetch(&fixture, &thumbs, &original).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.scope_insufficient");
    fetch(&fixture, &thumbs, &derivative)
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_capability_for_another_album_gets_the_answer_an_unknown_address_gets() {
    // The disclosure boundary: a `403` would confirm the address is referenced by somebody, so
    // a peer outside the album is told exactly what a stranger naming a random hash is told —
    // byte-identical, headers and body.
    let (fixture, _) = shared().await;
    let (elsewhere, _derivative) = two_roles(&fixture, "private-2", &second_album()).await;
    let (bearer, _) = capability(&fixture, PEER, Scope::Read, 1).await;

    let refused = fetch(&fixture, &bearer, &elsewhere).await;
    refused.assert_status(StatusCode::NOT_FOUND);
    let refused: Value = refused.json();
    let unknown = fetch(
        &fixture,
        &bearer,
        &ContentAddress::parse(&support::checksum(b"nothing holds these")).expect("an address"),
    )
    .await;
    unknown.assert_status(StatusCode::NOT_FOUND);
    let unknown: Value = unknown.json();
    assert_eq!(refused, unknown);
}

#[tokio::test]
async fn a_peer_is_told_a_revoked_grant_apart_from_an_accounts_revoked_membership() {
    // Both are `403`; the codes differ because the actions differ — an account re-syncs its
    // membership, a peer asks its home server for a fresh grant.
    let (fixture, _) = shared().await;
    let (original, _) = two_roles(&fixture, "shared-4", &album()).await;
    let (bearer, jti) = capability(&fixture, PEER, Scope::Read, 1).await;
    fixture
        .revocations
        .revoke_issued(&jti, fixture.clock.now())
        .await
        .expect("revokes");

    let refused = fetch(&fixture, &bearer, &original).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.capability_revoked");

    // And a former member of the same album still gets the account's code.
    let bob = fixture.other_bearer(BOB).await;
    fetch(&fixture, &bob, &original)
        .await
        .assert_status(StatusCode::OK);
    roster(&fixture, 2, &[]).await;
    let former = fetch(&fixture, &bob, &original).await;
    former.assert_status(StatusCode::FORBIDDEN);
    let former: Value = former.json();
    assert_eq!(former["code"], "error.blob.access_revoked");
}

#[tokio::test]
async fn a_peer_is_never_told_about_an_accounts_upload_in_flight() {
    // The transient `409` reports the caller's *own* device still sending the bytes. A peer has
    // no device here, so it gets what an unreferenced address gives and waits for the feed's
    // `original_held` to flip.
    let (fixture, _) = shared().await;
    let owner_bearer = fixture.bearer().await;
    let coming = vec![b'y'; 4096];
    let promised = support::checksum(&coming);
    let address = ContentAddress::parse(&promised).expect("an address");
    fixture
        .open_session(&coming, "original", &owner_bearer)
        .await;

    fetch(&fixture, &owner_bearer, &address)
        .await
        .assert_status(StatusCode::CONFLICT);
    let (bearer, _) = capability(&fixture, PEER, Scope::Read, 1).await;
    fetch(&fixture, &bearer, &address)
        .await
        .assert_status(StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_takedown_answers_a_peer_the_same_410_and_leaves_the_bytes_alone() {
    // The authority is asked first, so the `410` is legible only to a reader entitled to the
    // bytes — and the hold is a serving constraint, never a destruction.
    let (fixture, _) = shared().await;
    let (original, _) = two_roles(&fixture, "shared-5", &album()).await;
    let (bearer, _) = capability(&fixture, PEER, Scope::Read, 1).await;
    fetch(&fixture, &bearer, &original)
        .await
        .assert_status(StatusCode::OK);

    fixture
        .index
        .set_hold(&AssetId::new("shared-5"), Some(ServingHold::Takedown))
        .await
        .expect("the index holds");
    fetch(&fixture, &bearer, &original)
        .await
        .assert_status(StatusCode::GONE);

    // A peer outside the album still sees nothing, not even the takedown.
    let (other_album, _) = capability_over(
        &fixture,
        &fixture.codec,
        PEER,
        &second_album(),
        Scope::Read,
        1,
    )
    .await;
    fetch(&fixture, &other_album, &original)
        .await
        .assert_status(StatusCode::NOT_FOUND);

    assert_eq!(
        fixture
            .blobs
            .read_at(&original, 0, 64)
            .await
            .expect("the store answers")
            .expect("the bytes are there"),
        b"original-shared-5",
        "a takedown does not touch the ciphertext"
    );
}

#[tokio::test]
async fn a_blocked_peer_and_a_spent_budget_refuse_the_blob_route_too() {
    let (fixture, _) = shared().await;
    let (original, _) = two_roles(&fixture, "shared-6", &album()).await;
    let (bearer, _) = capability(&fixture, PEER, Scope::Read, 1).await;

    fixture
        .peers
        .block(&PeerId::new(PEER), fixture.clock.now(), None)
        .await
        .expect("blocks");
    let blocked = fetch(&fixture, &bearer, &original).await;
    blocked.assert_status(StatusCode::FORBIDDEN);
    let blocked: Value = blocked.json();
    assert_eq!(blocked["code"], "error.moderation.server_blocked");
    fixture
        .peers
        .unblock(&PeerId::new(PEER))
        .await
        .expect("unblocks");

    let key = CounterKey::PeerRequests(PEER.to_owned());
    for _ in 0..budgets::PEER_REQUESTS.limit {
        fixture
            .counters
            .hit(&key, budgets::PEER_REQUESTS, fixture.clock.now())
            .await
            .expect("the counter answers");
    }
    let spent = fetch(&fixture, &bearer, &original).await;
    spent.assert_status(StatusCode::TOO_MANY_REQUESTS);
    let spent: Value = spent.json();
    assert_eq!(spent["code"], "error.federation.rate_budget_exceeded");
}

// ===========================================================================================
// The lifecycle: minting, revoking and refreshing the grant (S-E2)
// ===========================================================================================

/// A fixture whose seeded account is anchored on `dsk` and whose album is provisioned, so the
/// roster route — and therefore the revocation write behind it — can run for real.
async fn anchored(dsk: &HybridSigningKey) -> (Fixture, String) {
    let fixture = Fixture::working();
    let bearer = fixture.bearer().await;
    fixture
        .client
        .post("/v1/auth/devices/directory")
        .header("authorization", &bearer)
        .header("x-capsule-identity-key", &support::identity_header(dsk))
        .body(
            "application/cbor",
            support::signed_directory_with_device(
                dsk,
                1,
                support::device(),
                dsk,
                "1970-01-01T00:00:00Z",
            ),
        )
        .send()
        .await
        .assert_status(StatusCode::OK);
    provision(&fixture, &bearer).await;
    (fixture, bearer)
}

/// PUT the seeded album's roster through the route, as the owner's client does.
async fn publish_roster(
    fixture: &Fixture,
    bearer: &str,
    dsk: &HybridSigningKey,
    version: u64,
    members: &[(&str, MemberRole)],
) -> kynos::test::TestResponse {
    fixture
        .client
        .put(&format!("/v1/albums/{}/roster", album()))
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(&serde_json::json!({
            "roster_cbor": support::signed_roster(
                dsk,
                support::device(),
                &album(),
                version,
                u32::try_from(version).expect("a small epoch"),
                members,
            ),
        }))
        .send()
        .await
}

/// Mint a capability through the route, as the owner's client does.
async fn mint(fixture: &Fixture, bearer: &str, body: Value) -> kynos::test::TestResponse {
    fixture
        .client
        .post(&format!("/v1/albums/{}/capabilities", album()))
        .header("authorization", bearer)
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await
}

/// The mint body for `PEER` over Bob's membership. Not renewable, which is the default.
fn mint_body(scope: &str) -> Value {
    serde_json::json!({ "peer": PEER, "member": BOB, "scope": scope })
}

/// The same, renewable until `hours` from the fixture's now.
fn renewable_body(fixture: &Fixture, scope: &str, hours: i64) -> Value {
    serde_json::json!({
        "peer": PEER,
        "member": BOB,
        "scope": scope,
        "renewable_until": crate::support::deadline(fixture, hours).to_string(),
    })
}

/// DELETE one capability of `on`, as `bearer`.
async fn revoke(
    fixture: &Fixture,
    bearer: &str,
    on: &AlbumId,
    jti: &str,
) -> kynos::test::TestResponse {
    fixture
        .client
        .delete(&format!("/v1/albums/{on}/capabilities/{jti}"))
        .header("authorization", bearer)
        .send()
        .await
}

/// POST the refresh, presenting `credential`.
async fn refresh(fixture: &Fixture, credential: &str) -> kynos::test::TestResponse {
    fixture
        .client
        .post("/v1/federation/capabilities/refresh")
        .header("authorization", credential)
        .header("accept", "application/json")
        .send()
        .await
}

/// The `jti`s the published revocation list carries.
async fn published(fixture: &Fixture) -> Vec<String> {
    let list: Value = fixture
        .client
        .get("/.well-known/capsule/revoked-jti")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    list["revoked"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|token| token["jti"].as_str().expect("a jti").to_owned())
        .collect()
}

/// E2E case 4 (server half), whole: the owner mints, the peer pulls, the roster cuts the grant.
#[tokio::test]
async fn e2e_case_4_the_owner_mints_the_peer_pulls_and_a_roster_change_cuts_the_grant() {
    let dsk = support::identity_key();
    let (fixture, bearer) = anchored(&dsk).await;
    publish_roster(&fixture, &bearer, &dsk, 1, &[(BOB, MemberRole::Reader)])
        .await
        .assert_status(StatusCode::OK);
    let seq = publish_into(&fixture, "case-4", &album()).await;

    // The owner's client mints, over the album's own protocol pin.
    let minted: Value = mint(&fixture, &bearer, mint_body("read"))
        .await
        .assert_status(StatusCode::CREATED)
        .json();
    assert_eq!(minted["album_id"], album().as_str());
    assert_eq!(minted["peer"], PEER);
    assert_eq!(minted["member"], BOB);
    assert_eq!(minted["scope"], "read");
    assert_eq!(minted["min_protocol_version"], PROTOCOL_VERSION);
    let jti = minted["jti"].as_str().expect("a jti").to_owned();
    let capability = format!("Bearer {}", minted["token"].as_str().expect("a token"));

    // The peer pulls the page with it.
    let body: Value = page(&fixture, &capability, &album_query())
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(
        body["entries"]
            .as_array()
            .expect("an array")
            .iter()
            .map(|entry| entry["sync_seq"].as_u64().expect("a position"))
            .collect::<Vec<_>>(),
        vec![seq]
    );
    assert!(!published(&fixture).await.contains(&jti));

    // The owner publishes a roster that omits Bob. The grant is cut and published, without the
    // owner having named it.
    publish_roster(&fixture, &bearer, &dsk, 2, &[])
        .await
        .assert_status(StatusCode::OK);
    assert!(
        published(&fixture).await.contains(&jti),
        "the roster change published the grant's jti"
    );
    let refused = page(&fixture, &capability, &album_query()).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.capability_revoked");
}

#[tokio::test]
async fn a_roster_change_that_keeps_the_member_cuts_nothing() {
    // An epoch bump is not a removal: the member still holds their keys and the server has
    // nothing to cut. Only the *member* leaving revokes.
    let dsk = support::identity_key();
    let (fixture, bearer) = anchored(&dsk).await;
    publish_roster(&fixture, &bearer, &dsk, 1, &[(BOB, MemberRole::Reader)])
        .await
        .assert_status(StatusCode::OK);
    let minted: Value = mint(&fixture, &bearer, mint_body("read"))
        .await
        .assert_status(StatusCode::CREATED)
        .json();
    let jti = minted["jti"].as_str().expect("a jti").to_owned();
    let capability = format!("Bearer {}", minted["token"].as_str().expect("a token"));

    publish_roster(
        &fixture,
        &bearer,
        &dsk,
        2,
        &[
            (BOB, MemberRole::Writer),
            (OTHER_MEMBER, MemberRole::Reader),
        ],
    )
    .await
    .assert_status(StatusCode::OK);
    assert!(!published(&fixture).await.contains(&jti));
    page(&fixture, &capability, &album_query())
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_mint_is_refused_for_another_account_a_blocked_peer_and_a_member_off_the_roster() {
    let dsk = support::identity_key();
    let (fixture, bearer) = anchored(&dsk).await;
    publish_roster(&fixture, &bearer, &dsk, 1, &[(BOB, MemberRole::Reader)])
        .await
        .assert_status(StatusCode::OK);

    // Not the caller's album is not found — the album ceremonies' answer, so a member holding
    // somebody else's album id learns nothing.
    let bob = fixture.other_bearer(BOB).await;
    let refused = mint(&fixture, &bob, mint_body("read")).await;
    refused.assert_status(StatusCode::NOT_FOUND);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.album_not_found");

    // A member the roster does not carry.
    let refused = mint(
        &fixture,
        &bearer,
        serde_json::json!({ "peer": PEER, "member": OTHER_MEMBER, "scope": "read" }),
    )
    .await;
    refused.assert_status(StatusCode::CONFLICT);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.member_not_on_roster");

    // A blocked peer gets no new grant, and gets one again when the block lifts.
    fixture
        .peers
        .block(&PeerId::new(PEER), fixture.clock.now(), None)
        .await
        .expect("blocks");
    let refused = mint(&fixture, &bearer, mint_body("read")).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.moderation.server_blocked");
    fixture
        .peers
        .unblock(&PeerId::new(PEER))
        .await
        .expect("unblocks");
    mint(&fixture, &bearer, mint_body("read"))
        .await
        .assert_status(StatusCode::CREATED);

    // And an empty peer origin is a client bug, not an unknown server.
    let refused = mint(
        &fixture,
        &bearer,
        serde_json::json!({ "peer": "   ", "member": BOB, "scope": "read" }),
    )
    .await;
    refused.assert_status(StatusCode::BAD_REQUEST);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.capability_malformed");
}

#[tokio::test]
async fn an_owner_revokes_one_grant_idempotently_and_cannot_reach_another_albums() {
    let dsk = support::identity_key();
    let (fixture, bearer) = anchored(&dsk).await;
    publish_roster(&fixture, &bearer, &dsk, 1, &[(BOB, MemberRole::Reader)])
        .await
        .assert_status(StatusCode::OK);
    let minted: Value = mint(&fixture, &bearer, mint_body("read"))
        .await
        .assert_status(StatusCode::CREATED)
        .json();
    let jti = minted["jti"].as_str().expect("a jti").to_owned();
    let capability = format!("Bearer {}", minted["token"].as_str().expect("a token"));

    // Another album cannot revoke this album's grant, even though both are the owner's: the
    // record's own album is checked, so a `jti` is not a handle on somebody else's grant. The
    // second album is not provisioned, so the answer is the same not-found either way.
    revoke(&fixture, &bearer, &second_album(), &jti)
        .await
        .assert_status(StatusCode::NOT_FOUND);
    assert!(!published(&fixture).await.contains(&jti));

    revoke(&fixture, &bearer, &album(), &jti)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    assert!(published(&fixture).await.contains(&jti));
    // Idempotent, and silent about a jti it never held: not a probe over identifiers.
    revoke(&fixture, &bearer, &album(), &jti)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    revoke(
        &fixture,
        &bearer,
        &album(),
        "01937b7c-0000-7000-8000-0000000000ff",
    )
    .await
    .assert_status(StatusCode::NO_CONTENT);

    let refused = page(&fixture, &capability, &album_query()).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.capability_revoked");
}

#[tokio::test]
async fn a_refresh_issues_a_successor_cuts_the_predecessor_and_replays_to_the_same_token() {
    let dsk = support::identity_key();
    let (fixture, bearer) = anchored(&dsk).await;
    publish_roster(&fixture, &bearer, &dsk, 1, &[(BOB, MemberRole::Reader)])
        .await
        .assert_status(StatusCode::OK);
    publish_into(&fixture, "refresh-1", &album()).await;
    let minted: Value = mint(
        &fixture,
        &bearer,
        renewable_body(&fixture, "read-derivative-only", 24 * 7),
    )
    .await
    .assert_status(StatusCode::CREATED)
    .json();
    assert_eq!(minted["renewable"], true);
    assert_ne!(minted["not_after"], minted["expires_at"]);
    let not_after = minted["not_after"].as_str().expect("a deadline").to_owned();
    let old_jti = minted["jti"].as_str().expect("a jti").to_owned();
    let old = format!("Bearer {}", minted["token"].as_str().expect("a token"));

    // An account has nothing to refresh here.
    let refused = refresh(&fixture, &bearer).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.capability_invalid");

    let first: Value = refresh(&fixture, &old)
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(first["replayed"], false);
    assert_eq!(
        first["not_after"], not_after,
        "a refresh carries the grant's deadline unchanged"
    );
    let successor = format!("Bearer {}", first["token"].as_str().expect("a token"));
    assert_ne!(first["jti"], old_jti.as_str());

    // The predecessor is cut and published; the successor pulls, and carries the same scope.
    assert!(published(&fixture).await.contains(&old_jti));
    page(&fixture, &old, &album_query())
        .await
        .assert_status(StatusCode::FORBIDDEN);
    page(&fixture, &successor, &album_query())
        .await
        .assert_status(StatusCode::OK);

    // A replay of the same predecessor answers the same successor, byte for byte: the grant is
    // re-signed from its record, and every instant is at whole seconds.
    let replay: Value = refresh(&fixture, &old)
        .await
        .assert_status(StatusCode::OK)
        .json();
    assert_eq!(replay["replayed"], true);
    assert_eq!(replay["token"], first["token"]);
    assert_eq!(replay["jti"], first["jti"]);

    // And a replay whose successor has since been revoked is refused rather than re-issued.
    fixture
        .revocations
        .revoke_issued(first["jti"].as_str().expect("a jti"), fixture.clock.now())
        .await
        .expect("revokes");
    let refused = refresh(&fixture, &old).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.capability_revoked");
}

#[tokio::test]
async fn a_deployment_that_does_not_federate_mints_nothing_but_still_revokes() {
    // Turning federation off must never be the thing that takes away an operator's ability to
    // cut a grant that is already out there.
    let fixture = Fixture::without_federation();
    let bearer = fixture.bearer().await;
    provision(&fixture, &bearer).await;
    roster(&fixture, 1, &[(BOB, MemberRole::Reader)]).await;

    let refused = mint(&fixture, &bearer, mint_body("read")).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.not_configured");

    // A grant minted while it federated still verifies — a token is not un-minted by a
    // configuration change — and can still be revoked and refused.
    let (capability, jti) = capability(&fixture, PEER, Scope::Read, 1).await;
    publish_into(&fixture, "unfederated-1", &album()).await;
    page(&fixture, &capability, &album_query())
        .await
        .assert_status(StatusCode::OK);
    // But it cannot be continued.
    refresh(&fixture, &capability)
        .await
        .assert_status(StatusCode::FORBIDDEN);
    revoke(&fixture, &bearer, &album(), &jti)
        .await
        .assert_status(StatusCode::NO_CONTENT);
    let refused = page(&fixture, &capability, &album_query()).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.capability_revoked");
}

// ===========================================================================================
// Moderation's federated halves (S-C49)
// ===========================================================================================

/// Pin `key` as `PEER`'s operational key, as an operator does.
async fn pin(fixture: &Fixture, key: [u8; 32]) {
    fixture
        .peers
        .pin(&PeerId::new(PEER), key, fixture.clock.now())
        .await
        .expect("the operator pins");
}

/// POST a federated report body.
async fn file(fixture: &Fixture, body: Value) -> kynos::test::TestResponse {
    fixture
        .client
        .post("/v1/federation/reports")
        .header("accept", "application/json")
        .json(&body)
        .send()
        .await
}

/// A report from `PEER` about Bob's copy of `hash`, signed by `pair`.
fn report(pair: &ring::signature::Ed25519KeyPair, hash: &str, reason: Option<&str>) -> Value {
    support::signed_report(
        pair,
        PEER,
        BOB,
        hash,
        &album(),
        reason,
        "2026-09-02T00:00:00Z",
    )
}

#[tokio::test]
async fn a_signed_report_from_a_pinned_peer_is_filed_and_changes_nothing_about_the_account() {
    let (fixture, _) = shared().await;
    let (signer, public) = support::peer_keypair();
    pin(&fixture, public).await;
    let hash = support::checksum(b"the reported bytes");

    let accepted: Value = file(&fixture, report(&signer, &hash, Some("csam")))
        .await
        .assert_status(StatusCode::ACCEPTED)
        .json();
    let report_id = accepted["report_id"].as_str().expect("a report id");

    // Asserted against the store the server wrote, never against a second read of the body.
    let pending = fixture
        .moderation
        .pending_reports()
        .await
        .expect("the store answers");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].report_id, report_id);
    assert_eq!(pending[0].reporting_server, PEER);
    assert_eq!(pending[0].reported_user, UserId::new(BOB));
    assert_eq!(pending[0].asset_hash, hash);
    assert_eq!(pending[0].album_id, album());
    assert_eq!(pending[0].reason.as_deref(), Some("csam"));
    assert!(
        !pending[0].signature.is_empty(),
        "the signature is kept so an operator can re-verify it"
    );

    // A report is an input to a decision, never a decision: nothing was done to the account.
    assert_eq!(
        fixture
            .moderation
            .standing(&UserId::new(BOB))
            .await
            .expect("the store answers"),
        capsule_server::moderation::Standing::Active
    );
    assert!(
        fixture
            .moderation
            .events_for_user(&UserId::new(BOB))
            .await
            .expect("the store answers")
            .is_empty(),
        "nothing was done to the account, so nothing is on its record"
    );
}

#[tokio::test]
async fn a_filed_reports_signature_re_verifies_against_the_row_that_was_stored() {
    // H2. The stored fields are *normalized* — `reporting_server` is the canonical PeerId form
    // and `reported_at` is a parsed instant — so rebuilding a claim from them produces different
    // bytes and a signature that no longer verifies. What makes the row re-verifiable is that
    // the exact signed bytes are kept beside it. This case is the round trip an operator does.
    let (fixture, _) = shared().await;
    let (signer, public) = support::peer_keypair();
    pin(&fixture, public).await;
    let hash = support::checksum(b"the reported bytes");

    // Sent the way a real peer might: a trailing dot and mixed case on the origin, and padding
    // the intake trims. Every one of them survives into the signed bytes and none into the row.
    let body = support::signed_report(
        &signer,
        "Other.Test.",
        BOB,
        &hash,
        &album(),
        Some("csam"),
        "2026-09-02T00:00:00Z",
    );
    file(&fixture, body.clone())
        .await
        .assert_status(StatusCode::ACCEPTED);

    let filed = fixture
        .moderation
        .pending_reports()
        .await
        .expect("the store answers");
    let filed = filed.first().expect("one report");
    assert_eq!(
        filed.reporting_server, PEER,
        "the row carries the canonical peer id, not what the peer wrote"
    );
    assert_eq!(
        filed.reported_at,
        "2026-09-02T00:00:00Z".parse::<Timestamp>().unwrap()
    );

    // The row re-verifies, months later, with nothing but its own two byte strings and the key.
    assert_eq!(
        capsule_server::federation::verify_signed_report(&filed.signed, &filed.signature, &public),
        Ok(()),
        "a filed report must re-verify from what was stored"
    );
    // And a different peer's key does not, so this is a real check and not a tautology.
    let (_, other) = support::peer_keypair();
    assert!(
        capsule_server::federation::verify_signed_report(&filed.signed, &filed.signature, &other)
            .is_err()
    );

    // The bytes are the peer's own, not a re-encoding of the row: rebuilding a claim from the
    // normalized fields would produce something else entirely.
    let rebuilt = capsule_server::federation::ReportClaim {
        reporting_server: filed.reporting_server.clone(),
        reported_user: filed.reported_user.as_str().to_owned(),
        asset_hash: filed.asset_hash.clone(),
        album_id: filed.album_id.as_str().to_owned(),
        reason: filed.reason.clone(),
        reported_at: filed.reported_at.to_string(),
    };
    assert_ne!(
        rebuilt.signing_bytes().expect("it encodes"),
        filed.signed,
        "if these were equal the stored bytes would be redundant and this case pointless"
    );
}

#[tokio::test]
async fn a_report_is_refused_unsigned_from_an_unknown_peer_and_from_a_blocked_one() {
    let (fixture, _) = shared().await;
    let (signer, public) = support::peer_keypair();
    let hash = support::checksum(b"the reported bytes");

    // Nobody has pinned this peer, so there is nothing to verify against.
    let refused = file(&fixture, report(&signer, &hash, None)).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.peer_unknown");

    pin(&fixture, public).await;

    // Another key's signature over the same claim.
    let (impostor, _) = support::peer_keypair();
    let refused = file(&fixture, report(&impostor, &hash, None)).await;
    refused.assert_status(StatusCode::UNAUTHORIZED);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.moderation.report_unsigned");

    // The peer's own signature over a *different* claim, replayed onto this one.
    let mut tampered = report(&signer, &hash, Some("csam"));
    tampered["asset_hash"] = Value::from(support::checksum(b"other bytes"));
    let refused = file(&fixture, tampered).await;
    refused.assert_status(StatusCode::UNAUTHORIZED);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.moderation.report_unsigned");

    // Blocked: refused before the signature is even looked at.
    fixture
        .peers
        .block(
            &PeerId::new(PEER),
            fixture.clock.now(),
            Some("noise".into()),
        )
        .await
        .expect("blocks");
    let refused = file(&fixture, report(&signer, &hash, None)).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.moderation.server_blocked");

    assert!(
        fixture
            .moderation
            .pending_reports()
            .await
            .expect("the store answers")
            .is_empty(),
        "nothing a refusal saw reached the queue"
    );
}

#[tokio::test]
async fn a_peers_reports_about_one_account_are_bounded_and_another_account_is_its_own_budget() {
    // Invariant 24: the budget is per `(reporting_server, reported_user)`, so a flood against
    // one user cannot silence a peer that has something to say about another.
    let (fixture, _) = shared().await;
    let (signer, public) = support::peer_keypair();
    pin(&fixture, public).await;
    let hash = support::checksum(b"the reported bytes");

    for _ in 1..budgets::FEDERATED_REPORTS.limit {
        fixture
            .counters
            .hit(
                &CounterKey::FederatedReports(format!("{PEER}:{BOB}")),
                budgets::FEDERATED_REPORTS,
                fixture.clock.now(),
            )
            .await
            .expect("the counter answers");
    }
    file(&fixture, report(&signer, &hash, None))
        .await
        .assert_status(StatusCode::ACCEPTED);
    let refused = file(&fixture, report(&signer, &hash, None)).await;
    refused.assert_status(StatusCode::TOO_MANY_REQUESTS);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.moderation.report_rate_limited");

    // Another account on this server is a different boundary.
    file(
        &fixture,
        support::signed_report(
            &signer,
            PEER,
            OTHER_MEMBER,
            &hash,
            &album(),
            None,
            "2026-09-02T00:00:00Z",
        ),
    )
    .await
    .assert_status(StatusCode::ACCEPTED);

    fixture.clock.advance(budgets::FEDERATED_REPORTS.window);
    file(&fixture, report(&signer, &hash, None))
        .await
        .assert_status(StatusCode::ACCEPTED);
}

#[tokio::test]
async fn blocking_a_peer_cuts_and_publishes_every_grant_it_holds() {
    // The blocklist already refuses at every boundary; the cascade is what puts the peer's jtis
    // on the record every peer polls, so a block is legible rather than only enforced.
    let (fixture, _) = shared().await;
    let (first, first_jti) = capability(&fixture, PEER, Scope::Read, 1).await;
    let (_, second_jti) = capability(&fixture, PEER, Scope::ReadDerivativeOnly, 1).await;
    let (other, other_jti) = capability(&fixture, "third.test", Scope::Read, 1).await;

    fixture
        .peers
        .block(&PeerId::new(PEER), fixture.clock.now(), None)
        .await
        .expect("blocks");
    // Over the *same* stores the server holds, so what the cascade writes is what the
    // published list and the next presentation read.
    let federation = FederationContext::new(FederationCollaborators {
        codec: fixture.codec.clone(),
        capabilities: fixture.revocations.clone(),
        peers: fixture.peers.clone(),
        clock: fixture.clock.clone(),
        federation_url: Some(support::FEDERATION_URL.to_owned()),
    });
    let cut = capsule_server::federation::on_peer_blocked(&federation, &PeerId::new(PEER))
        .await
        .expect("the cascade runs");
    assert_eq!(cut, 2);

    let list: Value = fixture
        .client
        .get("/.well-known/capsule/revoked-jti")
        .header("accept", "application/json")
        .send()
        .await
        .assert_status(StatusCode::OK)
        .json();
    let published: Vec<&str> = list["revoked"]
        .as_array()
        .expect("an array")
        .iter()
        .map(|token| token["jti"].as_str().expect("a jti"))
        .collect();
    assert!(published.contains(&first_jti.as_str()));
    assert!(published.contains(&second_jti.as_str()));
    assert!(
        !published.contains(&other_jti.as_str()),
        "another peer's grant is not this peer's block"
    );

    // Unblocking does not restore what the cascade cut.
    fixture
        .peers
        .unblock(&PeerId::new(PEER))
        .await
        .expect("unblocks");
    let refused = page(&fixture, &first, &album_query()).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.capability_revoked");
    page(&fixture, &other, &album_query())
        .await
        .assert_status(StatusCode::OK);
}

// ===========================================================================================
// The grant's absolute deadline (H1) and the refresh's own roster check
// ===========================================================================================

#[tokio::test]
async fn a_grant_the_owner_did_not_make_renewable_cannot_be_refreshed_at_all() {
    // The default, and the whole of H1's fix: without an absolute deadline a peer holding a
    // deliberately short grant refreshes to the default TTL inside its own lifetime and chains
    // forever, leaving `ttl_seconds` advisory for exactly one hop.
    let dsk = support::identity_key();
    let (fixture, bearer) = anchored(&dsk).await;
    publish_roster(&fixture, &bearer, &dsk, 1, &[(BOB, MemberRole::Reader)])
        .await
        .assert_status(StatusCode::OK);

    let minted: Value = mint(
        &fixture,
        &bearer,
        serde_json::json!({ "peer": PEER, "member": BOB, "scope": "read", "ttl_seconds": 60 }),
    )
    .await
    .assert_status(StatusCode::CREATED)
    .json();
    assert_eq!(minted["renewable"], false);
    assert_eq!(
        minted["not_after"], minted["expires_at"],
        "a grant nobody made renewable dies with its first token"
    );
    let capability = format!("Bearer {}", minted["token"].as_str().expect("a token"));

    let refused = refresh(&fixture, &capability).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.capability_expired");
}

#[tokio::test]
async fn a_renewable_grant_stops_at_its_deadline_and_its_last_token_does_not_overhang_it() {
    let dsk = support::identity_key();
    let (fixture, bearer) = anchored(&dsk).await;
    publish_roster(&fixture, &bearer, &dsk, 1, &[(BOB, MemberRole::Reader)])
        .await
        .assert_status(StatusCode::OK);

    // Renewable for ten hours; the default token life is six.
    let minted: Value = mint(&fixture, &bearer, renewable_body(&fixture, "read", 10))
        .await
        .assert_status(StatusCode::CREATED)
        .json();
    let deadline: Timestamp = minted["not_after"]
        .as_str()
        .expect("a deadline")
        .parse()
        .expect("an instant");
    let mut capability = format!("Bearer {}", minted["token"].as_str().expect("a token"));

    // Five hours in, the grant has five left and the default token life is six: the successor
    // is minted for the five that remain, so it ends *at* the deadline rather than past it.
    fixture.clock.advance(SignedDuration::from_hours(5));
    let renewed: Value = refresh(&fixture, &capability)
        .await
        .assert_status(StatusCode::OK)
        .json();
    let expires: Timestamp = renewed["expires_at"]
        .as_str()
        .expect("an expiry")
        .parse()
        .expect("an instant");
    assert_eq!(
        expires, deadline,
        "the last token of a grant is minted for exactly what is left"
    );
    assert_eq!(
        renewed["not_after"], minted["not_after"],
        "and the deadline itself never moves"
    );
    capability = format!("Bearer {}", renewed["token"].as_str().expect("a token"));

    // That successor is the last one. Refused while it is still a perfectly valid token, so the
    // answer is "this grant is over" and not "your token expired" — the peer's next move is the
    // album's owner, not this server.
    let refused = refresh(&fixture, &capability).await;
    refused.assert_status(StatusCode::FORBIDDEN);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.capability_expired");

    // And the token itself still works right up to the deadline.
    page(&fixture, &capability, &album_query())
        .await
        .assert_status(StatusCode::OK);
}

#[tokio::test]
async fn a_mint_refuses_a_deadline_that_is_past_or_absurd() {
    let dsk = support::identity_key();
    let (fixture, bearer) = anchored(&dsk).await;
    publish_roster(&fixture, &bearer, &dsk, 1, &[(BOB, MemberRole::Reader)])
        .await
        .assert_status(StatusCode::OK);

    for (name, until) in [
        ("in the past", support::deadline(&fixture, -1).to_string()),
        (
            "a century out",
            support::deadline(&fixture, 24 * 365 * 100).to_string(),
        ),
        ("not a date", "next tuesday".to_owned()),
    ] {
        let refused = mint(
            &fixture,
            &bearer,
            serde_json::json!({
                "peer": PEER, "member": BOB, "scope": "read", "renewable_until": until,
            }),
        )
        .await;
        refused.assert_status(StatusCode::BAD_REQUEST);
        let refused: Value = refused.json();
        assert_eq!(
            refused["code"], "error.federation.capability_malformed",
            "{name}"
        );
    }
}

#[tokio::test]
async fn a_refresh_stops_once_the_member_leaves_the_roster() {
    // The read path already refuses such a token, so nothing is exposed — but a server that
    // kept issuing successors for a membership that had ended would be minting tokens that can
    // never be used and writing a store row for each.
    let dsk = support::identity_key();
    let (fixture, bearer) = anchored(&dsk).await;
    publish_roster(&fixture, &bearer, &dsk, 1, &[(BOB, MemberRole::Reader)])
        .await
        .assert_status(StatusCode::OK);
    let minted: Value = mint(&fixture, &bearer, renewable_body(&fixture, "read", 24 * 7))
        .await
        .assert_status(StatusCode::CREATED)
        .json();
    let capability = format!("Bearer {}", minted["token"].as_str().expect("a token"));
    refresh(&fixture, &capability)
        .await
        .assert_status(StatusCode::OK);

    publish_roster(&fixture, &bearer, &dsk, 2, &[])
        .await
        .assert_status(StatusCode::OK);
    let refused = refresh(&fixture, &capability).await;
    refused.assert_status(StatusCode::CONFLICT);
    let refused: Value = refused.json();
    assert_eq!(refused["code"], "error.federation.member_not_on_roster");
}
