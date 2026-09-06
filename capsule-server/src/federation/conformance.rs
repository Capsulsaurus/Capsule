//! The one suite every [`CapabilityStore`] and [`PeerStore`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **The store is the revocation list.** Revoking an issued capability publishes its `jti`;
//!   revoking a `jti` nothing backs still publishes it; and what is published is pruned past
//!   the token's own expiry and bounded by the TTL ceiling — the rules the standalone list
//!   carried before this store replaced it.
//! - **Refresh is one operation and is idempotent.** The successor is recorded, the
//!   predecessor is linked and revoked, and a replay answers with the same successor.
//! - **A refusal changes nothing.** `AlreadyRevoked`, `AlreadyRefreshed`, `AlreadyBlocked`
//!   leave every row as it was.
//!
//! # Reusing a harness
//!
//! Every case scopes its own identifiers, so cases may share one store and [`run_all`] does.
//! The clock is the harness's own [`ManualClock`], because pruning is decided on the adapter's
//! clock rather than on an argument.

use jiff::SignedDuration;

use super::PeerId;
use super::capability::Scope;
use super::peers::{BlockOutcome, PeerStore, UnblockOutcome};
use super::store::{
    CapabilityFilter, CapabilityRecord, CapabilityStore, RefreshOutcome, RevokeOutcome,
};
use crate::discovery::revocation::{RevokeError, RevokedToken};
use crate::store::memory::ManualClock;
use crate::store::{AlbumId, Clock as _, StoreError, UserId};

/// The stores under test.
pub trait Harness: Send + Sync {
    /// The capability store under test.
    fn capabilities(&self) -> &dyn CapabilityStore;
    /// The peer store under test.
    fn peers(&self) -> &dyn PeerStore;
    /// The clock both adapters read.
    fn clock(&self) -> &ManualClock;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming federation store must succeed at {doing}: {error}"),
    }
}

/// A capability for `case`, minted at the clock's now and good for `hours`.
fn record(h: &dyn Harness, case: &str, jti: &str, hours: i64) -> CapabilityRecord {
    let now = h.clock().now();
    CapabilityRecord {
        jti: format!("{case}-{jti}"),
        album_id: AlbumId::new(format!("{case}-album")),
        peer_id: PeerId::new(format!("{case}.peer.test")),
        member: UserId::new(format!("{case}-member")),
        scope: Scope::Read,
        granted_epoch: 3,
        min_protocol_version: "2026-06-01".to_owned(),
        issued_at: now,
        expires_at: crate::store::deadline(now, SignedDuration::from_hours(hours)),
        revoked_at: None,
        refreshed_to: None,
    }
}

async fn issue(h: &dyn Harness, record: CapabilityRecord) {
    ok(h.capabilities().issue(record).await, "record a capability");
}

async fn find(h: &dyn Harness, jti: &str) -> Option<CapabilityRecord> {
    ok(h.capabilities().find(jti).await, "find a capability")
}

async fn published(h: &dyn Harness) -> Vec<String> {
    ok(h.capabilities().published().await, "read the list")
        .revoked
        .into_iter()
        .map(|token| token.jti)
        .collect()
}

// ===========================================================================================
// Capabilities
// ===========================================================================================

/// An issued capability reads back whole, and an unknown `jti` is `None`.
pub async fn an_issued_capability_reads_back_and_an_unknown_jti_is_none(h: &dyn Harness) {
    let case = "readback";
    let record = record(h, case, "one", 6);
    issue(h, record.clone()).await;
    assert_eq!(find(h, &record.jti).await, Some(record.clone()));
    assert!(record.is_live(h.clock().now()));
    assert_eq!(find(h, "readback-never").await, None);
}

/// A second record under one `jti` is refused as a rejection, and the first stands.
pub async fn a_duplicate_jti_is_rejected_and_the_first_record_stands(h: &dyn Harness) {
    let case = "duplicate";
    let first = record(h, case, "one", 6);
    issue(h, first.clone()).await;
    let error = h
        .capabilities()
        .issue(CapabilityRecord {
            scope: Scope::ReadDerivativeOnly,
            ..first.clone()
        })
        .await
        .expect_err("a jti is minted once");
    assert!(matches!(error, StoreError::Rejected { .. }), "{error:?}");
    assert_eq!(find(h, &first.jti).await, Some(first));
}

