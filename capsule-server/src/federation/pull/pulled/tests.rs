use super::*;
use crate::store::AlbumId;

fn hash(byte: u8) -> Hash32 {
    Hash32([byte; 32])
}

/// A head at `head` that has superseded nothing yet.
fn at(head: Hash32, deleted: bool) -> AssetHead {
    AssetHead {
        head,
        superseded: Vec::new(),
        deleted,
        amk_version: 1,
        metadata_blob: Some(hash(9)),
    }
}

#[tokio::test]
async fn the_in_memory_store_conforms() {
    conformance::run_all(&InMemoryPulledAlbums::new()).await;

    let subscriptions = Arc::new(crate::federation::subscriptions::InMemorySubscriptions::new());
    let budgeted = InMemoryPulledAlbums::new().with_budget(
        subscriptions.clone(),
        PeerCachingBudget::bytes(conformance::BUDGET),
    );
    conformance::run_all(&budgeted).await;
    conformance::run_budget(&budgeted, subscriptions.as_ref()).await;
}

#[test]
fn a_budget_is_its_share_of_the_hard_limit() {
    assert_eq!(PeerCachingBudget::of_hard_limit(1_000, 25).limit(), 250);
    assert_eq!(
        PeerCachingBudget::of_hard_limit(999, 25).limit(),
        249,
        "rounded down"
    );
    assert_eq!(PeerCachingBudget::of_hard_limit(1_000, 0).limit(), 0);
    assert_eq!(
        PeerCachingBudget::of_hard_limit(1_000, 250).limit(),
        1_000,
        "past the whole is the whole"
    );
    assert_eq!(
        PeerCachingBudget::of_hard_limit(u64::MAX, 100).limit(),
        u64::MAX,
        "no overflow on an unlimited deployment"
    );
    assert_eq!(
        PeerCachingBudget::of_hard_limit(u64::MAX, DEFAULT_PEER_BUDGET_PERCENT).limit(),
        u64::MAX / 4
    );
    assert_eq!(PeerCachingBudget::default(), PeerCachingBudget::unlimited());
}

#[test]
fn only_growth_past_the_budget_is_refused() {
    let budget = PeerCachingBudget::bytes(100);
    let (user, peer) = (UserId::new("u"), PeerId::new("home.test"));
    assert_eq!(
        budget.refusal(&user, &peer, 100, 100),
        None,
        "at the budget"
    );
    assert_eq!(
        budget.refusal(&user, &peer, 101, 1),
        Some(OverBudget {
            subscriber: user.clone(),
            peer: peer.clone(),
            used: 101,
            budget: 100,
        })
    );
    assert_eq!(
        budget.refusal(&user, &peer, 1_000, 0),
        None,
        "a commit adding nothing is never refused, however far past the budget"
    );
    assert_eq!(
        PeerCachingBudget::unlimited().refusal(&user, &peer, u64::MAX, u64::MAX),
        None
    );
}

#[test]
fn advancing_a_head_remembers_what_it_superseded() {
    let start = at(hash(1), false);
    let same = start.advanced(at(hash(1), true));
    assert_eq!(same.head, hash(1));
    assert!(
        same.superseded.is_empty(),
        "the same head supersedes nothing"
    );
    assert!(same.deleted);

    let next = start.advanced(AssetHead {
        amk_version: 2,
        metadata_blob: Some(hash(8)),
        ..at(hash(2), false)
    });
    assert_eq!(next.head, hash(2));
    assert_eq!(next.superseded, vec![hash(1)]);
    assert_eq!(next.amk_version, 2, "the epoch is the new record's");
    assert_eq!(next.metadata_blob, Some(hash(8)));

    // History is the held head's, whatever the successor was built with.
    let after = next.advanced(AssetHead {
        superseded: vec![hash(7)],
        ..at(hash(3), false)
    });
    assert_eq!(after.superseded, vec![hash(1), hash(2)]);
}

#[test]
fn the_superseded_heads_are_bounded() {
    let mut head = at(Hash32([0; 32]), false);
    for step in 1..=200_u16 {
        let mut bytes = [0_u8; 32];
        bytes[..2].copy_from_slice(&step.to_be_bytes());
        head = head.advanced(at(Hash32(bytes), false));
    }
    assert_eq!(head.superseded.len(), MAX_SUPERSEDED_HEADS);
    let mut newest = [0_u8; 32];
    newest[..2].copy_from_slice(&199_u16.to_be_bytes());
    assert_eq!(head.superseded.last(), Some(&Hash32(newest)));
}

/// A validated page at `high_water` holding `blobs` and no entries.
fn page(high_water: u64, blobs: &[&[u8]]) -> ValidatedPage {
    ValidatedPage {
        entries: Vec::new(),
        metadata_blobs: blobs
            .iter()
            .map(|bytes| {
                (
                    capsule_core::crypto::hash::hash_bytes(bytes),
                    bytes.to_vec(),
                )
            })
            .collect(),
        state: PulledAlbum {
            cursor: Some(format!("c{high_water}")),
            high_water: Some(high_water),
            ..PulledAlbum::default()
        },
    }
}

