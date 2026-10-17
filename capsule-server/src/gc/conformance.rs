//! The one suite every [`CollectionStore`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **A mark is one fact per address.** Marking twice is one mark, and the second call says so
//!   by answering `false`; re-marking moves the instant to the later one, which delays a sweep
//!   rather than hastening it.
//! - **The instant is the one the sweep is decided on.** `marked_since` gives back what `mark`
//!   was handed, at no coarser than microsecond precision and never *later* than it — a stored
//!   instant that drifted forward would hold a blob past its window, one that drifted backward
//!   by more than the storage resolution would sweep it early.
//! - **Unmarking is exact.** It removes one address's mark, says whether there was one, and
//!   leaves every other mark alone.
//! - **The listing is in address order.** `marks` walks addresses by their bytes, which is the
//!   order the blob store enumerates in and the order a collection report reads in.
//!
//! # Reusing a harness
//!
//! Every case scopes its own addresses, so cases may share one store and [`run_all`] does. The
//! listing case therefore asserts on its own addresses' relative order, not on the whole list.

use capsule_core::crypto::hash::hash_bytes;
use jiff::{SignedDuration, Timestamp};

use super::CollectionStore;
use crate::blob::ContentAddress;
use crate::store::StoreError;

/// The store under test.
pub trait Harness: Send + Sync {
    /// The mark store under test.
    fn marks(&self) -> &dyn CollectionStore;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming collection store must succeed at {doing}: {error}"),
    }
}

/// `case`'s blob `name`, as a content address no other case can produce.
fn address(case: &str, name: &str) -> ContentAddress {
    ContentAddress::parse(&hash_bytes(format!("{case}/{name}").as_bytes()).to_hex())
        .expect("a digest is an address")
}

/// A whole-microsecond instant `secs` seconds after a fixed origin.
fn at(secs: i64) -> Timestamp {
    Timestamp::UNIX_EPOCH + SignedDuration::from_secs(1_700_000_000 + secs)
}

/// `address`'s mark, if any.
async fn since(h: &dyn Harness, address: &ContentAddress) -> Option<Timestamp> {
    ok(h.marks().marked_since(address).await, "read a mark")
}

// ===========================================================================================
// Marking
// ===========================================================================================

/// An address nobody marked is not marked, and does not appear in the listing.
pub async fn an_unmarked_address_has_no_mark(h: &dyn Harness) {
    let blob = address("unmarked", "a");
    assert_eq!(since(h, &blob).await, None);
    let listed = ok(h.marks().marks().await, "list the marks");
    assert!(
        listed.iter().all(|(listed, _)| listed != &blob),
        "an address nobody marked must not be listed"
    );
}

/// The first mark is recorded, answers `true`, and reads back with its instant.
pub async fn the_first_mark_is_recorded_with_its_instant(h: &dyn Harness) {
    let blob = address("first", "a");
    assert!(
        ok(h.marks().mark(&blob, at(10)).await, "mark a blob"),
        "the first mark of an address must answer true"
    );
    assert_eq!(since(h, &blob).await, Some(at(10)));
    let listed = ok(h.marks().marks().await, "list the marks");
    assert!(
        listed.contains(&(blob, at(10))),
        "a marked address must be listed with its instant"
    );
}

/// Marking an already-marked address answers `false`, stays one mark, and re-stamps it.
pub async fn marking_twice_is_one_mark_restamped(h: &dyn Harness) {
    let blob = address("twice", "a");
    assert!(ok(h.marks().mark(&blob, at(10)).await, "mark a blob"));
    assert!(
        !ok(h.marks().mark(&blob, at(20)).await, "mark it again"),
        "a second mark of the same address must answer false"
    );
    assert_eq!(
        since(h, &blob).await,
        Some(at(20)),
        "a re-mark moves the instant to the later one, which delays the sweep"
    );
    let listed = ok(h.marks().marks().await, "list the marks");
    assert_eq!(
        listed.iter().filter(|(listed, _)| listed == &blob).count(),
        1,
        "two marks of one address are one listed mark"
    );
}

/// An instant comes back at microsecond precision or finer, and never later than it went in.
///
/// An adapter whose column is coarser than the instant may truncate; it may not round up, and
/// it may not lose more than the column's resolution.
pub async fn the_mark_instant_round_trips_to_the_microsecond(h: &dyn Harness) {
    let blob = address("precise", "a");
    let precise = Timestamp::from_nanosecond(1_700_000_000_123_456_789).expect("an instant");
    assert!(ok(h.marks().mark(&blob, precise).await, "mark a blob"));
    let held = since(h, &blob).await.expect("the mark is held");
    assert!(
        held <= precise,
        "a stored mark must never move later: {held} > {precise}"
    );
    assert!(
        precise.duration_since(held) < SignedDuration::from_micros(1),
        "a stored mark may lose at most sub-microsecond precision: {held} vs {precise}"
    );
}

// ===========================================================================================
// Unmarking
// ===========================================================================================

/// Unmarking removes the mark and answers `true`; unmarking again answers `false`.
pub async fn unmarking_removes_the_mark_once(h: &dyn Harness) {
    let blob = address("unmark", "a");
    assert!(ok(h.marks().mark(&blob, at(10)).await, "mark a blob"));
    assert!(
        ok(h.marks().unmark(&blob).await, "unmark it"),
        "unmarking a marked address must answer true"
    );
    assert_eq!(since(h, &blob).await, None);
    assert!(
        !ok(h.marks().unmark(&blob).await, "unmark it again"),
        "unmarking an unmarked address must answer false"
    );
    let listed = ok(h.marks().marks().await, "list the marks");
    assert!(listed.iter().all(|(listed, _)| listed != &blob));
}

