use super::*;
use crate::store::UserId;

#[test]
fn a_counter_key_names_its_kind_and_its_scope() {
    let key = CounterKey::ShareSource("203.0.113.7".to_owned());
    assert_eq!(
        counter_key(&key),
        "capsule:counter:share_source:203.0.113.7"
    );
    let key = CounterKey::LoginAttempts(UserId::new("u1"));
    assert_eq!(counter_key(&key), "capsule:counter:login_attempts:u1");
}

#[test]
fn a_partition_index_names_its_kind_and_no_scope() {
    let key = CounterKey::DropLink("d1".to_owned());
    assert_eq!(index_key(&key), "capsule:counter_index:drop_link");
    assert_eq!(
        index_key(&key),
        index_key(&CounterKey::DropLink("d2".to_owned())),
        "every scope of one variant shares one partition"
    );
    assert_ne!(
        index_key(&key),
        index_key(&CounterKey::ShareLink("d1".to_owned())),
        "and no two variants share one"
    );
}

#[test]
fn a_full_partition_is_the_refusal_is_at_capacity_recognises() {
    let key = CounterKey::DropLink("d1".to_owned());
    let error = at_ceiling(&key, 2, 2);
    assert!(super::super::is_at_capacity(&error), "{error:?}");
    assert!(error.to_string().contains("drop_link"), "{error}");
}

#[test]
fn a_window_is_passed_in_both_units() {
    let (micros, millis) = window_args(Budget::new(3, jiff::SignedDuration::from_secs(2)));
    assert_eq!(micros, "2000000");
    assert_eq!(millis, "2000");
}

#[test]
fn remaining_never_underflows() {
    assert_eq!(admitted(3, 5), Verdict::Admitted { remaining: 0 });
    assert_eq!(admitted(3, 1), Verdict::Admitted { remaining: 2 });
}
