//! The one suite every [`EscrowStore`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **One escrow per account.** A second store replaces the first and reports
//!   [`Replaced::Yes`]; the displaced blob is gone, not retained beside the new one.
//! - **The bytes are opaque.** What is fetched is exactly what was stored, byte for byte,
//!   whatever it contains — including bytes no wrap format would produce.
//! - **Accounts do not see each other.** Storing or replacing one account's escrow leaves every
//!   other account's untouched.
//! - **The instant reads back as the instant.** `stored_at` is what a client compares its cached
//!   copy against, so an adapter may drop sub-microsecond precision but must never move it.
//!
//! The atomicity of the replace — no window with zero escrows, none with two — is structural
//! (one mutex in memory, one upsert in Postgres); a single-process suite cannot exhibit the race,
//! so it asserts the consequence: after any sequence of stores exactly one blob is held.
//!
//! # Reusing a harness
//!
//! Every case scopes its own account ids, so cases may share one store and [`run_all`] does.

use jiff::{SignedDuration, Timestamp};

use super::{EscrowRecord, EscrowStore, Replaced};
use crate::store::{StoreError, UserId};

/// The store under test.
pub trait Harness: Send + Sync {
    /// The escrow store under test.
    fn escrows(&self) -> &dyn EscrowStore;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming escrow store must succeed at {doing}: {error}"),
    }
}

/// `case`'s account `name`.
fn user(case: &str, name: &str) -> UserId {
    UserId::new(format!("{case}-{name}"))
}

/// A whole-second instant `secs` after a fixed base, so equality is exact on every adapter.
fn at(secs: i64) -> Timestamp {
    Timestamp::UNIX_EPOCH + SignedDuration::from_secs(1_700_000_000 + secs)
}

/// An escrow for `user` holding `blob`, stored at `at`.
fn escrow(user: &UserId, blob: &[u8], at: Timestamp) -> EscrowRecord {
    EscrowRecord {
        user_id: user.clone(),
        blob: blob.to_vec(),
        stored_at: at,
    }
}

/// Store `record`, expecting it to work.
async fn store(h: &dyn Harness, record: EscrowRecord) -> Replaced {
    ok(h.escrows().store(record).await, "store an escrow")
}

/// What is held for `user`.
async fn fetch(h: &dyn Harness, user: &UserId) -> Option<EscrowRecord> {
    ok(h.escrows().fetch(user).await, "fetch an escrow")
}

// ===========================================================================================
// Cases
// ===========================================================================================

/// An account with nothing stored fetches `None`.
pub async fn an_account_with_no_escrow_fetches_none(h: &dyn Harness) {
    assert_eq!(fetch(h, &user("none", "alice")).await, None);
}

/// The first store reports nothing replaced and the record reads back whole.
pub async fn the_first_escrow_is_not_a_replacement_and_reads_back(h: &dyn Harness) {
    let alice = user("first", "alice");
    let record = escrow(&alice, b"wrapped master key", at(1));
    assert_eq!(store(h, record.clone()).await, Replaced::No);
    assert_eq!(fetch(h, &alice).await, Some(record));
}

/// A second store replaces the first: `Replaced::Yes`, and only the new blob is held.
pub async fn a_second_escrow_replaces_the_first_and_the_old_one_is_gone(h: &dyn Harness) {
    let alice = user("replace", "alice");
    store(h, escrow(&alice, b"the lost secret's wrap", at(1))).await;
    let rotated = escrow(&alice, b"the fresh secret's wrap", at(2));
    assert_eq!(store(h, rotated.clone()).await, Replaced::Yes);
    assert_eq!(
        fetch(h, &alice).await,
        Some(rotated),
        "after a rotation the previous wrap must unwrap nothing this store holds"
    );
}

/// Storing the same bytes again is still a replacement, and every later store is one too.
pub async fn every_store_after_the_first_is_a_replacement(h: &dyn Harness) {
    let alice = user("again", "alice");
    assert_eq!(store(h, escrow(&alice, b"same", at(1))).await, Replaced::No);
    assert_eq!(
        store(h, escrow(&alice, b"same", at(2))).await,
        Replaced::Yes
    );
    let last = escrow(&alice, b"other", at(3));
    assert_eq!(store(h, last.clone()).await, Replaced::Yes);
    assert_eq!(fetch(h, &alice).await, Some(last));
}

