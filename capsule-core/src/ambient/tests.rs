use super::testing::{FIXED_MILLIS, FixedClock, SeededRng, TestIds};
use super::*;

#[test]
fn uuid_v7_carries_the_clock_millis_and_version_7() {
    let fixture = TestIds::new(1);
    let id = fixture.ids().uuid_v7();
    assert_eq!(id.get_version_num(), 7);
    assert_eq!(id.get_variant(), uuid::Variant::RFC4122);
    let (secs, nanos) = id.get_timestamp().expect("v7 has a timestamp").to_unix();
    let millis = i64::try_from(secs).unwrap() * 1000 + i64::from(nanos / 1_000_000);
    assert_eq!(millis, FIXED_MILLIS);
}

#[test]
fn one_seed_reproduces_one_sequence() {
    let a = TestIds::new(42);
    let b = TestIds::new(42);
    let from_a: Vec<Uuid> = (0..4).map(|_| a.ids().uuid_v7()).collect();
    let from_b: Vec<Uuid> = (0..4).map(|_| b.ids().uuid_v7()).collect();
    assert_eq!(from_a, from_b);
}

#[test]
fn successive_ids_from_one_source_differ() {
    let fixture = TestIds::new(7);
    let ids = fixture.ids();
    let first = ids.uuid_v7();
    let second = ids.uuid_v7();
    assert_ne!(first, second);
}

#[test]
fn different_seeds_yield_different_ids() {
    assert_ne!(
        TestIds::new(1).ids().uuid_v7(),
        TestIds::new(2).ids().uuid_v7()
    );
}

#[test]
fn a_pre_epoch_clock_stamps_zero_and_stays_v7() {
    let clock = FixedClock::at_millis(-5_000);
    let rng = SeededRng::new(3);
    let id = IdSource::new(&clock, &rng).uuid_v7();
    assert_eq!(id.get_version_num(), 7);
    let (secs, nanos) = id.get_timestamp().unwrap().to_unix();
    assert_eq!((secs, nanos), (0, 0));
}

#[test]
fn ids_order_by_clock() {
    let rng = SeededRng::new(9);
    let early = FixedClock::at_millis(FIXED_MILLIS);
    let late = FixedClock::at_millis(FIXED_MILLIS + 1);
    let a = IdSource::new(&early, &rng).uuid_v7();
    let b = IdSource::new(&late, &rng).uuid_v7();
    assert!(a < b);
}

#[test]
fn seeded_rng_fills_a_partial_chunk() {
    let rng = SeededRng::new(5);
    let mut buf = [0u8; 13];
    rng.fill_bytes(&mut buf);
    assert_ne!(buf, [0u8; 13]);
}
