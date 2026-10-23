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
    fn hit<'a>(&'a self, _: &'a CounterKey, _: Budget, _: Timestamp) -> StoreFuture<'a, Verdict> {
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
        Ok(())
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
        Ok(())
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
