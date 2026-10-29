use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};

use super::*;
use crate::counter::{Budget, CounterStore, InMemoryCounters, Verdict};
use crate::store::memory::ManualClock;
use crate::store::{AlbumId, StoreError, StoreFuture, UserId};

#[test]
fn a_peer_id_is_canonical() {
    assert_eq!(PeerId::new("Other.Test."), PeerId::new("other.test"));
    assert_eq!(PeerId::new(" other.test ").as_str(), "other.test");
    assert_ne!(PeerId::new("other.test"), PeerId::new("another.test"));
}

#[test]
fn a_caller_supplied_peer_must_be_a_host_name() {
    for accepted in [
        "other.test",
        "Other.Test.",
        " localhost ",
        "a-b.c1.example",
        "10.0.0.1",
    ] {
        assert_eq!(
            PeerId::parse(accepted),
            Ok(PeerId::new(accepted)),
            "{accepted:?}"
        );
    }
    let longest = format!(
        "{}.{}.{}.{}",
        "a".repeat(63),
        "b".repeat(63),
        "c".repeat(63),
        "d".repeat(61)
    );
    assert_eq!(longest.len(), MAX_PEER_ID_BYTES);
    assert!(PeerId::parse(&longest).is_ok());
    for refused in [
        String::new(),
        "   ".to_owned(),
        "other.test:443".to_owned(),
        "https://other.test".to_owned(),
        "other.test/path".to_owned(),
        "user@other.test".to_owned(),
        "other..test".to_owned(),
        "-other.test".to_owned(),
        "other-.test".to_owned(),
        "oth er.test".to_owned(),
        "ötherr.test".to_owned(),
        format!("{}.test", "a".repeat(64)),
        format!("{longest}a"),
    ] {
        assert_eq!(PeerId::parse(&refused), Err(InvalidPeerId), "{refused:?}");
    }
}

/// A counter that cannot be reached.
#[derive(Debug)]
struct DownCounters;

fn down<T>() -> StoreFuture<'static, T> {
    Box::pin(async {
        Err(StoreError::Unavailable {
            store: "counters",
            detail: "down".to_owned(),
        })
    })
}

impl CounterStore for DownCounters {
    fn charge<'a>(
        &'a self,
        _: &'a CounterKey,
        _: u32,
        _: Budget,
        _: Timestamp,
    ) -> StoreFuture<'a, Verdict> {
        down()
    }

    fn peek<'a>(&'a self, _: &'a CounterKey, _: Budget, _: Timestamp) -> StoreFuture<'a, Verdict> {
        down()
    }

    fn reset<'a>(&'a self, _: &'a CounterKey) -> StoreFuture<'a, ()> {
        down()
    }
}

/// A peer store that cannot be reached.
#[derive(Debug)]
struct DownPeers;

impl PeerStore for DownPeers {
    fn pin<'a>(&'a self, _: &'a PeerId, _: [u8; 32], _: Timestamp) -> StoreFuture<'a, ()> {
        down()
    }

    fn read<'a>(&'a self, _: &'a PeerId) -> StoreFuture<'a, Option<PeerRecord>> {
        down()
    }

    fn block<'a>(
        &'a self,
        _: &'a PeerId,
        _: Timestamp,
        _: Option<String>,
    ) -> StoreFuture<'a, BlockOutcome> {
        down()
    }

    fn unblock<'a>(&'a self, _: &'a PeerId) -> StoreFuture<'a, UnblockOutcome> {
        down()
    }
}

fn context(peers: Arc<dyn PeerStore>, clock: Arc<ManualClock>) -> FederationContext {
    let der = ring::signature::Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
        .expect("a key generates");
    FederationContext::new(FederationCollaborators {
        codec: Arc::new(
            CapabilityCodec::from_pkcs8(der.as_ref(), "home.test", clock.clone()).expect("parses"),
        ),
        capabilities: Arc::new(InMemoryCapabilities::new(clock.clone())),
        peers,
        clock,
        enabled: Arc::new(true),
    })
}

fn verified(clock: &ManualClock) -> VerifiedCapability {
    let now = clock.now();
    let record = CapabilityRecord {
        jti: "01937b7c-0000-7000-8000-0000000000aa".to_owned(),
        album_id: AlbumId::new("album"),
        peer_id: PeerId::new("other.test"),
        member: UserId::new("bob"),
        scope: Scope::Read,
        granted_epoch: 1,
        min_protocol_version: "2026-06-01".to_owned(),
        issued_at: now,
        expires_at: crate::store::deadline(now, SignedDuration::from_hours(1)),
        not_after: crate::store::deadline(now, SignedDuration::from_hours(1)),
        revoked_at: None,
        refreshed_to: None,
    };
    VerifiedCapability {
        grant: record.grant(),
        record,
        foreseen: None,
    }
}