/// `live` answers by album and by peer, and leaves out the revoked and the expired.
pub async fn live_filters_by_album_and_peer_and_excludes_the_revoked_and_expired(h: &dyn Harness) {
    let case = "live";
    let now = h.clock().now();
    let a = record(h, case, "a", 6);
    let b = CapabilityRecord {
        peer_id: PeerId::new("other-live.peer.test"),
        ..record(h, case, "b", 6)
    };
    let revoked = record(h, case, "revoked", 6);
    let expiring = record(h, case, "expiring", 1);
    for record in [&a, &b, &revoked, &expiring] {
        issue(h, record.clone()).await;
    }
    assert_eq!(
        h.capabilities()
            .revoke_issued(&revoked.jti, now)
            .await
            .expect("revokes"),
        RevokeOutcome::Revoked
    );

    let later = crate::store::deadline(now, SignedDuration::from_hours(2));
    let mut by_album: Vec<String> = ok(
        h.capabilities()
            .live(&CapabilityFilter::Album(a.album_id.clone()), later)
            .await,
        "list by album",
    )
    .into_iter()
    .map(|record| record.jti)
    .collect();
    by_album.sort();
    assert_eq!(by_album, vec![a.jti.clone(), b.jti.clone()]);

    let by_peer: Vec<String> = ok(
        h.capabilities()
            .live(&CapabilityFilter::Peer(b.peer_id.clone()), later)
            .await,
        "list by peer",
    )
    .into_iter()
    .map(|record| record.jti)
    .collect();
    assert_eq!(by_peer, vec![b.jti]);
}

/// Revoking an issued capability sets `revoked_at`, publishes its `jti`, and is idempotent.
pub async fn revoking_an_issued_capability_publishes_it_once(h: &dyn Harness) {
    let case = "revoke";
    let now = h.clock().now();
    let record = record(h, case, "one", 6);
    issue(h, record.clone()).await;

    assert_eq!(
        h.capabilities()
            .revoke_issued(&record.jti, now)
            .await
            .expect("revokes"),
        RevokeOutcome::Revoked
    );
    let stored = find(h, &record.jti).await.expect("still recorded");
    assert_eq!(stored.revoked_at, Some(now));
    assert!(!stored.is_live(now));
    assert!(published(h).await.contains(&record.jti));

    assert_eq!(
        h.capabilities()
            .revoke_issued(&record.jti, now)
            .await
            .expect("answers"),
        RevokeOutcome::AlreadyRevoked
    );
    assert_eq!(
        find(h, &record.jti)
            .await
            .expect("still recorded")
            .revoked_at,
        Some(now),
        "a retry does not move the instant"
    );
    assert_eq!(
        h.capabilities()
            .revoke_issued("revoke-never", now)
            .await
            .expect("answers"),
        RevokeOutcome::Unknown
    );
    assert_eq!(
        published(h)
            .await
            .iter()
            .filter(|jti| *jti == &record.jti)
            .count(),
        1,
        "one entry however many times it is revoked"
    );
}

/// A `jti` nothing backs is still published, and one past the ceiling is refused.
pub async fn a_foreign_jti_is_published_and_one_beyond_the_ceiling_is_refused(h: &dyn Harness) {
    let now = h.clock().now();
    h.capabilities()
        .revoke(RevokedToken {
            jti: "foreign-one".to_owned(),
            expires_at: crate::store::deadline(now, SignedDuration::from_hours(2)),
        })
        .await
        .expect("a foreign jti is a fact the list carries");
    assert!(published(h).await.contains(&"foreign-one".to_owned()));
    assert_eq!(
        find(h, "foreign-one").await,
        None,
        "no record is invented for it"
    );

    let error = h
        .capabilities()
        .revoke(RevokedToken {
            jti: "foreign-beyond".to_owned(),
            expires_at: crate::store::deadline(now, SignedDuration::from_hours(25)),
        })
        .await
        .expect_err("an entry past the ceiling is refused");
    assert!(matches!(error, RevokeError::Refused(_)), "{error:?}");
    assert!(!published(h).await.contains(&"foreign-beyond".to_owned()));
}

