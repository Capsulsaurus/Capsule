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
mod tests;