#[tokio::test]
async fn a_predecessor_revoked_by_its_own_refresh_is_admitted_only_to_be_refreshed() {
    // What makes a replayed refresh answerable: the predecessor is revoked the moment its
    // successor is issued, and a rule that refused every revoked grant would make the
    // idempotency the contract promises unreachable. A read is still refused.
    let clock = Arc::new(ManualClock::default());
    let mut capability = verified(&clock);
    capability.record.revoked_at = Some(clock.now());
    capability.record.refreshed_to = Some("01937b7c-0000-7000-8000-0000000000bb".to_owned());
    let federation = context(Arc::new(InMemoryPeers::new()), clock.clone());
    let counters = CounterContext::new(Arc::new(InMemoryCounters::new()), clock);
    assert_eq!(
        admit(&federation, &counters, &capability, Presentation::Refresh).await,
        Ok(PeerTier::Probation)
    );
    assert_eq!(
        admit(&federation, &counters, &capability, Presentation::Read).await,
        Err(Refusal::Revoked)
    );

    // A grant revoked without a successor is refused on both.
    capability.record.refreshed_to = None;
    assert_eq!(
        admit(&federation, &counters, &capability, Presentation::Refresh).await,
        Err(Refusal::Revoked)
    );
}

#[tokio::test]
async fn a_store_that_cannot_answer_an_admission_is_an_outage_never_an_admission() {
    // The fail-closed rule at the seam every federated read passes through: a peer store
    // or a counter that cannot be reached decides nothing, and "nothing" is a refusal.
    let clock = Arc::new(ManualClock::default());
    let capability = verified(&clock);

    let federation = context(Arc::new(DownPeers), clock.clone());
    let counters = CounterContext::new(Arc::new(InMemoryCounters::new()), clock.clone());
    assert_eq!(
        admit(&federation, &counters, &capability, Presentation::Read).await,
        Err(Refusal::Unavailable)
    );

    let federation = context(Arc::new(InMemoryPeers::new()), clock.clone());
    let counters = CounterContext::new(Arc::new(DownCounters), clock.clone());
    assert_eq!(
        admit(&federation, &counters, &capability, Presentation::Read).await,
        Err(Refusal::Unavailable)
    );

    let counters = CounterContext::new(Arc::new(InMemoryCounters::new()), clock);
    assert_eq!(
        admit(&federation, &counters, &capability, Presentation::Read).await,
        Ok(PeerTier::Probation)
    );
}

fn pinned(first_seen_at: Timestamp) -> PeerRecord {
    PeerRecord {
        server_id: PeerId::new("other.test"),
        signing_key: Some([7; 32]),
        first_seen_at,
        blocked_at: None,
        note: None,
    }
}

#[test]
fn a_peer_is_on_probation_until_this_server_has_known_it_for_the_probation_period() {
    let pinned_at = Timestamp::UNIX_EPOCH;
    let graduates = crate::store::deadline(pinned_at, budgets::PEER_PROBATION);
    assert_eq!(PeerTier::of(None, graduates), PeerTier::Probation);
    assert_eq!(
        PeerTier::of(Some(&pinned(pinned_at)), pinned_at),
        PeerTier::Probation
    );
    assert_eq!(
        PeerTier::of(
            Some(&pinned(pinned_at)),
            crate::store::deadline(graduates, SignedDuration::from_secs(-1))
        ),
        PeerTier::Probation
    );
    assert_eq!(
        PeerTier::of(Some(&pinned(pinned_at)), graduates),
        PeerTier::Established
    );
    assert!(PeerTier::Probation.requests().limit < PeerTier::Established.requests().limit);
    assert!(PeerTier::Probation.kibibytes().limit < PeerTier::Established.kibibytes().limit);
}

#[tokio::test]
async fn admission_charges_the_events_budget_of_the_tier_it_answers() {
    // Two peers spending the same requests: the one on probation runs out at its own,
    // smaller, budget while the established one is still admitted.
    let clock = Arc::new(ManualClock::default());
    let peers = Arc::new(InMemoryPeers::new());
    let established_at = clock.now();
    peers
        .pin(&PeerId::new("other.test"), [7; 32], established_at)
        .await
        .expect("pins");
    clock.advance(budgets::PEER_PROBATION);
    let federation = context(peers.clone(), clock.clone());
    let counters = CounterContext::new(Arc::new(InMemoryCounters::new()), clock.clone());
    let capability = verified(&clock);
    assert_eq!(
        admit(&federation, &counters, &capability, Presentation::Read).await,
        Ok(PeerTier::Established)
    );

    let mut newcomer = verified(&clock);
    newcomer.record.peer_id = PeerId::new("new.test");
    peers
        .pin(&PeerId::new("new.test"), [8; 32], clock.now())
        .await
        .expect("pins");
    for _ in 0..budgets::PEER_REQUESTS_PROBATION.limit {
        assert_eq!(
            admit(&federation, &counters, &newcomer, Presentation::Read).await,
            Ok(PeerTier::Probation)
        );
    }
    assert!(matches!(
        admit(&federation, &counters, &newcomer, Presentation::Read).await,
        Err(Refusal::RateLimited { .. })
    ));
    assert_eq!(
        admit(&federation, &counters, &capability, Presentation::Read).await,
        Ok(PeerTier::Established),
        "one peer's spent budget is not another's"
    );
}