/// Revoking a `jti` through the list also revokes the record behind it, and once only.
pub async fn the_list_and_the_record_are_one_fact(h: &dyn Harness) {
    let case = "onefact";
    let now = h.clock().now();
    let record = record(h, case, "one", 6);
    issue(h, record.clone()).await;
    let entry = RevokedToken {
        jti: record.jti.clone(),
        expires_at: record.expires_at,
    };
    h.capabilities()
        .revoke(entry.clone())
        .await
        .expect("first revocation");
    h.capabilities()
        .revoke(entry)
        .await
        .expect("a retry is not a new fact");
    assert_eq!(
        find(h, &record.jti).await.expect("recorded").revoked_at,
        Some(now)
    );
    assert_eq!(
        published(h)
            .await
            .iter()
            .filter(|jti| *jti == &record.jti)
            .count(),
        1
    );
}

/// A list-side revocation of an issued `jti` is published under the record's own expiry.
///
/// A shorter expiry from the caller would prune the entry while the token still verifies —
/// a peer's cached list would drop it and honour a revoked token until its real `exp`.
pub async fn a_list_side_revocation_keeps_the_records_expiry(h: &dyn Harness) {
    let case = "keepexp";
    let now = h.clock().now();
    let record = record(h, case, "one", 6);
    issue(h, record.clone()).await;
    h.capabilities()
        .revoke(RevokedToken {
            jti: record.jti.clone(),
            expires_at: crate::store::deadline(now, SignedDuration::from_mins(1)),
        })
        .await
        .expect("revokes");
    let entry = ok(h.capabilities().published().await, "read the list")
        .revoked
        .into_iter()
        .find(|token| token.jti == record.jti)
        .expect("published");
    assert_eq!(entry.expires_at, record.expires_at);
    assert_eq!(
        find(h, &record.jti).await.expect("recorded").revoked_at,
        Some(now)
    );
}

/// A record that would outlive the ceiling is refused by the store, at issue and at refresh.
pub async fn a_record_past_the_ceiling_is_refused(h: &dyn Harness) {
    let case = "ceiling";
    let now = h.clock().now();
    let long = record(h, case, "long", 25);
    let error = h
        .capabilities()
        .issue(long.clone())
        .await
        .expect_err("the list is bounded by the ceiling, so the store holds it too");
    assert!(matches!(error, StoreError::Rejected { .. }), "{error:?}");
    assert_eq!(find(h, &long.jti).await, None);

    let old = record(h, case, "old", 6);
    issue(h, old.clone()).await;
    let error = h
        .capabilities()
        .refresh(&old.jti, record(h, case, "long-successor", 25), now)
        .await
        .expect_err("a successor is held to the same ceiling");
    assert!(matches!(error, StoreError::Rejected { .. }), "{error:?}");
    let old = find(h, &old.jti).await.expect("recorded");
    assert_eq!(old.refreshed_to, None, "a refusal changes nothing");
    assert_eq!(old.revoked_at, None);
}

/// A successor for another peer, album or member is refused; the link cannot widen a grant.
pub async fn a_successor_must_carry_the_predecessors_peer_album_and_member(h: &dyn Harness) {
    let case = "widen";
    let now = h.clock().now();
    let old = record(h, case, "old", 6);
    issue(h, old.clone()).await;
    for (name, successor) in [
        (
            "peer",
            CapabilityRecord {
                peer_id: PeerId::new("widen-other.peer.test"),
                ..record(h, case, "peer", 6)
            },
        ),
        (
            "album",
            CapabilityRecord {
                album_id: AlbumId::new("widen-other-album"),
                ..record(h, case, "album", 6)
            },
        ),
        (
            "member",
            CapabilityRecord {
                member: UserId::new("widen-other-member"),
                ..record(h, case, "member", 6)
            },
        ),
    ] {
        let error = h
            .capabilities()
            .refresh(&old.jti, successor.clone(), now)
            .await
            .expect_err("a successor naming another peer, album or member is a rejection");
        assert!(
            matches!(error, StoreError::Rejected { .. }),
            "{name}: {error:?}"
        );
        assert_eq!(
            find(h, &successor.jti).await,
            None,
            "{name}: a refusal records nothing"
        );
    }
    let old = find(h, &old.jti).await.expect("recorded");
    assert_eq!(old.refreshed_to, None);
    assert_eq!(old.revoked_at, None);
}

