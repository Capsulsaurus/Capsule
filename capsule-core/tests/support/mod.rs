//! What this crate's property suites share: one seeded proptest configuration.
//!
//! The testing plan's first rule is that a randomized test is seeded, prints its seed, and
//! replays from it. proptest's own default draws a fresh seed per run and prints none, so every
//! suite here runs under [`seeded`] instead: a fixed seed unless `PROPTEST_RNG_SEED` names
//! another, written to stderr (which nextest shows for a failing test) together with the
//! command-line that replays it. `PROPTEST_CASES` still raises the case count, which is how a
//! soak run widens the search without touching the code.

use proptest::test_runner::{Config, RngSeed};

/// The seed every suite runs under when `PROPTEST_RNG_SEED` is unset.
const DEFAULT_SEED: u64 = 0x0CA9_5017;

/// A proptest configuration running `cases` cases (unless `PROPTEST_CASES` overrides it) under a
/// fixed, printed seed, with no regression file written beside the sources: the seed is what
/// replays a failure.
pub(crate) fn seeded(cases: u32) -> Config {
    let seed = std::env::var("PROPTEST_RNG_SEED")
        .ok()
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(DEFAULT_SEED);
    eprintln!("proptest seed {seed}; replay with PROPTEST_RNG_SEED={seed}");
    let base = Config::default();
    let cases = if std::env::var_os("PROPTEST_CASES").is_some() {
        base.cases
    } else {
        cases
    };
    Config {
        cases,
        rng_seed: RngSeed::Fixed(seed),
        failure_persistence: None,
        ..base
    }
}
