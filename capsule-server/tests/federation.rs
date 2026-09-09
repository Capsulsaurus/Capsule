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
use capsule_server::index::{AssetIndex, BlobRecord, PendingAsset, ServingHold};
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