/// An entry leaves the list once the token it names has expired, and the list orders by expiry.
pub async fn the_published_list_prunes_expired_entries_and_orders_by_expiry(h: &dyn Harness) {
    let case = "prune";
    let now = h.clock().now();
    for (jti, hours) in [("later", 6), ("sooner", 2), ("middle", 4)] {
        let record = record(h, case, jti, hours);
        issue(h, record.clone()).await;
        h.capabilities()
            .revoke_issued(&record.jti, now)
            .await
            .expect("revokes");
    }
    let listed: Vec<String> = published(h)
        .await
        .into_iter()
        .filter(|jti| jti.starts_with("prune-"))
        .collect();
    assert_eq!(listed, ["prune-sooner", "prune-middle", "prune-later"]);

    h.clock().advance(SignedDuration::from_hours(3));
    let list = ok(h.capabilities().published().await, "read the list");
    assert_eq!(list.generated_at, h.clock().now());
    let listed: Vec<String> = list
        .revoked
        .into_iter()
        .map(|token| token.jti)
        .filter(|jti| jti.starts_with("prune-"))
        .collect();
    assert_eq!(
        listed,
        ["prune-middle", "prune-later"],
        "an expired token is refused whether or not it is listed, so its entry carries nothing"
    );
}

/// A refresh issues the successor, links and revokes the predecessor, and replays.
pub async fn a_refresh_is_one_operation_and_a_replay_answers_the_same_successor(h: &dyn Harness) {
    let case = "refresh";
    let now = h.clock().now();
    let old = record(h, case, "old", 6);
    issue(h, old.clone()).await;
    let new = record(h, case, "new", 6);

    let outcome = h
        .capabilities()
        .refresh(&old.jti, new.clone(), now)
        .await
        .expect("refreshes");
    assert_eq!(outcome, RefreshOutcome::Issued(new.clone()));
    let stored_old = find(h, &old.jti).await.expect("recorded");
    assert_eq!(stored_old.refreshed_to, Some(new.jti.clone()));
    assert_eq!(stored_old.revoked_at, Some(now));
    assert!(published(h).await.contains(&old.jti));
    assert_eq!(find(h, &new.jti).await, Some(new.clone()));

    // The replay: a different successor is offered and the first one is answered.
    let another = record(h, case, "another", 6);
    let replay = h
        .capabilities()
        .refresh(&old.jti, another.clone(), now)
        .await
        .expect("answers");
    assert_eq!(replay, RefreshOutcome::AlreadyRefreshed(new));
    assert_eq!(
        find(h, &another.jti).await,
        None,
        "nothing was recorded for the replay"
    );
}

/// A revoked predecessor cannot be refreshed, and an unknown one is unknown.
pub async fn a_revoked_or_unknown_predecessor_is_not_refreshed(h: &dyn Harness) {
    let case = "norefresh";
    let now = h.clock().now();
    let revoked = record(h, case, "revoked", 6);
    issue(h, revoked.clone()).await;
    h.capabilities()
        .revoke_issued(&revoked.jti, now)
        .await
        .expect("revokes");
    let successor = record(h, case, "successor", 6);
    assert_eq!(
        h.capabilities()
            .refresh(&revoked.jti, successor.clone(), now)
            .await
            .expect("answers"),
        RefreshOutcome::Revoked
    );
    assert_eq!(
        h.capabilities()
            .refresh("norefresh-never", successor.clone(), now)
            .await
            .expect("answers"),
        RefreshOutcome::Unknown
    );
    assert_eq!(
        find(h, &successor.jti).await,
        None,
        "a refusal records nothing"
    );
}

// ===========================================================================================
// Peers
// ===========================================================================================

/// An unknown peer is `None`; a pinned one reads back with its key and is not blocked.
pub async fn a_pinned_peer_reads_back_and_an_unknown_one_is_none(h: &dyn Harness) {
    let peer = PeerId::new("pin.peer.test");
    let now = h.clock().now();
    assert_eq!(ok(h.peers().read(&peer).await, "read a peer"), None);
    ok(h.peers().pin(&peer, [7; 32], now).await, "pin a peer");
    let record = ok(h.peers().read(&peer).await, "read a peer").expect("pinned");
    assert_eq!(record.server_id, peer);
    assert_eq!(record.signing_key, Some([7; 32]));
    assert_eq!(record.first_seen_at, now);
    assert!(!record.is_blocked());
    assert_eq!(record.note, None);

    // A re-pin rotates the key and keeps the first-seen instant.
    h.clock().advance(SignedDuration::from_hours(1));
    ok(
        h.peers().pin(&peer, [8; 32], h.clock().now()).await,
        "re-pin a peer",
    );
    let record = ok(h.peers().read(&peer).await, "read a peer").expect("pinned");
    assert_eq!(record.signing_key, Some([8; 32]));
    assert_eq!(record.first_seen_at, now);
}

