use super::{BlobRole, CommittedSet};

#[test]
fn every_role_reads_back_from_its_own_token() {
    // The adapters store the token and parse it back through `from_token`, so a role missing
    // from `ALL` is a role every stored record of it fails to decode.
    for role in BlobRole::ALL {
        assert_eq!(BlobRole::from_token(role.as_str()), Some(role));
    }
    assert_eq!(BlobRole::DerivativeManifest.as_str(), "derivative_manifest");
    assert_eq!(BlobRole::from_token("derivativemanifest"), None);
}

fn set(ranges: &[(u64, u64)]) -> CommittedSet {
    ranges
        .iter()
        .fold(CommittedSet::default(), |set, &(a, b)| set.with_range(a, b))
}

#[test]
fn committed_ranges_coalesce_when_they_touch_or_overlap() {
    assert_eq!(set(&[(0, 4096), (4096, 8192)]).ranges(), &[(0, 8192)]);
    assert_eq!(set(&[(0, 4096), (2048, 12288)]).ranges(), &[(0, 12288)]);
    assert_eq!(
        set(&[(8192, 12288), (0, 4096)]).ranges(),
        &[(0, 4096), (8192, 12288)],
        "out-of-order ranges are kept sorted"
    );
    assert_eq!(
        set(&[(0, 4096), (8192, 12288), (4096, 8192)]).ranges(),
        &[(0, 12288)],
        "filling a gap joins its neighbours"
    );
    assert_eq!(
        set(&[(0, 4096)]).with_range(10, 10),
        set(&[(0, 4096)]),
        "an empty range changes nothing"
    );
}

#[test]
fn the_committed_prefix_is_the_range_starting_at_zero() {
    assert_eq!(CommittedSet::default().prefix(), 0);
    assert_eq!(set(&[(4096, 8192)]).prefix(), 0);
    assert_eq!(set(&[(0, 4096), (8192, 12288)]).prefix(), 4096);
    assert_eq!(CommittedSet::prefix_of(0), CommittedSet::default());
    assert_eq!(CommittedSet::prefix_of(10).ranges(), &[(0, 10)]);
}

#[test]
fn committed_coverage_containment_and_overlap() {
    let committed = set(&[(0, 4096), (8192, 12288)]);
    assert_eq!(committed.covered(), 8192);
    assert!(committed.contains(0, 4096));
    assert!(committed.contains(8192, 9000));
    assert!(
        !committed.contains(0, 8192),
        "a range spanning a gap is not contained"
    );
    assert!(committed.overlaps(4000, 5000));
    assert!(
        !committed.overlaps(4096, 8192),
        "touching is not overlapping"
    );
    assert!(!committed.is_full(12288));
    assert!(set(&[(0, 12288)]).is_full(12288));
    assert!(!set(&[(0, 12288)]).is_full(12289));
}

#[test]
fn committed_fragments_are_counted_after_the_merge() {
    let committed = set(&[(0, 4096), (8192, 12288)]);
    assert_eq!(committed.fragments(), 2);
    assert_eq!(
        committed.fragments_with(4096, 8192),
        1,
        "a gap fill joins two"
    );
    assert_eq!(
        committed.fragments_with(16384, 20480),
        3,
        "a detached range adds one"
    );
    assert_eq!(
        committed.fragments_with(12288, 16384),
        2,
        "an adjacent range adds none"
    );
}

#[test]
fn the_committed_header_spells_inclusive_ranges() {
    assert_eq!(CommittedSet::default().to_header(), "");
    assert_eq!(
        set(&[(0, 1_048_576), (2_097_152, 4_194_304)]).to_header(),
        "0-1048575,2097152-4194303"
    );
}

#[test]
fn the_stored_committed_form_round_trips_and_refuses_what_is_not_normal() {
    for committed in [
        CommittedSet::default(),
        set(&[(0, 10)]),
        set(&[(0, 4096), (8192, 12288), (20480, 20481)]),
    ] {
        assert_eq!(
            CommittedSet::from_stored(&committed.to_stored()),
            Some(committed)
        );
    }
    for bad in [
        "0-0",
        "5-4",
        "0-10,10-20",
        "0-10,5-20",
        "8-9,0-4",
        "x-1",
        "1",
    ] {
        assert_eq!(
            CommittedSet::from_stored(bad),
            None,
            "{bad} is not a normal set"
        );
    }
}
