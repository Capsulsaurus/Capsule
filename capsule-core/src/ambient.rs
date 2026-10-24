//! The ambient inputs a caller injects: wall-clock time and identifier randomness.
//!
//! The stateless core reads no clock and draws no identifier itself ([Module Map — Stateless
//! Core]). Where a core operation mints a UUIDv7 — an account's user and device ids, an MLS
//! intent id, a federation `group_id` — it takes an [`IdSource`], built from a [`Clock`] and an
//! [`Rng`] the caller implements. `capsule-library` supplies the system implementations; tests
//! supply a fixed clock and a seeded source, so every minted identifier is reproducible.
//!
//! [`Rng`] is for **identifiers only**. Key material, salts and nonces never come from an
//! injectable source: they are drawn from the OS CSPRNG behind [`crate::crypto::rng`], the one
//! sanctioned exception.
//!
//! [Module Map — Stateless Core]: https://docs/design/module-map/#stateless-core
//! [`IdSource`]: crate::ambient::IdSource
//! [`Clock`]: crate::ambient::Clock
//! [`Rng`]: crate::ambient::Rng

use jiff::Timestamp;
use uuid::Uuid;

/// A source of wall-clock time.
pub trait Clock: Send + Sync {
    /// The current instant.
    fn now(&self) -> Timestamp;
}

/// A source of random bytes for **identifiers** (never key material; see the module docs).
pub trait Rng: Send + Sync {
    /// Fill `buf` with random bytes.
    fn fill_bytes(&self, buf: &mut [u8]);
}

/// Mints UUIDv7 identifiers from an injected [`Clock`] and [`Rng`].
///
/// Cheap to copy: it borrows both sources, so one value is threaded through a call chain.
#[derive(Clone, Copy)]
pub struct IdSource<'a> {
    clock: &'a dyn Clock,
    rng: &'a dyn Rng,
}

impl std::fmt::Debug for IdSource<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IdSource").finish_non_exhaustive()
    }
}

impl<'a> IdSource<'a> {
    /// An identifier source over `clock` and `rng`.
    pub fn new(clock: &'a dyn Clock, rng: &'a dyn Rng) -> Self {
        Self { clock, rng }
    }

    /// A fresh UUIDv7: the clock's Unix milliseconds followed by 74 random bits.
    ///
    /// An instant before the Unix epoch stamps as `0`; the identifier stays well-formed and
    /// unique by its random bits.
    pub fn uuid_v7(&self) -> Uuid {
        let now = self.clock.now();
        let millis = u64::try_from(now.as_millisecond()).unwrap_or(0);
        let mut random = [0u8; 10];
        self.rng.fill_bytes(&mut random);
        let id = uuid::Builder::from_unix_timestamp_millis(millis, &random).into_uuid();
        tracing::trace!(%id, millis, "minted UUIDv7");
        id
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

#[cfg(test)]
mod tests;