/// A block on a never-pinned peer creates a keyless row; a second block changes nothing.
pub async fn a_block_needs_no_key_and_is_idempotent(h: &dyn Harness) {
    let peer = PeerId::new("block.peer.test");
    let now = h.clock().now();
    assert_eq!(
        ok(
            h.peers().block(&peer, now, Some("spam".to_owned())).await,
            "block a peer"
        ),
        BlockOutcome::Blocked
    );
    let record = ok(h.peers().read(&peer).await, "read a peer").expect("recorded");
    assert!(record.is_blocked());
    assert_eq!(record.blocked_at, Some(now));
    assert_eq!(record.signing_key, None);
    assert_eq!(record.note.as_deref(), Some("spam"));

    let later = crate::store::deadline(now, SignedDuration::from_hours(1));
    assert_eq!(
        ok(
            h.peers()
                .block(&peer, later, Some("again".to_owned()))
                .await,
            "block a peer again"
        ),
        BlockOutcome::AlreadyBlocked
    );
    let record = ok(h.peers().read(&peer).await, "read a peer").expect("recorded");
    assert_eq!(
        record.blocked_at,
        Some(now),
        "a retry does not move the instant"
    );
    assert_eq!(record.note.as_deref(), Some("spam"));
}

/// Pinning keeps a block, and unblocking keeps the key.
pub async fn a_pin_keeps_a_block_and_an_unblock_keeps_the_key(h: &dyn Harness) {
    let peer = PeerId::new("keep.peer.test");
    let now = h.clock().now();
    ok(h.peers().block(&peer, now, None).await, "block a peer");
    ok(
        h.peers().pin(&peer, [9; 32], now).await,
        "pin a blocked peer",
    );
    let record = ok(h.peers().read(&peer).await, "read a peer").expect("recorded");
    assert!(
        record.is_blocked(),
        "pinning a key is not an opinion about talking to its owner"
    );
    assert_eq!(record.signing_key, Some([9; 32]));

    assert_eq!(
        ok(h.peers().unblock(&peer).await, "unblock a peer"),
        UnblockOutcome::Unblocked
    );
    let record = ok(h.peers().read(&peer).await, "read a peer").expect("recorded");
    assert!(!record.is_blocked());
    assert_eq!(record.signing_key, Some([9; 32]));
    assert_eq!(
        ok(h.peers().unblock(&peer).await, "unblock a peer again"),
        UnblockOutcome::NotBlocked
    );
    assert_eq!(
        ok(
            h.peers()
                .unblock(&PeerId::new("keep-never.peer.test"))
                .await,
            "unblock an unknown peer"
        ),
        UnblockOutcome::NotBlocked
    );
}

/// Every case, against one harness.
pub async fn run_all(h: &dyn Harness) {
    an_issued_capability_reads_back_and_an_unknown_jti_is_none(h).await;
    a_duplicate_jti_is_rejected_and_the_first_record_stands(h).await;
    live_filters_by_album_and_peer_and_excludes_the_revoked_and_expired(h).await;
    revoking_an_issued_capability_publishes_it_once(h).await;
    a_foreign_jti_is_published_and_one_beyond_the_ceiling_is_refused(h).await;
    the_list_and_the_record_are_one_fact(h).await;
    a_list_side_revocation_keeps_the_records_expiry(h).await;
    a_record_past_the_ceiling_is_refused(h).await;
    a_successor_must_carry_the_predecessors_peer_album_and_member(h).await;
    the_published_list_prunes_expired_entries_and_orders_by_expiry(h).await;
    a_refresh_is_one_operation_and_a_replay_answers_the_same_successor(h).await;
    a_revoked_or_unknown_predecessor_is_not_refreshed(h).await;
    a_pinned_peer_reads_back_and_an_unknown_one_is_none(h).await;
    a_block_needs_no_key_and_is_idempotent(h).await;
    a_pin_keeps_a_block_and_an_unblock_keeps_the_key(h).await;
}
