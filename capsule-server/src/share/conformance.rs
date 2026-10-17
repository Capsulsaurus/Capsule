//! The one suite every [`ShareStore`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **A link serves only what it enumerates.** The serve set reads back exactly as issued — no
//!   address added, none dropped — because that set is the enforceable half of the privacy
//!   strip.
//! - **Resolve does not pre-collapse.** A revoked or expired link still resolves, with its state
//!   on the record; collapsing the refusals into one answer is the route's job, and the owner's
//!   listing needs the revoked link.
//! - **Revocation is the owner's, once.** Only the owner revokes; a second revocation, another
//!   account's, or one of an unknown id changes nothing and answers `false`.
//! - **The opaque bytes are opaque.** A wrapped secret reads back byte for byte.
//! - **Instants are not moved.** `expires_at` and `revoked_at` may lose sub-microsecond
//!   precision and nothing more — an expiry that moved later is a link live past its owner's
//!   choice.
//!
//! # Reusing a harness
//!
//! Every case scopes its own opaque ids, so cases may share one store and [`run_all`] does.

use std::collections::BTreeSet;

use capsule_core::sharing::ShareScope;
use jiff::{SignedDuration, Timestamp};
use uuid::Uuid;

use super::{ShareRecord, ShareStore};
use crate::blob::ContentAddress;
use crate::store::{StoreError, UserId};

/// The store under test.
pub trait Harness: Send + Sync {
    /// The share store under test.
    fn shares(&self) -> &dyn ShareStore;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming share store must succeed at {doing}: {error}"),
    }
}

/// A 32-hex opaque id unique to `case` and `n`.
fn opaque(case: &str, n: u8) -> String {
    let digest = capsule_core::crypto::hash::hash_bytes(format!("{case}/{n}").as_bytes());
    digest.to_hex()[..super::OPAQUE_ID_HEX_LEN].to_owned()
}

/// A content address derived from `seed`.
fn address(seed: &str) -> ContentAddress {
    ContentAddress::parse(&capsule_core::crypto::hash::hash_bytes(seed.as_bytes()).to_hex())
        .expect("a BLAKE3 digest is an address")
}

/// `case`'s account `name`.
fn user(case: &str, name: &str) -> UserId {
    UserId::new(format!("{case}-{name}"))
}

/// A whole-second instant `secs` after a fixed base, so equality is exact on every adapter.
fn at(secs: i64) -> Timestamp {
    Timestamp::UNIX_EPOCH + SignedDuration::from_secs(1_700_000_000 + secs)
}

/// A fully-populated link for `case` owned by `owner`.
fn link(case: &str, n: u8, owner: &UserId) -> ShareRecord {
    let metadata = address(&format!("{case}/{n}/metadata"));
    let serves: BTreeSet<ContentAddress> = [
        metadata.clone(),
        address(&format!("{case}/{n}/original")),
        address(&format!("{case}/{n}/thumbnail")),
    ]
    .into_iter()
    .collect();
    ShareRecord {
        opaque_id: opaque(case, n),
        owner_id: owner.clone(),
        scope: ShareScope::Album(Uuid::from_u128(0xA1B2_0000 + u128::from(n))),
        serves,
        metadata,
        wrapped_secret: Some(vec![0, 1, 2, 0xff, 0]),
        expires_at: Some(at(3_600)),
        revoked_at: None,
    }
}

/// Issue `record`, expecting it to work.
async fn issue(h: &dyn Harness, record: ShareRecord) {
    ok(h.shares().issue(record).await, "issue a share link");
}

/// What `opaque_id` resolves to.
async fn resolve(h: &dyn Harness, opaque_id: &str) -> Option<ShareRecord> {
    ok(h.shares().resolve(opaque_id).await, "resolve a share link")
}

/// Revoke `opaque_id` as `owner` at `when`.
async fn revoke(h: &dyn Harness, owner: &UserId, opaque_id: &str, when: Timestamp) -> bool {
    ok(
        h.shares().revoke(owner, opaque_id, when).await,
        "revoke a share link",
    )
}

// ===========================================================================================
// Issuing and resolving
// ===========================================================================================

