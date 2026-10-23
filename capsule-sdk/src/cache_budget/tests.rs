use capsule_library::db::{CachedRepresentationRow, DatabaseDriver};
use capsule_library::library::cache_sweep;

use super::*;

const ALL: [ConnectionClass; 5] = [
    ConnectionClass::Unmetered,
    ConnectionClass::Metered,
    ConnectionClass::Constrained,
    ConnectionClass::Adverse,
    ConnectionClass::Offline,
];

/// Each class yields the budget multiplier and the bulk deferral the networking table states:
/// only `unmetered` permits bulk work, and the budget grows with re-fetch cost.
#[test]
fn each_class_yields_its_budget_and_deferral() {
    const BASE: u64 = 1000;
    let expected = [
        (ConnectionClass::Unmetered, 1000, false),
        (ConnectionClass::Metered, 2000, true),
        (ConnectionClass::Constrained, 2000, true),
        (ConnectionClass::Adverse, 3000, true),
        (ConnectionClass::Offline, 4000, true),
    ];
    for (class, budget, defer) in expected {
        let plan = plan_for(class, BASE);
        assert_eq!(plan.class, class);
        assert_eq!(plan.budget_bytes, budget, "{class:?} budget");
        assert_eq!(plan.defer_bulk, defer, "{class:?} deferral");
    }
}

#[test]
fn sweep_budget_saturates_into_the_signed_width() {
    let plan = plan_for(ConnectionClass::Offline, u64::MAX);
    assert_eq!(plan.budget_bytes, u64::MAX);
    assert_eq!(plan.sweep_budget(), i64::MAX);
    assert_eq!(plan_for(ConnectionClass::Unmetered, 7).sweep_budget(), 7);
}

#[test]
fn in_memory_source_reports_what_was_set() {
    let source = InMemoryConnectionClass::new(ConnectionClass::Unmetered);
    assert_eq!(source.current(), ConnectionClass::Unmetered);
    for class in ALL {
        source.set(class);
        assert_eq!(source.current(), class);
    }
}

#[test]
fn desktop_source_defaults_to_unmetered() {
    assert_eq!(
        DesktopConnectionClass::new().current(),
        ConnectionClass::Unmetered
    );
    assert_eq!(
        DesktopConnectionClass::with_probe(Box::new(|| None)).current(),
        ConnectionClass::Unmetered
    );
}

#[test]
fn desktop_source_classifies_what_its_probe_reads() {
    let metered = DesktopConnectionClass::with_probe(Box::new(|| {
        Some(PathSignals {
            has_path: true,
            is_expensive: true,
            not_metered_capability: false,
            is_constrained: false,
        })
    }));
    assert_eq!(metered.current(), ConnectionClass::Metered);
    let offline = DesktopConnectionClass::with_probe(Box::new(|| Some(PathSignals::offline())));
    assert_eq!(offline.current(), ConnectionClass::Offline);
}

#[test]
fn plan_is_reused_until_the_class_changes() {
    let mut budget = CacheBudget::new(InMemoryConnectionClass::new(ConnectionClass::Metered), 10);
    let (first, replanned) = budget.current_plan();
    assert!(replanned, "the first sample plans");
    assert_eq!(first, plan_for(ConnectionClass::Metered, 10));
    assert!(budget.defer_bulk());

    let (same, replanned) = budget.current_plan();
    assert!(!replanned);
    assert_eq!(same, first);

    budget.source().set(ConnectionClass::Unmetered);
    let (next, replanned) = budget.current_plan();
    assert!(replanned);
    assert_eq!(next, plan_for(ConnectionClass::Unmetered, 10));
    assert!(!budget.defer_bulk());
}

/// A bulk-deferring plan demotes a full push to staged and keeps a staged one; a plan that
/// permits bulk work keeps whatever was requested.
#[test]
fn upload_policy_stages_exactly_when_bulk_defers() {
    for class in ALL {
        let plan = plan_for(class, 1);
        for requested in [UploadPolicy::Full, UploadPolicy::Staged] {
            let expected = if plan.defer_bulk {
                UploadPolicy::Staged
            } else {
                requested
            };
            assert_eq!(
                plan.upload_policy(requested),
                expected,
                "{class:?} {requested:?}"
            );
        }
    }
}

