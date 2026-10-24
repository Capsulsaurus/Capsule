//! Deterministic [`Clock`] and [`Rng`] doubles for tests: a fixed clock and a seeded source.
//!
//! Compiled only under `cfg(test)` or the non-default `test-support` feature.

use std::sync::Mutex;

use jiff::Timestamp;

use super::{Clock, IdSource, Rng};

/// A clock stopped at one instant.
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(pub Timestamp);

impl FixedClock {
    /// A clock stopped at `millis` Unix milliseconds.
    pub fn at_millis(millis: i64) -> Self {
        Self(Timestamp::from_millisecond(millis).expect("millis within jiff's range"))
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.0
    }
}

/// A seeded, deterministic byte source (SplitMix64). Two sources with one seed yield one
/// sequence; successive draws from one source differ.
#[derive(Debug)]
pub struct SeededRng(Mutex<u64>);

impl SeededRng {
    /// A source seeded with `seed`.
    pub fn new(seed: u64) -> Self {
        Self(Mutex::new(seed))
    }
}

impl Rng for SeededRng {
    fn fill_bytes(&self, buf: &mut [u8]) {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for chunk in buf.chunks_mut(8) {
            *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
            let mut z = *state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            chunk.copy_from_slice(&z.to_le_bytes()[..chunk.len()]);
        }
    }
}

/// The clock every fixed test source is stopped at: 2026-01-01T00:00:00Z.
pub const FIXED_MILLIS: i64 = 1_767_225_600_000;

/// A fixed clock at [`FIXED_MILLIS`] and a source seeded with `seed`, for building an
/// [`IdSource`] with [`TestIds::ids`].
#[derive(Debug)]
pub struct TestIds {
    /// The fixed clock.
    pub clock: FixedClock,
    /// The seeded source.
    pub rng: SeededRng,
}

impl TestIds {
    /// A fixed clock at [`FIXED_MILLIS`] and a source seeded with `seed`.
    pub fn new(seed: u64) -> Self {
        Self {
            clock: FixedClock::at_millis(FIXED_MILLIS),
            rng: SeededRng::new(seed),
        }
    }

    /// The identifier source over this clock and source.
    pub fn ids(&self) -> IdSource<'_> {
        IdSource::new(&self.clock, &self.rng)
    }
}
