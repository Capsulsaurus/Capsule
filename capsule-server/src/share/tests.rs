//! The share port's own suite.
//!
//! Two properties carry it: a link serves only what its record enumerates, and a link that is
//! not live serves nothing. Everything else on this surface is a consequence of those.

use capsule_core::sharing::ShareScope;
use jiff::SignedDuration;

use super::*;

fn owner() -> UserId {
    UserId::new("01937b7c-0000-7000-8000-000000000001")
}

fn address(seed: u8) -> ContentAddress {
    ContentAddress::parse(&capsule_core::crypto::hash::hash_bytes(&[seed; 8]).to_hex())
        .expect("a content address")
}

fn at(hours: i64) -> Timestamp {
    crate::store::deadline(Timestamp::UNIX_EPOCH, SignedDuration::from_hours(hours))
}

fn record(opaque_id: &str) -> ShareRecord {
    let metadata = address(1);
    ShareRecord {
        opaque_id: opaque_id.to_owned(),
        owner_id: owner(),
        scope: ShareScope::Asset(uuid::Uuid::from_u128(7)),
        serves: [metadata.clone(), address(2)].into_iter().collect(),
        metadata,
        wrapped_secret: None,
        expires_at: None,
        revoked_at: None,
    }
}

/// A well-formed opaque id: 32 lowercase hex characters, with letters in it so a
/// case-sensitivity assertion has something to bite on.
fn opaque(tag: u8) -> String {
    format!(
        "{:032x}",
        u128::from(tag) | 0xabcd_ef01_2345_6789_abcd_ef01_2345_0000
    )
}

#[test]
fn an_opaque_id_is_exactly_a_hundred_and_twenty_eight_bits_of_lowercase_hex() {
    // Structural, and the contract says so: 128 bits is the defense against enumeration
    // *independent of rate limiting*, so a shorter or structured id fails here rather than
    // being caught later by a limiter that does not exist yet.
    assert!(is_opaque_id(&opaque(1)));
    assert!(!is_opaque_id(""), "empty");
    assert!(
        !is_opaque_id(&opaque(1)[..31]),
        "31 characters is not 128 bits"
    );
    assert!(!is_opaque_id(&format!("{}0", opaque(1))), "33 characters");
    assert!(
        !is_opaque_id(&opaque(1).to_uppercase()),
        "one spelling per id, so two requests for one link cannot look like two links"
    );
    assert!(!is_opaque_id(&"g".repeat(32)), "not hex");
    assert_eq!(OPAQUE_ID_HEX_LEN, 32);
}

#[test]
fn a_link_serves_only_what_its_record_enumerates() {
    // The enforceable half of the privacy strip. A link that resolved an album and served
    // whatever it held would serve the unstripped metadata the user never meant to export.
    let record = record(&opaque(1));
    assert!(record.serves(&address(1)));
    assert!(record.serves(&address(2)));
    assert!(
        !record.serves(&address(3)),
        "a blob the link does not name is not the link's to serve, whoever else holds it"
    );
}

#[test]
fn liveness_covers_expiry_and_revocation_the_same_way() {
    let mut live = record(&opaque(1));
    assert!(live.is_live_at(at(0)));

    live.expires_at = Some(at(2));
    assert!(live.is_live_at(at(1)));
    assert!(
        !live.is_live_at(at(2)),
        "expiry is exclusive at the instant"
    );
    assert!(!live.is_live_at(at(3)));

    let mut revoked = record(&opaque(2));
    revoked.revoked_at = Some(at(1));
    assert!(
        !revoked.is_live_at(at(0)),
        "a revoked link is dead even at an instant before the revocation was recorded — the \
         alternative is a clock skew that serves a revoked link"
    );
}

#[tokio::test]
async fn a_revocation_is_the_owners_and_happens_once() {
    let store = InMemoryShares::new();
    let id = opaque(1);
    store.issue(record(&id)).await.expect("the store issues");

    let stranger = UserId::new("01937b7c-0000-7000-8000-0000000000ff");
    assert!(
        !store
            .revoke(&stranger, &id, at(1))
            .await
            .expect("the store answers"),
        "another account cannot revoke this link"
    );
    assert!(
        store
            .resolve(&id)
            .await
            .expect("the store answers")
            .expect("the link is held")
            .is_live_at(at(1)),
        "and the refused revocation changed nothing"
    );

    assert!(
        store
            .revoke(&owner(), &id, at(1))
            .await
            .expect("the store answers")
    );
    assert!(
        !store
            .revoke(&owner(), &id, at(2))
            .await
            .expect("the store answers"),
        "a second revocation is not a second event, and must not move the timestamp"
    );

    let held = store
        .resolve(&id)
        .await
        .expect("the store answers")
        .expect("the record survives revocation");
    assert_eq!(held.revoked_at, Some(at(1)));
}

#[tokio::test]
async fn resolve_returns_dead_links_too() {
    // The route collapses not-found, revoked and expired into one answer; the *store* must not,
    // or an owner's own listing could never show a link they revoked.
    let store = InMemoryShares::new();
    let id = opaque(1);
    store.issue(record(&id)).await.expect("the store issues");
    store
        .revoke(&owner(), &id, at(1))
        .await
        .expect("the store answers");

    assert!(
        store
            .resolve(&id)
            .await
            .expect("the store answers")
            .is_some()
    );
    assert!(
        store
            .resolve(&opaque(9))
            .await
            .expect("the store answers")
            .is_none()
    );
}
