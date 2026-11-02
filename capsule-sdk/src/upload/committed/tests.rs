//! The committed set and the lowest-gap-first plan.

use proptest::prelude::*;

use super::*;

const K: u64 = GRID;

#[test]
fn the_header_parses_inclusive_ranges() {
    let set = CommittedSet::parse("0-4095,8192-12287", 3 * K).expect("well formed");
    assert_eq!(set.ranges(), &[(0, K), (2 * K, 3 * K)]);
    assert_eq!(set.to_header(), "0-4095,8192-12287");
    assert_eq!(set.prefix_end(), K);
    assert_eq!(set.len(), 2 * K);
    assert!(!set.covers(3 * K));
}

#[test]
fn the_empty_header_is_the_empty_set() {
    assert_eq!(CommittedSet::parse("", 10), Some(CommittedSet::empty()));
    assert_eq!(CommittedSet::parse("  ", 10), Some(CommittedSet::empty()));
}

#[test]
fn a_range_may_end_off_the_grid_only_at_the_declared_size() {
    assert_eq!(
        CommittedSet::parse("0-99", 100).map(|s| s.ranges().to_vec()),
        Some(vec![(0, 100)])
    );
    assert_eq!(CommittedSet::parse("0-99", 200), None, "ends off the grid");
}

#[test]
fn a_malformed_header_is_refused_rather_than_trusted() {
    for header in [
        "x",
        "0-",
        "-1",
        "100-4095",         // starts off the grid
        "4096-0",           // backwards
        "0-12287",          // past the size
        "4096-8191,0-4095", // descending
        "0-8191,4096-8191", // overlapping
        "18446744073709551615-18446744073709551615",
    ] {
        assert_eq!(CommittedSet::parse(header, 2 * K), None, "{header:?}");
    }
    let too_many = (0..=MAX_FRAGMENTS as u64)
        .map(|i| format!("{}-{}", 2 * i * K, (2 * i + 1) * K - 1))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(CommittedSet::parse(&too_many, 1 << 30), None);
}

#[test]
fn adjacent_ranges_coalesce() {
    let set = CommittedSet::parse("0-4095,4096-8191", 2 * K).expect("well formed");
    assert_eq!(set.ranges(), &[(0, 2 * K)]);
    assert!(set.covers(2 * K));
}

#[test]
fn insert_coalesces_whatever_it_touches() {
    let mut set = CommittedSet::empty();
    set.insert(4 * K, 5 * K);
    set.insert(0, K);
    set.insert(2 * K, 3 * K);
    assert_eq!(set.fragments(), 3);
    set.insert(K, 4 * K);
    assert_eq!(set.ranges(), &[(0, 5 * K)]);
    set.insert(7, 7);
    assert_eq!(set.ranges(), &[(0, 5 * K)], "an empty range adds nothing");
    assert!(set.contains(K, 2 * K));
    assert!(!set.contains(4 * K, 6 * K));
}

#[test]
fn an_empty_blob_is_covered_by_nothing() {
    assert!(CommittedSet::empty().covers(0));
    assert_eq!(next_range(&CommittedSet::empty(), &[], 0, K), None);
}

#[test]
fn the_plan_fills_the_lowest_gap_first() {
    let committed = CommittedSet::parse("0-4095,12288-16383", 10 * K).expect("well formed");
    // The lowest gap is [4 KiB, 12 KiB): a 16 KiB chunk stops where the next committed range
    // starts.
    assert_eq!(next_range(&committed, &[], 10 * K, 4 * K), Some((K, 3 * K)));
    // With that gap in flight, the next one is after the committed range.
    assert_eq!(
        next_range(&committed, &[(K, 3 * K)], 10 * K, 4 * K),
        Some((4 * K, 8 * K))
    );
    // The last range ends at the declared size.
    assert_eq!(
        next_range(&committed, &[(K, 3 * K), (4 * K, 8 * K)], 10 * K - 5, 4 * K),
        Some((8 * K, 10 * K - 5))
    );
    // Nothing left to plan.
    assert_eq!(
        next_range(&committed, &[(K, 3 * K), (4 * K, 10 * K)], 10 * K, 4 * K),
        None
    );
}

#[test]
fn a_window_of_one_is_the_sequential_schedule() {
    let size = 10 * K + 123;
    let mut committed = CommittedSet::empty();
    let mut sent = Vec::new();
    while let Some((start, end)) = next_range(&committed, &[], size, 4 * K) {
        sent.push(start);
        committed.insert(start, end);
    }
    assert_eq!(sent, vec![0, 4 * K, 8 * K]);
    assert!(committed.covers(size));
}

proptest! {
    /// Whatever is committed and in flight, a planned range is on the grid, inside the declared
    /// size, at most the chunk size long, disjoint from everything committed or in flight, and
    /// starts at the lowest such byte.
    #[test]
    fn every_plan_is_on_the_grid_disjoint_and_lowest(
        blocks in 1u64..64,
        tail in 0u64..GRID,
        want_blocks in 1u64..8,
        marks in proptest::collection::vec(0u8..3, 64),
    ) {
        let size = blocks * GRID + tail;
        let want = want_blocks * GRID;
        let total_blocks = size.div_ceil(GRID);
        let mut committed = CommittedSet::empty();
        let mut in_flight = Vec::new();
        for block in 0..total_blocks {
            let range = (block * GRID, ((block + 1) * GRID).min(size));
            match marks[(block % 64) as usize] {
                1 => committed.insert(range.0, range.1),
                2 => in_flight.push(range),
                _ => {}
            }
        }
        let lowest_free = (0..total_blocks)
            .map(|b| b * GRID)
            .find(|&at| !committed.contains(at, at + 1) && !in_flight.iter().any(|&(s, e)| s <= at && at < e));
        match next_range(&committed, &in_flight, size, want) {
            None => prop_assert_eq!(lowest_free, None),
            Some((start, end)) => {
                prop_assert_eq!(Some(start), lowest_free);
                prop_assert!(start.is_multiple_of(GRID));
                prop_assert!(end.is_multiple_of(GRID) || end == size);
                prop_assert!(start < end && end <= size);
                prop_assert!(end - start <= want);
                for &(from, to) in committed.ranges().iter().chain(&in_flight) {
                    prop_assert!(end <= from || to <= start, "overlaps [{from}, {to})");
                }
            }
        }
    }

    /// The header spelling round-trips, and parsing coalesces.
    #[test]
    fn the_header_round_trips(marks in proptest::collection::vec(any::<bool>(), 1..40), tail in 1u64..GRID) {
        let size = (marks.len() as u64 - 1) * GRID + tail;
        let mut set = CommittedSet::empty();
        for (block, &held) in marks.iter().enumerate() {
            let start = block as u64 * GRID;
            if held {
                set.insert(start, (start + GRID).min(size));
            }
        }
        prop_assume!(set.fragments() <= MAX_FRAGMENTS);
        prop_assert_eq!(CommittedSet::parse(&set.to_header(), size), Some(set));
    }
}
