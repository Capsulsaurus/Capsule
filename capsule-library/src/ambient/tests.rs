use super::*;

#[test]
fn system_ids_mint_distinct_v7_ids_stamped_now() {
    let before = Timestamp::now().as_millisecond();
    let ids = system_ids();
    let (a, b) = (ids.uuid_v7(), ids.uuid_v7());
    let after = Timestamp::now().as_millisecond();
    assert_ne!(a, b);
    assert_eq!(a.get_version_num(), 7);
    let (secs, nanos) = a.get_timestamp().expect("v7 has a timestamp").to_unix();
    let millis = i64::try_from(secs).unwrap() * 1000 + i64::from(nanos / 1_000_000);
    assert!((before..=after).contains(&millis));
}

#[test]
fn os_rng_fills_the_buffer() {
    let mut buf = [0u8; 32];
    OsRng.fill_bytes(&mut buf);
    assert_ne!(buf, [0u8; 32]);
}
