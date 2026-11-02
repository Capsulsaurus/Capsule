use super::*;

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
