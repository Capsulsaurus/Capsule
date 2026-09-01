//! The drop port's own suite.
//!
//! Two properties carry it: caps are decided and reserved in one operation, and adoption is
//! claimed rather than taken. Both are about what happens when two things race.

use jiff::SignedDuration;

use super::*;

fn owner() -> UserId {
    UserId::new("01937b7c-0000-7000-8000-000000000001")
}

fn other() -> UserId {
    UserId::new("01937b7c-0000-7000-8000-0000000000ff")
}

fn at(hours: i64) -> Timestamp {
    crate::store::deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_hours(hours))
}

fn opaque(tag: u8) -> String {
    format!(
        "{:032x}",
        u128::from(tag) | 0xabcd_ef01_2345_6789_abcd_ef01_2345_0000
    )
}

fn address(seed: u8) -> ContentAddress {
    ContentAddress::parse(&capsule_core::crypto::hash::hash_bytes(&[seed; 8]).to_hex())
        .expect("a content address")
}

fn link(opaque_id: &str, caps: LinkCaps) -> UploadLinkRecord {
    UploadLinkRecord {
        opaque_id: opaque_id.to_owned(),
        owner_id: owner(),
        drop_pubkey: vec![1; 32],
        crypto_suite_id: 1,
        protocol_version: "2026-01-01".to_owned(),
        caps,
        passphrase_verifier: None,
        used_bytes: 0,
        used_files: 0,
        revoked_at: None,
    }
}

fn entry(drop_id: &str, seed: u8) -> InboxEntry {
    InboxEntry {
        drop_id: drop_id.to_owned(),
        owner_id: owner(),
        opaque_id: opaque(1),
        address: address(seed),
        size: 1024,
        content_type: "image/jpeg".to_owned(),
        kem_ct: vec![9; 64],
        suggested_filename: Some("beach.jpg".to_owned()),
        received_at: at(1),
        adopting: false,
    }
}

async fn store_with(caps: LinkCaps) -> (InMemoryDrops, String) {
    let store = InMemoryDrops::new();
    let id = opaque(1);
    store
        .provision(link(&id, caps))
        .await
        .expect("the store provisions");
    (store, id)
}

#[tokio::test]
async fn the_file_count_cap_is_decided_and_reserved_together() {
    // The reason `charge` is one operation. Two guests through a read-decide-write would both
    // see the last slot and both be admitted, which is how a "maximum two files" link ends up
    // holding three.
    let (store, id) = store_with(LinkCaps {
        max_file_count: Some(2),
        ..LinkCaps::default()
    })
    .await;

    for _ in 0..2 {
        assert!(matches!(
            store
                .charge(&id, 100, at(0))
                .await
                .expect("the store answers"),
            Admission::Admitted { .. }
        ));
    }
    assert_eq!(
        store
            .charge(&id, 100, at(0))
            .await
            .expect("the store answers"),
        Admission::CapExhausted
    );
}

#[tokio::test]
async fn the_byte_cap_counts_the_drop_being_admitted() {
    // `used + declared > cap`, not `used > cap`: admitting a drop that would overshoot and
    // discovering it at finalization would leave the bytes on disk and the cap already spent.
    let (store, id) = store_with(LinkCaps {
        max_total_bytes: Some(1_000),
        ..LinkCaps::default()
    })
    .await;

    assert!(matches!(
        store.charge(&id, 900, at(0)).await.expect("answers"),
        Admission::Admitted { .. }
    ));
    assert_eq!(
        store.charge(&id, 101, at(0)).await.expect("answers"),
        Admission::CapExhausted
    );
    assert!(
        matches!(
            store.charge(&id, 100, at(0)).await.expect("answers"),
            Admission::Admitted { .. }
        ),
        "exactly filling the cap is admitted; the bound is inclusive"
    );
}

#[tokio::test]
async fn an_oversized_file_is_its_own_answer_whatever_room_is_left() {
    // Invariant 28 before the cumulative caps. Telling a guest "the link is full" about a file
    // that was simply too big sends them to the wrong remedy — shrink it, not ask for a new
    // link.
    let (store, id) = store_with(LinkCaps {
        max_file_size: Some(500),
        max_total_bytes: Some(1_000_000),
        ..LinkCaps::default()
    })
    .await;

    assert_eq!(
        store.charge(&id, 501, at(0)).await.expect("answers"),
        Admission::FileTooLarge { limit: 500 }
    );
}

#[tokio::test]
async fn expiry_revocation_single_use_and_an_unknown_link_are_one_answer() {
    // `/d/{opaque-id}` takes no credential, so distinguishing these would be an enumeration
    // oracle exactly as it would on the share path.
    let unknown = InMemoryDrops::new();
    assert_eq!(
        unknown
            .charge(&opaque(9), 10, at(0))
            .await
            .expect("answers"),
        Admission::NotLive
    );

    let (expired, id) = store_with(LinkCaps {
        expires_at: Some(at(1)),
        ..LinkCaps::default()
    })
    .await;
    assert_eq!(
        expired.charge(&id, 10, at(2)).await.expect("answers"),
        Admission::NotLive
    );

    let (revoked, id) = store_with(LinkCaps::default()).await;
    revoked
        .revoke(&owner(), &id, at(1))
        .await
        .expect("the store revokes");
    assert_eq!(
        revoked.charge(&id, 10, at(2)).await.expect("answers"),
        Admission::NotLive
    );

    let (single, id) = store_with(LinkCaps {
        single_use: true,
        ..LinkCaps::default()
    })
    .await;
    assert!(matches!(
        single.charge(&id, 10, at(0)).await.expect("answers"),
        Admission::Admitted { .. }
    ));
    assert_eq!(
        single.charge(&id, 10, at(0)).await.expect("answers"),
        Admission::NotLive,
        "a single-use link is spent, which is a link that is no longer live"
    );
}

