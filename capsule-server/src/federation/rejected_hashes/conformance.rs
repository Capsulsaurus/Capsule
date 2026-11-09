//! The one suite every [`RejectedHashes`] adapter must pass.
//!
//! # The rules the suite exists to protect
//!
//! - **A refusal reads back as recorded**, and presenting it again is a reference: its last
//!   reference moves forward, never back, its count grows, and its first attribution stays.
//! - **The table evicts at its cap** (design/federation.md, testing: "the rejected-hash table
//!   evicts at its cap"): past [`BOUNDS`]' capacity the row referenced longest ago goes first,
//!   ties broken by the smaller hash, and a lookup is no reference.
//! - **A row is forgotten at its age**: a lookup at or past its last reference plus the TTL
//!   finds nothing, and presenting a forgotten hash again remembers it afresh.
//!
//! # Every case starts empty
//!
//! The table is one bounded set, so a case that shared it would count another case's rows
//! against the cap. [`Harness::empty`] hands each case an empty table bounded by [`BOUNDS`].

use std::num::NonZeroU32;

use capsule_core::crypto::hash::Hash32;
use jiff::{SignedDuration, Timestamp};

use super::{RejectedHash, RejectedHashBounds, RejectedHashes, Rejection};
use crate::federation::PeerId;
use crate::store::{AlbumId, StoreError, StoreFuture};

/// The bounds every case's table is assembled with: three rows, kept a day.
pub const BOUNDS: RejectedHashBounds = RejectedHashBounds::new(
    match NonZeroU32::new(3) {
        Some(capacity) => capacity,
        None => NonZeroU32::MIN,
    },
    NonZeroU32::MIN,
);

/// What the suite is driven through.
pub trait Harness: Sync {
    /// An empty table bounded by [`BOUNDS`].
    fn empty(&self) -> StoreFuture<'_, Box<dyn RejectedHashes>>;
}

/// Unwrap a store result, failing with the operation that was expected to work.
#[track_caller]
fn ok<T>(result: Result<T, StoreError>, doing: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("a conforming rejected-hash table must succeed at {doing}: {error}"),
    }
}

/// The hash numbered `n`.
fn hash(n: u8) -> Hash32 {
    Hash32([n; 32])
}

/// A refusal of `hash(n)` presented by `peer`.
fn rejection(n: u8, peer: &str) -> Rejection {
    Rejection {
        hash: hash(n),
        peer: PeerId::new(peer),
        album: AlbumId::new("0190a5b0-0000-7000-8000-000000000002"),
        asset_id: format!("asset-{n}"),
        rule: "invariant 17".to_owned(),
    }
}

/// The suite's epoch, plus `seconds`.
fn at(seconds: i64) -> Timestamp {
    let start: Timestamp = "2026-06-01T00:00:00Z".parse().expect("an instant");
    start + SignedDuration::from_secs(seconds)
}

async fn record(table: &dyn RejectedHashes, n: u8, seconds: i64) {
    ok(
        table.record(rejection(n, "home.test"), at(seconds)).await,
        "recording",
    );
}

async fn held(table: &dyn RejectedHashes, n: u8, seconds: i64) -> Option<RejectedHash> {
    ok(table.lookup(&hash(n), at(seconds)).await, "looking up")
}

/// Which of `hash(1..=last)` the table holds at `seconds`.
async fn holding(table: &dyn RejectedHashes, last: u8, seconds: i64) -> Vec<u8> {
    let mut holding = Vec::new();
    for n in 1..=last {
        if held(table, n, seconds).await.is_some() {
            holding.push(n);
        }
    }
    holding
}

/// A recorded refusal reads back, and nothing else does.
pub async fn a_refusal_reads_back_as_recorded(h: &dyn Harness) {
    let table = h.empty().await.expect("an empty table");
    assert_eq!(held(table.as_ref(), 1, 0).await, None, "an empty table");
    record(table.as_ref(), 1, 10).await;
    assert_eq!(
        held(table.as_ref(), 1, 20).await,
        Some(RejectedHash {
            rejection: rejection(1, "home.test"),
            first_rejected_at: at(10),
            last_referenced_at: at(10),
            references: 1,
        })
    );
    assert_eq!(held(table.as_ref(), 2, 20).await, None);
}

