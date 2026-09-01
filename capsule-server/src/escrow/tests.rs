//! The escrow port's own suite.
//!
//! Two properties carry it: the bytes come back exactly as they went in, and storing a new
//! escrow leaves nothing of the old one. The second is the guided re-wrap contract — a rotation
//! whose point is that the lost recovery secret stops working.

use super::*;

fn user() -> UserId {
    UserId::new("01937b7c-0000-7000-8000-000000000001")
}

fn other() -> UserId {
    UserId::new("01937b7c-0000-7000-8000-0000000000ff")
}

fn record(user: &UserId, blob: &[u8], at: Timestamp) -> EscrowRecord {
    EscrowRecord {
        user_id: user.clone(),
        blob: blob.to_vec(),
        stored_at: at,
    }
}

#[tokio::test]
async fn a_stored_escrow_comes_back_byte_for_byte() {
    // The server never derives, unwraps or re-encodes: what it hands back has to be what a
    // client can run its KDF against, and a re-encoded wrap is a wrap that no longer opens.
    let store = InMemoryEscrow::new();
    let blob = b"\x00\xff wrapped master key \x01\x02".to_vec();

    assert_eq!(
        store
            .store(record(&user(), &blob, Timestamp::UNIX_EPOCH))
            .await
            .expect("the store accepts"),
        Replaced::No
    );

    let held = store
        .fetch(&user())
        .await
        .expect("the store answers")
        .expect("an escrow is held");
    assert_eq!(held.blob, blob);
    assert_eq!(held.stored_at, Timestamp::UNIX_EPOCH);
}

#[tokio::test]
async fn storing_a_new_escrow_leaves_nothing_of_the_old_one() {
    // The single-active-escrow rule. After a guided re-wrap the lost recovery secret must unwrap
    // nothing — a server that kept the previous blob would preserve exactly the artifact the
    // rotation exists to destroy.
    let store = InMemoryEscrow::new();
    store
        .store(record(&user(), b"first wrap", Timestamp::UNIX_EPOCH))
        .await
        .expect("the store accepts");

    assert_eq!(
        store
            .store(record(&user(), b"second wrap", Timestamp::UNIX_EPOCH))
            .await
            .expect("the store accepts"),
        Replaced::Yes,
        "a rotation is a different event from a first escrow, and a client acts on which it was"
    );

    let held = store
        .fetch(&user())
        .await
        .expect("the store answers")
        .expect("an escrow is held");
    assert_eq!(held.blob, b"second wrap");
}

#[tokio::test]
async fn an_escrow_is_scoped_to_its_account() {
    let store = InMemoryEscrow::new();
    store
        .store(record(&user(), b"mine", Timestamp::UNIX_EPOCH))
        .await
        .expect("the store accepts");

    assert_eq!(
        store.fetch(&other()).await.expect("the store answers"),
        None
    );
}

#[test]
fn an_empty_body_is_not_an_escrow() {
    assert_eq!(admissible(b""), Err(MalformedEscrow::Empty));
}

#[test]
fn a_body_past_the_ceiling_is_refused() {
    let huge = vec![0_u8; MAX_ESCROW_BYTES + 1];
    assert_eq!(
        admissible(&huge),
        Err(MalformedEscrow::TooLarge {
            size: MAX_ESCROW_BYTES + 1
        })
    );
}

#[test]
fn anything_of_a_plausible_size_is_admissible() {
    // The bound is a refusal to store something that cannot be an escrow at any version — not a
    // format check. The server cannot tell a real wrap from noise of the same length, and
    // pretending otherwise would put a format it does not own on its critical path.
    assert_eq!(admissible(b"x"), Ok(()));
    assert_eq!(admissible(&vec![0_u8; MAX_ESCROW_BYTES]), Ok(()));
}