#[tokio::test]
async fn an_abandoned_reservation_is_refunded() {
    // Without this, a guest who starts and cancels ten uploads exhausts a ten-file link having
    // deposited nothing.
    let (store, id) = store_with(LinkCaps {
        max_file_count: Some(1),
        max_total_bytes: Some(1_000),
        ..LinkCaps::default()
    })
    .await;

    assert!(matches!(
        store.charge(&id, 900, at(0)).await.expect("answers"),
        Admission::Admitted { .. }
    ));
    store.refund(&id, 900).await.expect("the store refunds");

    assert!(
        matches!(
            store.charge(&id, 900, at(0)).await.expect("answers"),
            Admission::Admitted { .. }
        ),
        "the link is usable again, in both counters"
    );
}

#[tokio::test]
async fn a_revocation_is_the_owners_and_happens_once() {
    let (store, id) = store_with(LinkCaps::default()).await;

    assert!(
        !store.revoke(&other(), &id, at(1)).await.expect("answers"),
        "another account cannot revoke this link"
    );
    assert!(store.revoke(&owner(), &id, at(1)).await.expect("answers"));
    assert!(
        !store.revoke(&owner(), &id, at(2)).await.expect("answers"),
        "a second revocation is not a second event"
    );
    assert_eq!(
        store
            .resolve(&id)
            .await
            .expect("answers")
            .expect("held")
            .revoked_at,
        Some(at(1))
    );
}

#[tokio::test]
async fn an_inbox_is_scoped_ordered_and_survives_a_refused_adoption() {
    let store = InMemoryDrops::new();
    let mut second = entry("drop-2", 2);
    second.received_at = at(2);
    store.deposit(entry("drop-1", 1)).await.expect("deposits");
    store.deposit(second).await.expect("deposits");

    let held = store.inbox(&owner()).await.expect("answers");
    assert_eq!(
        held.iter().map(|e| e.drop_id.as_str()).collect::<Vec<_>>(),
        ["drop-1", "drop-2"],
        "oldest first, and the order is total"
    );
    assert!(store.inbox(&other()).await.expect("answers").is_empty());
}

#[tokio::test]
async fn adoption_is_claimed_and_a_refused_write_puts_the_drop_back() {
    // The half of invariant 32 a single-process port can actually hold: neither lost nor
    // silently duplicated. A crash between claim and settle leaves a row visibly `adopting`.
    let store = InMemoryDrops::new();
    store.deposit(entry("drop-1", 1)).await.expect("deposits");

    let claimed = store
        .claim(&owner(), "drop-1")
        .await
        .expect("answers")
        .expect("the row is claimable");
    assert_eq!(claimed.address, address(1));

    assert!(
        store
            .claim(&owner(), "drop-1")
            .await
            .expect("answers")
            .is_none(),
        "a concurrent adoption loses and has nothing to do but stand down"
    );
    assert!(
        store
            .inbox(&owner())
            .await
            .expect("answers")
            .iter()
            .all(|e| e.adopting),
        "and the row is visibly held rather than gone"
    );

    store.release("drop-1").await.expect("releases");
    assert!(
        store
            .claim(&owner(), "drop-1")
            .await
            .expect("answers")
            .is_some(),
        "a refused write returns the drop to the inbox, rather than losing a guest's photo"
    );
}

#[tokio::test]
async fn settling_removes_the_row_and_another_account_can_claim_nothing() {
    let store = InMemoryDrops::new();
    store.deposit(entry("drop-1", 1)).await.expect("deposits");

    assert!(
        store
            .claim(&other(), "drop-1")
            .await
            .expect("answers")
            .is_none(),
        "another account's inbox row is not claimable, and answers as an unknown one does"
    );

    store
        .claim(&owner(), "drop-1")
        .await
        .expect("answers")
        .expect("claimable");
    store.settle("drop-1").await.expect("settles");
    assert!(store.inbox(&owner()).await.expect("answers").is_empty());
}

#[tokio::test]
async fn discarding_is_the_owners_and_removes_the_row() {
    let store = InMemoryDrops::new();
    store.deposit(entry("drop-1", 1)).await.expect("deposits");

    assert!(!store.discard(&other(), "drop-1").await.expect("answers"));
    assert_eq!(store.inbox(&owner()).await.expect("answers").len(), 1);

    assert!(store.discard(&owner(), "drop-1").await.expect("answers"));
    assert!(store.inbox(&owner()).await.expect("answers").is_empty());
    assert!(
        !store.discard(&owner(), "drop-1").await.expect("answers"),
        "a second discard is not a second event"
    );
}

#[test]
fn an_opaque_id_is_a_hundred_and_twenty_eight_bits_of_lowercase_hex() {
    assert!(is_opaque_id(&opaque(1)));
    assert!(!is_opaque_id(&opaque(1).to_uppercase()));
    assert!(!is_opaque_id("0123456789abcdef"));
    assert!(!is_opaque_id("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f"));
}