#[test]
fn a_delivery_is_charged_in_whole_kibibytes_rounded_up() {
    assert_eq!(kibibytes(0), 0);
    assert_eq!(kibibytes(1), 1);
    assert_eq!(kibibytes(1024), 1);
    assert_eq!(kibibytes(1025), 2);
    assert_eq!(
        kibibytes(u64::MAX),
        u32::MAX,
        "saturates rather than wrapping"
    );
}

#[tokio::test]
async fn served_bytes_are_charged_in_one_call_and_refused_whole_past_the_budget() {
    let clock = Arc::new(ManualClock::default());
    let counters = CounterContext::new(Arc::new(InMemoryCounters::new()), clock.clone());
    let peer = PeerId::new("other.test");
    let budget = PeerTier::Probation.kibibytes();
    let half = u64::from(budget.limit) * 1024 / 2;

    assert_eq!(
        charge_served_bytes(&counters, &peer, PeerTier::Probation, half).await,
        Ok(())
    );
    assert!(
        matches!(
            charge_served_bytes(&counters, &peer, PeerTier::Probation, half + 1).await,
            Err(Refusal::RateLimited { .. })
        ),
        "a blob that does not fit what is left is refused whole"
    );
    assert_eq!(
        charge_served_bytes(&counters, &peer, PeerTier::Probation, half).await,
        Ok(()),
        "and the refusal spent nothing, so exactly what was left still fits"
    );
    assert_eq!(
        charge_served_bytes(
            &counters,
            &PeerId::new("another.test"),
            PeerTier::Probation,
            half
        )
        .await,
        Ok(()),
        "each peer is its own budget"
    );

    let down = CounterContext::new(Arc::new(DownCounters), clock);
    assert_eq!(
        charge_served_bytes(&down, &peer, PeerTier::Established, 1).await,
        Err(Refusal::Unavailable),
        "a counter that cannot answer refuses"
    );
}

#[tokio::test]
async fn a_revoked_capability_is_refused_before_the_peer_is_charged() {
    let clock = Arc::new(ManualClock::default());
    let mut capability = verified(&clock);
    capability.record.revoked_at = Some(clock.now());
    let federation = context(Arc::new(InMemoryPeers::new()), clock.clone());
    let store = Arc::new(InMemoryCounters::new());
    let counters = CounterContext::new(store.clone(), clock.clone());
    assert_eq!(
        admit(&federation, &counters, &capability, Presentation::Read).await,
        Err(Refusal::Revoked)
    );
    assert_eq!(
        store
            .peek(
                &CounterKey::PeerRequests("other.test".to_owned()),
                budgets::PEER_REQUESTS,
                clock.now(),
            )
            .await
            .expect("answers"),
        Verdict::Admitted {
            remaining: budgets::PEER_REQUESTS.limit
        },
        "a revoked grant costs the peer nothing"
    );
}

#[tokio::test]
async fn a_foreseen_refusal_is_answered_after_an_unblock_or_a_window_rollover() {
    // The authenticator left the nonce unspent because the peer was blocked or over budget.
    // By the time the route admits, the peer row and the budget read clean — the race the
    // field closes: admitting now would serve a signature whose nonce was never recorded.
    let clock = Arc::new(ManualClock::default());
    let federation = context(Arc::new(InMemoryPeers::new()), clock.clone());
    let store = Arc::new(InMemoryCounters::new());
    let counters = CounterContext::new(store.clone(), clock.clone());
    let retry_after = clock.now();
    for foreseen in [Refusal::PeerBlocked, Refusal::RateLimited { retry_after }] {
        let mut capability = verified(&clock);
        capability.foreseen = Some(foreseen);
        assert_eq!(
            admit(&federation, &counters, &capability, Presentation::Read).await,
            Err(foreseen)
        );
    }
    assert_eq!(
        store
            .peek(
                &CounterKey::PeerRequests("other.test".to_owned()),
                budgets::PEER_REQUESTS,
                clock.now(),
            )
            .await
            .expect("answers"),
        Verdict::Admitted {
            remaining: budgets::PEER_REQUESTS.limit
        },
        "a request refused as foreseen costs the peer nothing"
    );

    // A revocation still comes first, as it does for every other refusal.
    let mut capability = verified(&clock);
    capability.foreseen = Some(Refusal::PeerBlocked);
    capability.record.revoked_at = Some(clock.now());
    assert_eq!(
        admit(&federation, &counters, &capability, Presentation::Read).await,
        Err(Refusal::Revoked)
    );
}
