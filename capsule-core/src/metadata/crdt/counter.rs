//! The per-device monotonic `add_id` counter (SSoT: [Metadata — Add-id Binding §
//! Counter durability across restarts]).
//!
//! A `monotonic_counter` must never repeat for a given `(device, asset, OR-set)`: a reused
//! `add_id` would alias two distinct adds. On restart/reinstall the counter is reseeded to
//! **one past the maximum counter this device has ever issued** (recovered from the signed
//! sidecars it wrote). It resets to zero only when the device can prove it has issued
//! nothing. This makes the counter monotonic over a `device_id`'s lifetime, not just a
//! process.
//!
//! [Metadata — Add-id Binding § Counter durability across restarts]: https://docs/design/metadata/#add-id-binding

use uuid::Uuid;

use super::or_set::AddId;

/// A monotonic counter issuing `add_id`s for one device.
#[derive(Debug, Clone)]
pub struct Counter {
    device: Uuid,
    next: u64,
}

impl Counter {
    /// A counter for `device` that has issued nothing yet (next = 0).
    pub fn new(device: Uuid) -> Self {
        Self { device, next: 0 }
    }

    /// Reseed from the maximum `add_id.counter` this device has ever issued (recovered from
    /// its own signed sidecars). `None` means it has issued nothing → reset to zero.
    pub fn reseed_from_max(&mut self, max_issued: Option<u64>) {
        self.next = match max_issued {
            Some(m) => m + 1,
            None => 0,
        };
    }

    /// Issue the next `add_id` and advance the counter.
    pub fn issue(&mut self) -> AddId {
        let id = AddId {
            device: self.device,
            counter: self.next,
        };
        self.next += 1;
        id
    }

    /// The next counter value that would be issued.
    pub fn peek(&self) -> u64 {
        self.next
    }
}

#[cfg(test)]
mod tests {
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
}