/// An id nothing issued resolves to `None`.
pub async fn an_unknown_link_resolves_to_none(h: &dyn Harness) {
    assert_eq!(resolve(h, &opaque("unknown", 0)).await, None);
}

/// An issued link resolves to the record that was issued, every field included.
pub async fn an_issued_link_resolves_to_its_whole_record(h: &dyn Harness) {
    let case = "whole";
    let record = link(case, 1, &user(case, "alice"));
    issue(h, record.clone()).await;
    assert_eq!(resolve(h, &record.opaque_id).await, Some(record));
}

/// A link with no secret and no expiry reads back with neither, and an asset scope survives.
pub async fn the_optional_fields_read_back_absent(h: &dyn Harness) {
    let case = "bare";
    let mut record = link(case, 1, &user(case, "alice"));
    record.wrapped_secret = None;
    record.expires_at = None;
    record.scope = ShareScope::Asset(Uuid::from_u128(0xF00D));
    issue(h, record.clone()).await;
    assert_eq!(resolve(h, &record.opaque_id).await, Some(record));
}

/// The serve set is exactly the enumerated one: one address, or many, never another link's.
pub async fn a_link_serves_exactly_what_it_enumerates(h: &dyn Harness) {
    let case = "serves";
    let alice = user(case, "alice");
    let mut only_metadata = link(case, 1, &alice);
    only_metadata.serves = std::iter::once(only_metadata.metadata.clone()).collect();
    let wide = link(case, 2, &alice);
    issue(h, only_metadata.clone()).await;
    issue(h, wide.clone()).await;

    let narrow = resolve(h, &only_metadata.opaque_id).await.expect("issued");
    assert_eq!(narrow.serves, only_metadata.serves);
    for other in &wide.serves {
        assert!(
            !narrow.serves(other),
            "a link must never serve an address another link enumerates"
        );
    }
    assert_eq!(
        resolve(h, &wide.opaque_id).await.expect("issued").serves,
        wide.serves
    );
}

/// A wrapped secret is handed back byte for byte, whatever it holds.
pub async fn the_wrapped_secret_is_returned_byte_for_byte(h: &dyn Harness) {
    let case = "secret";
    let mut record = link(case, 1, &user(case, "alice"));
    let mut noise: Vec<u8> = (0..=255_u8).collect();
    noise.extend([0, 0]);
    record.wrapped_secret = Some(noise);
    issue(h, record.clone()).await;
    assert_eq!(
        resolve(h, &record.opaque_id)
            .await
            .expect("issued")
            .wrapped_secret,
        record.wrapped_secret
    );
}

/// Instants lose at most sub-microsecond precision and are never moved later.
pub async fn instants_read_back_at_microsecond_precision(h: &dyn Harness) {
    let case = "precise";
    let alice = user(case, "alice");
    let mut record = link(case, 1, &alice);
    let expiry = Timestamp::from_nanosecond(1_700_003_600_123_456_789).expect("an instant");
    record.expires_at = Some(expiry);
    issue(h, record.clone()).await;
    let revoked = Timestamp::from_nanosecond(1_700_000_010_987_654_321).expect("an instant");
    assert!(revoke(h, &alice, &record.opaque_id, revoked).await);

    let held = resolve(h, &record.opaque_id).await.expect("issued");
    for (written, read) in [(expiry, held.expires_at), (revoked, held.revoked_at)] {
        let read = read.expect("present");
        assert!(read <= written, "{read} is later than {written}");
        assert!(
            written.duration_since(read) < SignedDuration::from_micros(1),
            "{read} lost more than sub-microsecond precision from {written}"
        );
    }
}

// ===========================================================================================
// Revoking
// ===========================================================================================

/// The owner revokes once: `true`, and the link still resolves, carrying when.
pub async fn the_owner_revokes_and_the_link_still_resolves_revoked(h: &dyn Harness) {
    let case = "revoke";
    let alice = user(case, "alice");
    let record = link(case, 1, &alice);
    issue(h, record.clone()).await;
    assert!(revoke(h, &alice, &record.opaque_id, at(10)).await);

    let held = resolve(h, &record.opaque_id)
        .await
        .expect("a revoked link still resolves");
    assert_eq!(held.revoked_at, Some(at(10)));
    assert!(!held.is_live_at(at(11)));
    assert_eq!(
        ShareRecord {
            revoked_at: None,
            ..held
        },
        record,
        "revocation changes nothing but `revoked_at`"
    );
}

