//! Observed-remove set (OR-set) with explicit `add_id` binding (SSoT: [Metadata —
//! Add-id Binding] and [Collaborative Metadata]).
//!
//! Tags are modelled as an OR-set so a tag added on one device and removed on another
//! converge predictably. Every add carries an `add_id = (device_id, monotonic_counter)`;
//! every remove targets a specific `add_id`. A remove naming an `add_id` the receiver has
//! never observed an add for is **rejected**, not silently no-op — defeating the "remove an
//! element you never added" attack. Merge is the union of adds and removes, so it is
//! commutative, associative, and idempotent.
//!
//! [Metadata — Add-id Binding]: https://docs/design/metadata/#add-id-binding
//! [Collaborative Metadata]: https://docs/design/metadata/#collaborative-metadata

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A per-device, per-(asset, OR-set) monotonic add identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AddId {
    /// The issuing device (UUIDv4).
    pub device: Uuid,
    /// Monotonic counter, unique per `(device, asset, OR-set)`.
    pub counter: u64,
}

/// A remove targeted an `add_id` that was never observed locally as an add.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnobservedRemove;

/// An observed-remove set of `T`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrSet<T: Ord + Clone> {
    /// Observed adds: `add_id -> element`.
    adds: BTreeMap<AddId, T>,
    /// Tombstoned `add_id`s.
    removes: BTreeSet<AddId>,
}

// Manual `Default` so it does not require `T: Default` (the maps are empty regardless).
impl<T: Ord + Clone> Default for OrSet<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Ord + Clone> OrSet<T> {
    /// An empty set.
    pub fn new() -> Self {
        Self {
            adds: BTreeMap::new(),
            removes: BTreeSet::new(),
        }
    }

    /// Record an add of `element` under `add_id`.
    pub fn add(&mut self, element: T, add_id: AddId) {
        self.adds.insert(add_id, element);
    }

    /// Tombstone `add_id`. Returns [`UnobservedRemove`] if no add for it was ever observed
    /// locally (a fabricated remove), rather than silently no-oping.
    pub fn remove(&mut self, add_id: AddId) -> Result<(), UnobservedRemove> {
        if !self.adds.contains_key(&add_id) {
            return Err(UnobservedRemove);
        }
        self.removes.insert(add_id);
        Ok(())
    }

    /// Merge another replica's state: the union of adds and of removes. Commutative,
    /// associative, idempotent — order of arrival does not matter.
    pub fn merge(&mut self, other: &Self) {
        for (id, el) in &other.adds {
            self.adds.insert(*id, el.clone());
        }
        self.removes.extend(other.removes.iter().copied());
    }

    /// The current logical value: every element whose `add_id` is not tombstoned.
    pub fn value(&self) -> BTreeSet<T> {
        self.adds
            .iter()
            .filter(|(id, _)| !self.removes.contains(id))
            .map(|(_, el)| el.clone())
            .collect()
    }

    /// Whether `add_id` has been observed as an add.
    pub fn observed(&self, add_id: &AddId) -> bool {
        self.adds.contains_key(add_id)
    }

    /// The highest `add_id.counter` this set has ever observed from `device`, or `None` if it
    /// has observed none.
    ///
    /// Deliberately spans **tombstoned adds as well as live ones**: a removed add still *burned*
    /// its counter, so the high-water mark a restarting device reseeds from
    /// ([`Counter::reseed_from_max`](super::counter::Counter::reseed_from_max)) must count it.
    /// Using [`entries`](Self::entries) here instead would let a device re-issue the `add_id` of
    /// a tag it had removed, aliasing two distinct adds — exactly the failure the add-id binding
    /// rule exists to prevent (SSoT: [Metadata — Add-id Binding]).
    ///
    /// [Metadata — Add-id Binding]: https://docs/design/metadata/#add-id-binding
    pub fn max_add_counter_for(&self, device: &Uuid) -> Option<u64> {
        self.adds
            .keys()
            .chain(self.removes.iter())
            .filter(|id| &id.device == device)
            .map(|id| id.counter)
            .max()
    }

    /// The live `(add_id, element)` pairs — those not tombstoned. Unlike [`value`](Self::value)
    /// this preserves each element's `add_id`, so a caller can surface elements (e.g. AI tags) and
    /// later target a specific one by `add_id` (dismiss, promote).
    pub fn entries(&self) -> Vec<(AddId, T)> {
        self.adds
            .iter()
            .filter(|(id, _)| !self.removes.contains(id))
            .map(|(id, el)| (*id, el.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
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
}
