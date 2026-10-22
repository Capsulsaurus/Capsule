use std::collections::{BTreeMap, HashSet};
use std::time::Duration;

use super::*;

#[test]
fn splitmix_matches_the_published_reference_sequence() {
    // The first outputs for seed 0 in the reference implementation: a schedule printed by one
    // build replays on every other only if the generator is the published one.
    let mut rng = SplitMix64(0);
    assert_eq!(rng.next(), 0xE220_A839_7B1D_CDAF);
    assert_eq!(rng.next(), 0x6E78_9E6A_A1B9_65F4);
    assert_eq!(rng.next(), 0x06C4_5D18_8009_454F);
}

#[test]
fn a_seed_always_yields_the_same_plan() {
    for seed in [0, 1, DEFAULT_SEED, u64::MAX] {
        assert_eq!(KillPlan::from_seed(seed), KillPlan::from_seed(seed));
    }
}

#[test]
fn a_plan_cuts_every_phase_once_in_pipeline_order() {
    let plan = KillPlan::from_seed(DEFAULT_SEED);
    assert_eq!(plan.seed, DEFAULT_SEED);
    let phases: Vec<Phase> = plan.points.iter().map(|point| point.phase).collect();
    assert_eq!(phases, Phase::ALL.to_vec());
    for phase in Phase::ALL {
        assert_eq!(plan.point(phase).phase, phase);
    }
}

#[test]
fn every_point_lands_inside_the_window_and_import_only_kills_the_cli() {
    for seed in 0..2_000 {
        for point in KillPlan::from_seed(seed).points {
            assert!(
                (MIN_PERMILLE..=MAX_PERMILLE).contains(&point.permille),
                "seed {seed}: {point:?}"
            );
            if !point.phase.reaches_server() {
                assert_eq!(point.victim, Victim::Cli, "seed {seed}: {point:?}");
            }
        }
    }
}

#[test]
fn seeds_reach_both_victims_and_spread_across_the_window() {
    // Not a statistical test of the generator, which is the published one: a guard that the
    // draw is wired to both outcomes and to the whole window rather than stuck on one value.
    let mut victims: BTreeMap<&str, HashSet<String>> = BTreeMap::new();
    let mut deciles = HashSet::new();
    for seed in 0..500 {
        for point in KillPlan::from_seed(seed).points {
            victims
                .entry(point.phase.name())
                .or_default()
                .insert(format!("{:?}", point.victim));
            deciles.insert(point.permille / 100);
        }
    }
    assert_eq!(victims["import"].len(), 1);
    assert_eq!(victims["push"].len(), 2);
    assert_eq!(victims["restore"].len(), 2);
    assert_eq!(deciles.len(), 10, "{deciles:?}");
}

#[test]
fn the_delay_scales_the_reference_by_the_permille() {
    let point = KillPoint {
        phase: Phase::Push,
        victim: Victim::Server,
        permille: 250,
    };
    assert_eq!(point.delay(Duration::from_secs(4)), Duration::from_secs(1));
    assert_eq!(point.delay(Duration::ZERO), Duration::ZERO);
}

#[test]
fn seeds_default_to_the_pull_request_depth() {
    let seeds = seeds_from(|_| None).expect("the defaults parse");
    assert_eq!(seeds, vec![DEFAULT_SEED, DEFAULT_SEED + 1]);
    assert_eq!(seeds.len() as u64, DEFAULT_SEED_COUNT);
}

#[test]
fn seeds_follow_the_variables() {
    let seeds = seeds_from(|variable| match variable {
        SEED_VAR => Some(" 41 ".to_owned()),
        SEEDS_VAR => Some("3".to_owned()),
        _ => None,
    })
    .expect("both parse");
    assert_eq!(seeds, vec![41, 42, 43]);

    let wraps = seeds_from(|variable| match variable {
        SEED_VAR => Some(u64::MAX.to_string()),
        SEEDS_VAR => Some("2".to_owned()),
        _ => None,
    })
    .expect("both parse");
    assert_eq!(wraps, vec![u64::MAX, 0]);

    let none = seeds_from(|variable| (variable == SEEDS_VAR).then(|| "0".to_owned()))
        .expect("zero parses");
    assert!(none.is_empty());
}

#[test]
fn a_malformed_seed_variable_is_named() {
    let error = seeds_from(|variable| (variable == SEEDS_VAR).then(|| "many".to_owned()))
        .expect_err("not a number");
    assert_eq!(error.variable, SEEDS_VAR);
    assert_eq!(error.value, "many");
    assert!(error.to_string().contains(SEEDS_VAR), "{error}");
}
