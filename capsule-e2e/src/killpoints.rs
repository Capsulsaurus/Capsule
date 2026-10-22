//! Seeded kill schedules for the kill-anywhere suite (`tests/kill_anywhere.rs`, issue #678).
//!
//! A schedule says, for each phase of the pipeline the suite drives — import, push, restore —
//! **which process dies** and **how far into the phase** it dies. Everything here is a pure
//! function of a `u64` seed, so a failing seed printed by a nightly soak replays exactly on a
//! developer's machine with `CAPSULE_KILL_SEED=<seed> CAPSULE_KILL_SEEDS=1`.
//!
//! # Why a hand-written generator
//!
//! SplitMix64, ten lines, rather than `rand`: a seed is only worth printing if the same seed
//! yields the same schedule next year, and `rand` documents that `StdRng`'s algorithm may change
//! between releases. SplitMix64 is fixed by its definition, and nothing here needs more than a
//! uniform draw.
//!
//! # Where in a phase
//!
//! A kill point is a fraction of the phase, in permille, not a wall-clock delay: how long an
//! import of the corpus takes depends on the machine and the build profile, and a fixed delay
//! that lands mid-transfer on a laptop lands after exit on a slow CI runner. The suite measures
//! each phase once, unkilled, and [`KillPoint::delay`] scales the fraction by that measurement.
//! A point that still lands after the process has exited is reported as such and is not a
//! failure: the rerun must converge either way.

use std::time::Duration;

/// The variable naming the first seed of a run.
pub const SEED_VAR: &str = "CAPSULE_KILL_SEED";

/// The variable naming how many consecutive seeds a run tries.
pub const SEEDS_VAR: &str = "CAPSULE_KILL_SEEDS";

/// The first seed when [`SEED_VAR`] is unset: the issue number, so the default run is stable.
pub const DEFAULT_SEED: u64 = 678;

/// How many seeds a run tries when [`SEEDS_VAR`] is unset — the pull-request depth. The nightly
/// soak raises it.
pub const DEFAULT_SEED_COUNT: u64 = 2;

/// The earliest a kill lands, in permille of the phase. Not zero: a process killed before it
/// has read its arguments proves nothing a later point does not.
pub const MIN_PERMILLE: u16 = 50;

/// The latest a kill lands, in permille of the phase.
pub const MAX_PERMILLE: u16 = 950;

/// One step of the pipeline the suite drives through the real CLI binary, ordered as the
/// pipeline runs them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Phase {
    /// `capsule import`: offline, so only the CLI can die under it.
    Import,
    /// `capsule push`: the CLI or the server.
    Push,
    /// `capsule restore`: the CLI or the server.
    Restore,
}

impl Phase {
    /// Every phase, in pipeline order.
    pub const ALL: [Self; 3] = [Self::Import, Self::Push, Self::Restore];

    /// Whether the phase talks to the server, and so whether killing the server can matter.
    #[must_use]
    pub const fn reaches_server(self) -> bool {
        !matches!(self, Self::Import)
    }

    /// The phase's name, as the suite logs it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Import => "import",
            Self::Push => "push",
            Self::Restore => "restore",
        }
    }
}

/// The process a kill point `SIGKILL`s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Victim {
    /// The `capsule` process running the phase.
    Cli,
    /// The `capsule-server` process the phase talks to; the suite restarts it on the same port
    /// at once, over the same Postgres, Valkey and blob root.
    Server,
}

/// Where one phase is cut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillPoint {
    /// The phase it cuts.
    pub phase: Phase,
    /// The process that dies.
    pub victim: Victim,
    /// How far into the phase, in permille, within [`MIN_PERMILLE`]`..=`[`MAX_PERMILLE`].
    pub permille: u16,
}

impl KillPoint {
    /// The delay after the phase starts at which this point fires, given how long the phase
    /// took unkilled.
    #[must_use]
    pub fn delay(&self, reference: Duration) -> Duration {
        reference * u32::from(self.permille) / 1000
    }
}

/// One seed's schedule: one kill point per phase, in pipeline order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillPlan {
    /// The seed the plan was drawn from.
    pub seed: u64,
    /// One point per [`Phase::ALL`] entry, in the same order.
    pub points: [KillPoint; 3],
}

impl KillPlan {
    /// Draw the plan for `seed`. The same seed always yields the same plan.
    #[must_use]
    pub fn from_seed(seed: u64) -> Self {
        let mut rng = SplitMix64(seed);
        let points = Phase::ALL.map(|phase| {
            let victim = if phase.reaches_server() && rng.below(2) == 1 {
                Victim::Server
            } else {
                Victim::Cli
            };
            let span = u64::from(MAX_PERMILLE - MIN_PERMILLE) + 1;
            let offset = u16::try_from(rng.below(span)).expect("the span fits a u16");
            KillPoint {
                phase,
                victim,
                permille: MIN_PERMILLE + offset,
            }
        });
        Self { seed, points }
    }

    /// The point that cuts `phase`.
    #[must_use]
    pub fn point(&self, phase: Phase) -> KillPoint {
        self.points
            .iter()
            .copied()
            .find(|point| point.phase == phase)
            .expect("a plan holds one point per phase")
    }
}

/// Why the seed variables could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedError {
    /// The variable at fault.
    pub variable: &'static str,
    /// What it held.
    pub value: String,
}

impl std::fmt::Display for SeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} must be an unsigned integer, got {:?}",
            self.variable, self.value
        )
    }
}

impl std::error::Error for SeedError {}

/// The seeds a run tries: [`SEED_VAR`] (default [`DEFAULT_SEED`]) and the
/// [`SEEDS_VAR`]` - 1` seeds after it (default [`DEFAULT_SEED_COUNT`] in all), read through
/// `lookup` so the parsing is testable without touching the process environment.
///
/// # Errors
///
/// [`SeedError`] when either variable is set to something that is not an unsigned integer.
pub fn seeds_from(lookup: impl Fn(&str) -> Option<String>) -> Result<Vec<u64>, SeedError> {
    let read = |variable: &'static str, default: u64| match lookup(variable) {
        None => Ok(default),
        Some(value) => value
            .trim()
            .parse::<u64>()
            .map_err(|_| SeedError { variable, value }),
    };
    let first = read(SEED_VAR, DEFAULT_SEED)?;
    let count = read(SEEDS_VAR, DEFAULT_SEED_COUNT)?;
    Ok((0..count).map(|i| first.wrapping_add(i)).collect())
}

/// [`seeds_from`] over the process environment.
///
/// # Errors
///
/// See [`seeds_from`].
pub fn seeds_from_env() -> Result<Vec<u64>, SeedError> {
    seeds_from(|variable| std::env::var(variable).ok())
}

/// SplitMix64 (Steele, Lea and Flood, 2014): the whole generator.
struct SplitMix64(u64);

impl SplitMix64 {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A draw in `0..n`. The modulo bias is below 2⁻⁵⁰ for the spans used here.
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[cfg(test)]
mod tests;