/// Presenting a refused hash again references it and keeps its first attribution.
pub async fn presenting_it_again_is_a_reference(h: &dyn Harness) {
    let table = h.empty().await.expect("an empty table");
    record(table.as_ref(), 1, 10).await;
    ok(
        table.record(rejection(1, "other.test"), at(30)).await,
        "recording again",
    );
    let row = held(table.as_ref(), 1, 40).await.expect("held");
    assert_eq!(
        row.rejection.peer,
        PeerId::new("home.test"),
        "the first attribution stays"
    );
    assert_eq!(row.first_rejected_at, at(10));
    assert_eq!(row.last_referenced_at, at(30));
    assert_eq!(row.references, 2);

    // A presentation timed before the last reference counts, and moves nothing back.
    record(table.as_ref(), 1, 20).await;
    let row = held(table.as_ref(), 1, 40).await.expect("held");
    assert_eq!(
        row.last_referenced_at,
        at(30),
        "a reference never moves backwards"
    );
    assert_eq!(row.references, 3);
}

/// Past capacity the row referenced longest ago is evicted, and a lookup is no reference.
pub async fn the_table_evicts_at_its_cap(h: &dyn Harness) {
    let table = h.empty().await.expect("an empty table");
    for (n, seconds) in [(1, 1), (2, 2), (3, 3)] {
        record(table.as_ref(), n, seconds).await;
    }
    assert_eq!(
        holding(table.as_ref(), 4, 4).await,
        vec![1, 2, 3],
        "at capacity"
    );
    // A read of the oldest does not keep it.
    assert!(held(table.as_ref(), 1, 4).await.is_some());
    record(table.as_ref(), 4, 5).await;
    assert_eq!(
        holding(table.as_ref(), 4, 6).await,
        vec![2, 3, 4],
        "the row referenced longest ago is evicted"
    );

    // A reference moves a row to the back of the order.
    record(table.as_ref(), 2, 7).await;
    record(table.as_ref(), 5, 8).await;
    assert_eq!(
        holding(table.as_ref(), 5, 9).await,
        vec![2, 4, 5],
        "the referenced row outlives one recorded after it"
    );
}

/// Rows referenced at one instant are evicted smaller hash first.
pub async fn a_tie_evicts_the_smaller_hash(h: &dyn Harness) {
    let table = h.empty().await.expect("an empty table");
    for n in [3, 1, 2] {
        record(table.as_ref(), n, 1).await;
    }
    record(table.as_ref(), 4, 2).await;
    assert_eq!(holding(table.as_ref(), 4, 3).await, vec![2, 3, 4]);
}

/// A row is forgotten once its TTL has passed since its last reference.
pub async fn a_row_is_forgotten_at_its_age(h: &dyn Harness) {
    let table = h.empty().await.expect("an empty table");
    let day = BOUNDS.ttl().as_secs();
    record(table.as_ref(), 1, 0).await;
    record(table.as_ref(), 2, 10).await;
    assert!(
        held(table.as_ref(), 1, day - 1).await.is_some(),
        "kept until its age"
    );
    assert_eq!(
        held(table.as_ref(), 1, day).await,
        None,
        "forgotten at its age"
    );

    // A forgotten row is no reference: presenting it again remembers it afresh.
    record(table.as_ref(), 1, day + 5).await;
    let row = held(table.as_ref(), 1, day + 6).await.expect("held");
    assert_eq!(row.first_rejected_at, at(day + 5));
    assert_eq!(row.references, 1);

    // Hash 2 ages out beside the rows recorded after it, which are kept.
    record(table.as_ref(), 3, day + 11).await;
    record(table.as_ref(), 4, day + 12).await;
    assert_eq!(holding(table.as_ref(), 4, day + 13).await, vec![1, 3, 4]);
}

/// Every case.
pub async fn run_all(h: &dyn Harness) {
    a_refusal_reads_back_as_recorded(h).await;
    presenting_it_again_is_a_reference(h).await;
    the_table_evicts_at_its_cap(h).await;
    a_tie_evicts_the_smaller_hash(h).await;
    a_row_is_forgotten_at_its_age(h).await;
}
