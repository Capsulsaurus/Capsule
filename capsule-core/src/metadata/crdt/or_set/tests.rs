use super::*;

fn dev(n: u128) -> Uuid {
    Uuid::from_u128(n)
}
fn id(device: u128, counter: u64) -> AddId {
    AddId {
        device: dev(device),
        counter,
    }
}

#[test]
fn max_add_counter_is_per_device_and_counts_tombstones() {
    let mut s: OrSet<String> = OrSet::new();
    assert_eq!(s.max_add_counter_for(&dev(1)), None);

    s.add("a".into(), id(1, 0));
    s.add("b".into(), id(1, 7));
    s.add("c".into(), id(2, 99));
    // A tombstoned add still burned its counter, so it must raise the high-water mark.
    s.remove(id(1, 7)).unwrap();

    assert_eq!(s.max_add_counter_for(&dev(1)), Some(7));
    assert_eq!(s.max_add_counter_for(&dev(2)), Some(99));
    assert_eq!(s.max_add_counter_for(&dev(3)), None);
}

/// A remove merged in from a peer names an `add_id` this replica may hold no add for once
/// the sets are pruned; the high-water mark still has to see it.
#[test]
fn max_add_counter_sees_a_merged_remove_without_its_add() {
    let mut a: OrSet<String> = OrSet::new();
    a.add("x".into(), id(4, 12));
    a.remove(id(4, 12)).unwrap();

    let mut b: OrSet<String> = OrSet::new();
    b.merge(&a);
    assert_eq!(b.max_add_counter_for(&dev(4)), Some(12));
}

#[test]
fn add_then_value() {
    let mut s = OrSet::new();
    s.add("vacation".to_string(), id(1, 0));
    s.add("2026".to_string(), id(1, 1));
    let v = s.value();
    assert!(v.contains("vacation"));
    assert!(v.contains("2026"));
}

#[test]
fn remove_of_unobserved_add_id_is_rejected() {
    let mut s: OrSet<String> = OrSet::new();
    assert_eq!(s.remove(id(9, 9)), Err(UnobservedRemove));
    // After observing the add, the remove succeeds.
    s.add("x".into(), id(9, 9));
    assert_eq!(s.remove(id(9, 9)), Ok(()));
    assert!(s.value().is_empty());
}

#[test]
fn merge_converges_regardless_of_order() {
    // Replica A adds "a" then removes it; replica B adds "b". Merge both directions.
    let mut a: OrSet<String> = OrSet::new();
    a.add("a".into(), id(1, 0));
    a.remove(id(1, 0)).unwrap();
    a.add("shared".into(), id(1, 1));

    let mut b: OrSet<String> = OrSet::new();
    b.add("b".into(), id(2, 0));
    b.add("shared".into(), id(2, 1)); // same value, different add_id

    let mut ab = a.clone();
    ab.merge(&b);
    let mut ba = b.clone();
    ba.merge(&a);

    assert_eq!(ab, ba, "merge is commutative");
    assert_eq!(ab.value(), ba.value());
    let v = ab.value();
    assert!(
        !v.contains("a"),
        "removed element stays removed after merge"
    );
    assert!(v.contains("b"));
    assert!(v.contains("shared"));
}

#[test]
fn merge_is_idempotent_and_associative() {
    let mut a: OrSet<String> = OrSet::new();
    a.add("x".into(), id(1, 0));
    let mut b: OrSet<String> = OrSet::new();
    b.add("y".into(), id(2, 0));
    let mut c: OrSet<String> = OrSet::new();
    c.add("z".into(), id(3, 0));

    // (a ∪ b) ∪ c
    let mut left = a.clone();
    left.merge(&b);
    left.merge(&c);
    // a ∪ (b ∪ c)
    let mut bc = b.clone();
    bc.merge(&c);
    let mut right = a.clone();
    right.merge(&bc);
    assert_eq!(left.value(), right.value());

    // Idempotent: merging the same state twice changes nothing.
    let before = left.clone();
    left.merge(&b);
    assert_eq!(left, before);
}

#[test]
fn add_remove_concurrent_on_different_add_ids() {
    // "shared" added independently on two devices; removing one add_id keeps the other.
    let mut s: OrSet<String> = OrSet::new();
    s.add("shared".into(), id(1, 0));
    s.add("shared".into(), id(2, 0));
    s.remove(id(1, 0)).unwrap();
    assert!(s.value().contains("shared"), "the other add keeps it alive");
    s.remove(id(2, 0)).unwrap();
    assert!(!s.value().contains("shared"));
}
