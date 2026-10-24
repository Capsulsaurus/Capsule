use super::*;

#[test]
fn short_id_is_the_last_eight_hex_digits() {
    let id = Uuid::parse_str("0192f3a4-5b6c-7d8e-9fa0-1b2c3d4e5f60").unwrap();
    assert_eq!(short_id(id), "3d4e5f60");
}

#[test]
fn short_id_tells_apart_ids_that_share_a_creation_prefix() {
    let a = Uuid::parse_str("0192f3a4-5b6c-7d8e-9fa0-000000000001").unwrap();
    let b = Uuid::parse_str("0192f3a4-5b6c-7d8e-9fa0-000000000002").unwrap();
    assert_ne!(short_id(a), short_id(b));
}
