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
mod tests;
