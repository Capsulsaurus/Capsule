use super::*;

#[test]
fn issues_strictly_increasing_counters() {
    let mut c = Counter::new(Uuid::from_u128(1));
    assert_eq!(c.issue().counter, 0);
    assert_eq!(c.issue().counter, 1);
    assert_eq!(c.issue().counter, 2);
    assert_eq!(c.peek(), 3);
}

#[test]
fn reseed_is_one_past_max_ever_issued() {
    let device = Uuid::from_u128(7);
    let mut c = Counter::new(device);
    c.issue();
    c.issue(); // issued 0, 1
    // Simulate a restart: drop the in-memory counter, reseed from the max observed in
    // this device's existing sidecars (1).
    let mut restarted = Counter::new(device);
    restarted.reseed_from_max(Some(1));
    let next = restarted.issue();
    assert_eq!(
        next.counter, 2,
        "must be strictly greater than every prior counter"
    );
    assert_eq!(next.device, device);
}

#[test]
fn reseed_to_zero_when_nothing_ever_issued() {
    let mut c = Counter::new(Uuid::from_u128(1));
    c.reseed_from_max(None);
    assert_eq!(c.issue().counter, 0);
}

#[test]
fn reseed_never_reuses_a_written_counter() {
    // The key safety property: across many restart cycles, no counter ever repeats.
    let device = Uuid::from_u128(3);
    let mut max_written: Option<u64> = None;
    let mut all = Vec::new();
    for _ in 0..5 {
        let mut c = Counter::new(device);
        c.reseed_from_max(max_written);
        for _ in 0..3 {
            let issued = c.issue().counter;
            assert!(!all.contains(&issued), "counter {issued} reused");
            all.push(issued);
            max_written = Some(max_written.map_or(issued, |m| m.max(issued)));
        }
    }
}