/// Unmarking an address nobody marked answers `false` and changes nothing.
pub async fn unmarking_an_unknown_address_changes_nothing(h: &dyn Harness) {
    let kept = address("unknown", "kept");
    let never = address("unknown", "never");
    assert!(ok(h.marks().mark(&kept, at(10)).await, "mark a blob"));
    assert!(!ok(
        h.marks().unmark(&never).await,
        "unmark an unknown address"
    ));
    assert_eq!(since(h, &kept).await, Some(at(10)));
}

/// A blob that is unmarked and later marked again carries the new instant.
pub async fn a_mark_after_an_unmark_is_a_fresh_mark(h: &dyn Harness) {
    let blob = address("remark", "a");
    assert!(ok(h.marks().mark(&blob, at(10)).await, "mark a blob"));
    assert!(ok(h.marks().unmark(&blob).await, "unmark it"));
    assert!(
        ok(h.marks().mark(&blob, at(30)).await, "mark it afresh"),
        "a mark after an unmark is a first mark again"
    );
    assert_eq!(since(h, &blob).await, Some(at(30)));
}

/// One address's mark never leaks into another's.
pub async fn marks_do_not_leak_between_addresses(h: &dyn Harness) {
    let one = address("isolated", "one");
    let two = address("isolated", "two");
    assert!(ok(h.marks().mark(&one, at(10)).await, "mark one"));
    assert_eq!(
        since(h, &two).await,
        None,
        "marking one address marks no other"
    );
    assert!(ok(h.marks().mark(&two, at(20)).await, "mark two"));
    assert!(ok(h.marks().unmark(&one).await, "unmark one"));
    assert_eq!(since(h, &one).await, None);
    assert_eq!(
        since(h, &two).await,
        Some(at(20)),
        "unmarking one address leaves another's mark exactly as it was"
    );
}

// ===========================================================================================
// Listing
// ===========================================================================================

/// The listing walks addresses in their byte order, each with its own instant.
pub async fn the_listing_is_in_address_order(h: &dyn Harness) {
    let mut mine: Vec<ContentAddress> =
        (0..6).map(|n| address("ordered", &n.to_string())).collect();
    // Marked in reverse of the order they will be listed in, so an adapter that returned rows
    // in insertion order would fail.
    mine.sort();
    for (offset, blob) in mine.iter().rev().enumerate() {
        let offset = i64::try_from(offset).expect("small");
        assert!(ok(h.marks().mark(blob, at(offset)).await, "mark a blob"));
    }
    let listed = ok(h.marks().marks().await, "list the marks");
    let all: Vec<&ContentAddress> = listed.iter().map(|(address, _)| address).collect();
    let mut sorted = all.clone();
    sorted.sort();
    assert_eq!(all, sorted, "the whole listing must be in address order");
    let ours: Vec<&ContentAddress> = all.into_iter().filter(|a| mine.contains(a)).collect();
    assert_eq!(ours, mine.iter().collect::<Vec<_>>());
    for (offset, blob) in mine.iter().rev().enumerate() {
        let offset = i64::try_from(offset).expect("small");
        assert!(
            listed.contains(&(blob.clone(), at(offset))),
            "each listed address carries its own instant"
        );
    }
}

// ===========================================================================================
// The whole suite
// ===========================================================================================

/// Run every case above against one harness, in order.
pub async fn run_all(h: &dyn Harness) {
    an_unmarked_address_has_no_mark(h).await;
    the_first_mark_is_recorded_with_its_instant(h).await;
    marking_twice_is_one_mark_restamped(h).await;
    the_mark_instant_round_trips_to_the_microsecond(h).await;
    unmarking_removes_the_mark_once(h).await;
    unmarking_an_unknown_address_changes_nothing(h).await;
    a_mark_after_an_unmark_is_a_fresh_mark(h).await;
    marks_do_not_leak_between_addresses(h).await;
    the_listing_is_in_address_order(h).await;
}

#[cfg(test)]
mod tests {
    use super::{Harness, run_all};
    use crate::gc::CollectionStore;
    use crate::gc::memory::InMemoryCollection;

    /// The deterministic store.
    #[derive(Debug, Default)]
    struct MemoryHarness {
        marks: InMemoryCollection,
    }

    impl Harness for MemoryHarness {
        fn marks(&self) -> &dyn CollectionStore {
            &self.marks
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
        an_unmarked_address_has_no_mark,
        the_first_mark_is_recorded_with_its_instant,
        marking_twice_is_one_mark_restamped,
        the_mark_instant_round_trips_to_the_microsecond,
        unmarking_removes_the_mark_once,
        unmarking_an_unknown_address_changes_nothing,
        a_mark_after_an_unmark_is_a_fresh_mark,
        marks_do_not_leak_between_addresses,
        the_listing_is_in_address_order,
    }

    /// The whole suite, in one pass on one store — the entry point the container case uses.
    #[tokio::test]
    async fn the_in_memory_store_conforms() {
        run_all(&MemoryHarness::default()).await;
    }
}
