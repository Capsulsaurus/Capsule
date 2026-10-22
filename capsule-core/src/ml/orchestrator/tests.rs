//! The orchestration's store-free rules. The paths that land vectors in an index are driven
//! end to end over the library's SQLite catalog in `capsule-library/tests/ml_orchestrator.rs`.

use super::*;
use crate::ml::FixtureRunner;

const PLATFORM: &str = "cpu-reference";

// ── Platform-partition fallback logic ────────────────────────────────────────────────────

#[test]
fn a_bit_exact_device_shares_the_canonical_partition() {
    let runner = FixtureRunner::new(PLATFORM);
    let reg = Registry::canonical();
    let ka = KnownAnswer::capture(
        &runner,
        &reg,
        TaskKind::SemanticSearch,
        b"known-answer probe",
    )
    .unwrap();
    // The same runner reproduces its own known-answer bit-for-bit → shared partition.
    assert_eq!(
        resolve_partition(&runner, &ka).unwrap(),
        CANONICAL_PARTITION
    );
}

#[test]
fn a_non_bit_exact_device_falls_back_to_its_own_partition() {
    let runner = FixtureRunner::new("wonky-npu");
    let reg = Registry::canonical();
    // A known-answer this device's deterministic path does NOT reproduce (a synthetic vector
    // no hash-seeded embedding equals): the check fails, so the fallback is its own platform.
    let mut expected = vec![0.0f32; reg.dim_for(TaskKind::SemanticSearch).unwrap().get()];
    expected[0] = 1.0;
    let ka = KnownAnswer {
        task: TaskKind::SemanticSearch,
        input: b"known-answer probe".to_vec(),
        expected,
    };
    assert_eq!(resolve_partition(&runner, &ka).unwrap(), "wonky-npu");
}

// ── Device-bound execution policy ────────────────────────────────────────────────────────

#[test]
fn batch_mode_follows_the_memory_budget() {
    assert_eq!(choose_batch_mode(4096, 2048), BatchMode::Vertical);
    assert_eq!(choose_batch_mode(1024, 2048), BatchMode::Horizontal);
    assert_eq!(choose_batch_mode(2048, 2048), BatchMode::Vertical); // exactly fits
}

#[test]
fn micro_batch_clamps_to_ceiling_and_pending() {
    assert_eq!(micro_batch_size(100, 8), 8);
    assert_eq!(micro_batch_size(5, 8), 4);
    assert_eq!(micro_batch_size(3, 8), 1);
    assert_eq!(micro_batch_size(100, 4), 4);
    assert_eq!(micro_batch_size(0, 8), 1); // never zero
}

#[test]
fn thermal_pause_triggers_at_threshold() {
    assert!(!should_pause_for_heat(38.0, 40.0));
    assert!(should_pause_for_heat(40.0, 40.0));
    assert!(should_pause_for_heat(41.5, 40.0));
}