/// `user` subscribed to `job`, already lapsed.
async fn subscribe(subscriptions: &dyn SubscriptionStore, user: &UserId, job: &JobKey) {
    use crate::federation::{CapabilityToken, Scope, Subscription};
    let at = Timestamp::UNIX_EPOCH;
    subscriptions
        .put(Subscription {
            subscriber: user.clone(),
            peer: job.peer.clone(),
            album: job.album.clone(),
            scope: Scope::Read,
            jti: format!("quota-{user}-{job}"),
            token: CapabilityToken::new(format!("token-quota-{user}-{job}")),
            issued_at: at,
            expires_at: at,
            min_protocol_version: "2026-06-01".to_owned(),
            subscribed_at: at,
            updated_at: at,
        })
        .await
        .expect("subscribing");
}

/// What `quotas` says `user` uses.
async fn used(quotas: &crate::quota::InMemoryQuota, user: &UserId) -> u64 {
    use crate::quota::QuotaStore as _;
    quotas.usage(user).await.expect("reading usage").used
}

/// The in-memory twin of the Postgres `charged_to_quota` case: a written page charges every
/// subscriber's `quota_used`, lapsed ones too; a page the budget refuses charges nobody; an
/// unsubscribe releases one subscriber only.
#[tokio::test]
async fn the_budgeted_in_memory_store_is_charged_to_quota() {
    let subscriptions = Arc::new(crate::federation::subscriptions::InMemorySubscriptions::new());
    let store = Arc::new(InMemoryPulledAlbums::new().with_budget(
        subscriptions.clone(),
        PeerCachingBudget::bytes(conformance::BUDGET),
    ));
    let quotas = crate::quota::InMemoryQuota::new().with_pulled(store.clone());

    let job = JobKey::new(PeerId::new("home.quota.test"), AlbumId::new("quota-a"));
    let (alice, bob) = (UserId::new("quota-alice"), UserId::new("quota-bob"));
    subscribe(subscriptions.as_ref(), &alice, &job).await;
    subscribe(subscriptions.as_ref(), &bob, &job).await;

    let held: &[u8] = b"a metadata blob the quota counts";
    assert_eq!(
        store
            .commit(&job, None, page(1, &[held]))
            .await
            .expect("commit"),
        Committed::Written
    );
    let size = u64::try_from(held.len()).expect("a small size");
    assert_eq!(
        used(&quotas, &alice).await,
        size,
        "charged in full to each subscriber"
    );
    assert_eq!(used(&quotas, &bob).await, size);

    let over = vec![7_u8; usize::try_from(conformance::BUDGET).expect("a small budget")];
    assert!(matches!(
        store
            .commit(&job, Some(1), page(2, &[held, &over]))
            .await
            .expect("commit"),
        Committed::OverBudget(_)
    ));
    assert_eq!(
        used(&quotas, &alice).await,
        size,
        "a refused page charges nothing"
    );

    assert!(
        subscriptions
            .remove(&alice, &job.peer, &job.album)
            .await
            .expect("unsubscribing")
    );
    assert_eq!(
        used(&quotas, &alice).await,
        0,
        "the unsubscribe released it"
    );
    assert_eq!(used(&quotas, &bob).await, size, "for that subscriber only");
}

/// A blob held through two of an account's albums is counted once, whichever peers they are
/// on, and a store assembled without a budget knows no subscriber to count anything for.
#[tokio::test]
async fn pulled_holdings_count_each_distinct_blob_once_across_peers() {
    use crate::quota::PulledHoldings as _;

    let subscriptions = Arc::new(crate::federation::subscriptions::InMemorySubscriptions::new());
    let store = InMemoryPulledAlbums::new()
        .with_budget(subscriptions.clone(), PeerCachingBudget::unlimited());
    let user = UserId::new("holder");
    let here = JobKey::new(PeerId::new("one.test"), AlbumId::new("a"));
    let there = JobKey::new(PeerId::new("two.test"), AlbumId::new("b"));
    let unsubscribed = JobKey::new(PeerId::new("one.test"), AlbumId::new("c"));
    subscribe(subscriptions.as_ref(), &user, &here).await;
    subscribe(subscriptions.as_ref(), &user, &there).await;

    let (shared, own, foreign): (&[u8], &[u8], &[u8]) = (
        b"held through both",
        b"held through one",
        b"held for nobody",
    );
    for (job, blobs) in [
        (&here, vec![shared]),
        (&there, vec![shared, own]),
        (&unsubscribed, vec![foreign]),
    ] {
        assert_eq!(
            store
                .commit(job, None, page(1, &blobs))
                .await
                .expect("commit"),
            Committed::Written
        );
    }

    let address = |bytes: &[u8]| {
        crate::blob::ContentAddress::parse(&capsule_core::crypto::hash::hash_bytes(bytes).to_hex())
            .expect("an address")
    };
    let size = |bytes: &[u8]| u64::try_from(bytes.len()).expect("a small size");
    assert_eq!(
        store.held_by(&user).await.expect("holdings"),
        BTreeMap::from([(address(shared), size(shared)), (address(own), size(own))])
    );
    assert!(
        store
            .held_by(&UserId::new("stranger"))
            .await
            .expect("holdings")
            .is_empty()
    );

    let unbudgeted = InMemoryPulledAlbums::new();
    assert_eq!(
        unbudgeted
            .commit(&here, None, page(1, &[shared]))
            .await
            .expect("commit"),
        Committed::Written
    );
    assert!(
        unbudgeted
            .held_by(&user)
            .await
            .expect("holdings")
            .is_empty(),
        "no subscriptions, so no subscriber"
    );
}