/// The blob is opaque: arbitrary bytes, including NULs and the ceiling's worth, round-trip.
pub async fn the_blob_is_returned_byte_for_byte(h: &dyn Harness) {
    let alice = user("opaque", "alice");
    let mut noise: Vec<u8> = (0..=255_u8).collect();
    noise.extend([0, 0, 0xff, 0]);
    let record = escrow(&alice, &noise, at(1));
    store(h, record.clone()).await;
    assert_eq!(fetch(h, &alice).await, Some(record));

    let bob = user("opaque", "bob");
    let largest = escrow(&bob, &vec![0xa5; super::MAX_ESCROW_BYTES], at(2));
    store(h, largest.clone()).await;
    assert_eq!(fetch(h, &bob).await, Some(largest));
}

/// One account's escrow, stored or replaced, never touches another's.
pub async fn escrows_do_not_leak_between_accounts(h: &dyn Harness) {
    let alice = user("isolated", "alice");
    let bob = user("isolated", "bob");
    let bobs = escrow(&bob, b"bob's wrap", at(1));
    store(h, bobs.clone()).await;
    assert_eq!(
        store(h, escrow(&alice, b"alice's wrap", at(2))).await,
        Replaced::No
    );
    store(h, escrow(&alice, b"alice's rotated wrap", at(3))).await;
    assert_eq!(fetch(h, &bob).await, Some(bobs));
    assert_eq!(fetch(h, &user("isolated", "carol")).await, None);
}

/// `stored_at` reads back unmoved: never later than written, and within a microsecond of it.
///
/// An adapter that keeps the instant at a coarser grain than its writer would hand a client a
/// `stored_at` that disagrees with the one it cached, which manufactures exactly the false
/// stale-cache verdict the field exists to prevent.
pub async fn the_stored_instant_reads_back_at_microsecond_precision(h: &dyn Harness) {
    let alice = user("precise", "alice");
    let written = Timestamp::from_nanosecond(1_700_000_000_123_456_789).expect("an instant");
    store(h, escrow(&alice, b"wrap", written)).await;
    let held = fetch(h, &alice).await.expect("held").stored_at;
    assert!(held <= written, "{held} is later than {written}");
    assert!(
        written.duration_since(held) < SignedDuration::from_micros(1),
        "{held} lost more than sub-microsecond precision from {written}"
    );
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order.
pub async fn run_all(h: &dyn Harness) {
    an_account_with_no_escrow_fetches_none(h).await;
    the_first_escrow_is_not_a_replacement_and_reads_back(h).await;
    a_second_escrow_replaces_the_first_and_the_old_one_is_gone(h).await;
    every_store_after_the_first_is_a_replacement(h).await;
    the_blob_is_returned_byte_for_byte(h).await;
    escrows_do_not_leak_between_accounts(h).await;
    the_stored_instant_reads_back_at_microsecond_precision(h).await;
}

#[cfg(test)]
mod tests {
    use super::{Harness, run_all};
    use crate::escrow::{EscrowStore, InMemoryEscrow};

    /// The deterministic store.
    #[derive(Debug, Default)]
    struct MemoryHarness {
        escrows: InMemoryEscrow,
    }

    impl Harness for MemoryHarness {
        fn escrows(&self) -> &dyn EscrowStore {
            &self.escrows
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
        an_account_with_no_escrow_fetches_none,
        the_first_escrow_is_not_a_replacement_and_reads_back,
        a_second_escrow_replaces_the_first_and_the_old_one_is_gone,
        every_store_after_the_first_is_a_replacement,
        the_blob_is_returned_byte_for_byte,
        escrows_do_not_leak_between_accounts,
        the_stored_instant_reads_back_at_microsecond_precision,
    }

    /// The whole suite, in one pass on one store — the entry point the container case uses.
    #[tokio::test]
    async fn the_in_memory_store_conforms() {
        run_all(&MemoryHarness::default()).await;
    }
}