/// The staged scheduler a plan's policy and class drive opens a bulk tier exactly when the plan
/// does not defer bulk work, whatever policy the user asked for; the index tier is gated only
/// on a usable path.
#[test]
fn a_plans_scheduler_opens_bulk_tiers_exactly_when_bulk_is_not_deferred() {
    use capsule_library::import::UploadTier;

    use crate::staged::StagedScheduler;

    for class in ALL {
        let plan = plan_for(class, 1);
        for requested in [UploadPolicy::Full, UploadPolicy::Staged] {
            let scheduler = StagedScheduler::new(plan.upload_policy(requested), plan.class);
            for tier in [UploadTier::Preview, UploadTier::Original] {
                assert_eq!(
                    scheduler.tier_opens_now(tier),
                    !plan.defer_bulk,
                    "{class:?} {requested:?} {tier:?}"
                );
            }
            assert_eq!(
                scheduler.tier_opens_now(UploadTier::Index),
                class != ConnectionClass::Offline,
                "{class:?} {requested:?} index"
            );
        }
    }
}

#[test]
fn sweep_error_is_returned_and_plan_kept() {
    let mut budget = CacheBudget::new(InMemoryConnectionClass::new(ConnectionClass::Adverse), 5);
    let err = budget
        .sweep(|b| -> Result<EvictionReport, i64> { Err(b) })
        .unwrap_err();
    assert_eq!(err, 15, "the sweep received the adverse budget");
    let (_, replanned) = budget.current_plan();
    assert!(!replanned, "the plan survives a failed sweep");
}

fn row(uuid: &str, bytes: i64, at: i64, pinned: bool, owned: bool) -> CachedRepresentationRow {
    CachedRepresentationRow {
        uuid: uuid.to_string(),
        tier: "original".to_string(),
        format: None,
        bytes,
        path: format!("/nonexistent/cache-budget/{uuid}"),
        last_accessed_at: at,
        pinned,
        is_owned_original: owned,
    }
}

fn survivors(db: &DatabaseDriver, uuids: &[&str]) -> Vec<String> {
    uuids
        .iter()
        .filter(|u| !db.representations_for(u).unwrap().is_empty())
        .map(|u| (*u).to_string())
        .collect()
}

/// A class change mid-session re-plans the budget the real `cache_sweep` runs under, and no plan
/// ever evicts a pinned representation or a device-owned original.
#[test]
fn class_change_mid_session_replans_without_dropping_pinned_or_owned() {
    let db = DatabaseDriver::open_in_memory().unwrap();
    // Four 100-byte reclaimable originals, newest first by access time, plus a pinned one and a
    // device-owned one that are older than all of them.
    for (uuid, at) in [("a", 40), ("b", 30), ("c", 20), ("d", 10)] {
        db.upsert_representation(&row(uuid, 100, at, false, false))
            .unwrap();
    }
    db.upsert_representation(&row("pinned", 100, 1, true, false))
        .unwrap();
    db.upsert_representation(&row("owned", 100, 2, false, true))
        .unwrap();
    let all = ["a", "b", "c", "d", "pinned", "owned"];

    // Base 100 bytes. Offline retains 4x: nothing reclaimable is evicted.
    let mut budget = CacheBudget::new(InMemoryConnectionClass::new(ConnectionClass::Offline), 100);
    let outcome = budget.sweep(|b| cache_sweep(&db, b)).unwrap();
    assert!(outcome.replanned);
    assert_eq!(outcome.plan.budget_bytes, 400);
    assert!(outcome.report.evicted.is_empty());
    assert_eq!(survivors(&db, &all), all);

    // Metered retains 2x: the two least-recently-accessed reclaimable rows go.
    budget.source().set(ConnectionClass::Metered);
    let outcome = budget.sweep(|b| cache_sweep(&db, b)).unwrap();
    assert!(outcome.replanned);
    assert_eq!(outcome.plan.budget_bytes, 200);
    assert_eq!(outcome.report.bytes_reclaimed, 200);
    assert_eq!(survivors(&db, &all), ["a", "b", "pinned", "owned"]);

    // Same class again: no re-plan, nothing more to reclaim.
    let outcome = budget.sweep(|b| cache_sweep(&db, b)).unwrap();
    assert!(!outcome.replanned);
    assert!(outcome.report.evicted.is_empty());

    // Unmetered keeps only the base: one more goes, the exempt rows stay.
    budget.source().set(ConnectionClass::Unmetered);
    let outcome = budget.sweep(|b| cache_sweep(&db, b)).unwrap();
    assert!(outcome.replanned);
    assert!(!outcome.plan.defer_bulk);
    assert_eq!(survivors(&db, &all), ["a", "pinned", "owned"]);
}
