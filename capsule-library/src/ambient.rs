//! The system implementations of `capsule-core`'s injected [`Clock`] and [`Rng`].
//!
//! `capsule-core` reads no clock and draws no identifier itself; every UUIDv7 it mints comes from
//! an [`IdSource`] its caller builds. This module is where a client's real wall clock and the OS
//! random source enter: [`system_ids`] is the source every production call in this crate passes.
//! See [Module Map — Stateless Core](https://docs/design/module-map/#stateless-core).

use capsule_core::ambient::{Clock, IdSource, Rng};
use jiff::Timestamp;

/// The system wall clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// The OS random source, through `capsule-core`'s CSPRNG.
#[derive(Debug, Clone, Copy, Default)]
pub struct OsRng;

impl Rng for OsRng {
    fn fill_bytes(&self, buf: &mut [u8]) {
        capsule_core::crypto::rng::fill(buf);
    }
}

/// An identifier source over the [`SystemClock`] and [`OsRng`].
pub fn system_ids() -> IdSource<'static> {
    IdSource::new(&SystemClock, &OsRng)
}

#[cfg(test)]
mod tests;
