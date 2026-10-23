use super::*;

fn dev(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

#[test]
fn later_timestamp_wins_and_loser_is_superseded() {
    let mut r = Lww::new();
    r.set("first".to_string(), "2026-05-31T10:00:00Z", dev(1));
    r.set("second".to_string(), "2026-05-31T11:00:00Z", dev(2));
    assert_eq!(r.get(), Some(&"second".to_string()));
    assert_eq!(r.superseded.len(), 1);
    assert_eq!(r.superseded[0].value, "first");
}

#[test]
fn tie_breaks_on_larger_device_id() {
    // Same timestamp, two devices → the larger device id wins; the other is superseded.
    let mut r = Lww::new();
    r.set("from-dev-1".to_string(), "2026-05-31T10:00:00Z", dev(1));
    r.set("from-dev-9".to_string(), "2026-05-31T10:00:00Z", dev(9));
    assert_eq!(r.get(), Some(&"from-dev-9".to_string()));
    assert_eq!(r.superseded[0].value, "from-dev-1");
}

#[test]
fn an_earlier_write_arriving_late_does_not_clobber() {
    let mut r = Lww::new();
    r.set("new".to_string(), "2026-05-31T11:00:00Z", dev(2));
    // An older edit arrives after the newer one: current is unchanged, loser recorded.
    r.set("old".to_string(), "2026-05-31T09:00:00Z", dev(1));
    assert_eq!(r.get(), Some(&"new".to_string()));
    assert!(r.superseded.iter().any(|s| s.value == "old"));
}

#[test]
fn superseded_log_is_capped() {
    let mut r = Lww::new();
    for i in 0..(SUPERSEDED_CAP + 5) {
        // Each later write wins and pushes the prior winner to superseded.
        let ts = format!("2026-05-31T{i:02}:00:00Z");
        r.set(format!("v{i}"), ts, dev(1));
    }
    assert_eq!(r.superseded.len(), SUPERSEDED_CAP);
    // Most-recently superseded is first.
    assert_eq!(r.superseded[0].value, format!("v{}", SUPERSEDED_CAP + 3));
}

#[test]
fn merge_converges() {
    let mut a = Lww::new();
    a.set("a".to_string(), "2026-05-31T10:00:00Z", dev(1));
    let mut b = Lww::new();
    b.set("b".to_string(), "2026-05-31T11:00:00Z", dev(2));

    let mut ab = a.clone();
    ab.merge(&b);
    let mut ba = b.clone();
    ba.merge(&a);
    assert_eq!(ab.get(), ba.get());
    assert_eq!(ab.get(), Some(&"b".to_string()));
}

#[test]
fn unparseable_timestamp_is_a_no_op() {
    let mut r = Lww::new();
    r.set("ok".to_string(), "2026-05-31T10:00:00Z", dev(1));
    assert!(!r.set("bad".to_string(), "not-a-date", dev(2)));
    assert_eq!(r.get(), Some(&"ok".to_string()));
}