/// A second revocation answers `false` and keeps the first instant.
pub async fn a_second_revocation_changes_nothing(h: &dyn Harness) {
    let case = "twice";
    let alice = user(case, "alice");
    let record = link(case, 1, &alice);
    issue(h, record.clone()).await;
    assert!(revoke(h, &alice, &record.opaque_id, at(10)).await);
    assert!(!revoke(h, &alice, &record.opaque_id, at(20)).await);
    assert_eq!(
        resolve(h, &record.opaque_id)
            .await
            .expect("issued")
            .revoked_at,
        Some(at(10))
    );
}

/// Another account cannot revoke a link, and an unknown id revokes nothing.
pub async fn only_the_owner_can_revoke(h: &dyn Harness) {
    let case = "stranger";
    let alice = user(case, "alice");
    let mallory = user(case, "mallory");
    let record = link(case, 1, &alice);
    issue(h, record.clone()).await;
    assert!(!revoke(h, &mallory, &record.opaque_id, at(10)).await);
    assert_eq!(resolve(h, &record.opaque_id).await, Some(record));
    assert!(!revoke(h, &alice, &opaque(case, 9), at(10)).await);
    assert_eq!(resolve(h, &opaque(case, 9)).await, None);
}

/// Revoking one link leaves the owner's other links live.
pub async fn revocation_does_not_leak_between_links(h: &dyn Harness) {
    let case = "isolated";
    let alice = user(case, "alice");
    let first = link(case, 1, &alice);
    let second = link(case, 2, &alice);
    issue(h, first.clone()).await;
    issue(h, second.clone()).await;
    assert!(revoke(h, &alice, &first.opaque_id, at(10)).await);
    assert_eq!(resolve(h, &second.opaque_id).await, Some(second));
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order.
pub async fn run_all(h: &dyn Harness) {
    an_unknown_link_resolves_to_none(h).await;
    an_issued_link_resolves_to_its_whole_record(h).await;
    the_optional_fields_read_back_absent(h).await;
    a_link_serves_exactly_what_it_enumerates(h).await;
    the_wrapped_secret_is_returned_byte_for_byte(h).await;
    instants_read_back_at_microsecond_precision(h).await;

    the_owner_revokes_and_the_link_still_resolves_revoked(h).await;
    a_second_revocation_changes_nothing(h).await;
    only_the_owner_can_revoke(h).await;
    revocation_does_not_leak_between_links(h).await;
}

#[cfg(test)]
mod tests {
    use super::{Harness, run_all};
    use crate::share::{InMemoryShares, ShareStore};

    /// The deterministic store.
    #[derive(Debug, Default)]
    struct MemoryHarness {
        shares: InMemoryShares,
    }

    impl Harness for MemoryHarness {
        fn shares(&self) -> &dyn ShareStore {
            &self.shares
        }
    }

    /// Declares one `#[tokio::test]` per conformance case, on a fresh store each.
    macro_rules! conformance_cases {
        ($($case:ident),+ $(,)?) => {
            $(
                #[tokio::test]
                async fn $case() {
                    super::$case(&MemoryHarness::default()).await;
                }
            )+
        };
    }

    conformance_cases! {
        an_unknown_link_resolves_to_none,
        an_issued_link_resolves_to_its_whole_record,
        the_optional_fields_read_back_absent,
        a_link_serves_exactly_what_it_enumerates,
        the_wrapped_secret_is_returned_byte_for_byte,
        instants_read_back_at_microsecond_precision,
        the_owner_revokes_and_the_link_still_resolves_revoked,
        a_second_revocation_changes_nothing,
        only_the_owner_can_revoke,
        revocation_does_not_leak_between_links,
    }

    /// The whole suite, in one pass on one store — the entry point the container case uses.
    #[tokio::test]
    async fn the_in_memory_store_conforms() {
        run_all(&MemoryHarness::default()).await;
    }
}
