use super::*;

fn snapshot(sample: PathSample) -> PathSnapshot {
    PathSnapshot {
        sample,
        generation: PathGeneration(7),
    }
}

fn sample() -> PathSample {
    PathSample::desktop()
}

const CALM: LinkBehaviour = LinkBehaviour { adverse: false };
const ADVERSE: LinkBehaviour = LinkBehaviour { adverse: true };

fn bounds() -> LaneWindows {
    LaneWindows::new(4, 4)
}

/// One golden row: the inputs that differ from the desktop default, and the plan's verdict.
struct Row {
    name: &'static str,
    sample: PathSample,
    link: LinkBehaviour,
    class: ConnectionClass,
    admission: TierAdmission,
    hold: Option<Hold>,
    sessions: u32,
    chunk_target: Duration,
    range_window: Option<u64>,
}

fn with(f: impl FnOnce(&mut PathSample)) -> PathSample {
    let mut s = sample();
    f(&mut s);
    s
}

fn golden_rows() -> Vec<Row> {
    let normal = TARGET_REQUEST_DURATION;
    let short = ADVERSE_TARGET_REQUEST_DURATION;
    let window = Some(crate::net::ADVERSE_RANGE_WINDOW);
    vec![
        Row {
            name: "desktop default runs everything",
            sample: sample(),
            link: CALM,
            class: ConnectionClass::Unmetered,
            admission: TierAdmission::All,
            hold: None,
            sessions: 4,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "metered holds bulk",
            sample: with(|s| s.cost = PathCost::Metered),
            link: CALM,
            class: ConnectionClass::Metered,
            admission: TierAdmission::IndexOnly,
            hold: Some(Hold::Cost),
            sessions: 4,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "data saver holds bulk",
            sample: with(|s| s.cost = PathCost::Constrained),
            link: CALM,
            class: ConnectionClass::Constrained,
            admission: TierAdmission::IndexOnly,
            hold: Some(Hold::Cost),
            sessions: 4,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "unreachable runs nothing",
            sample: with(|s| s.reachability = Reachability::Unreachable),
            link: CALM,
            class: ConnectionClass::Offline,
            admission: TierAdmission::Nothing,
            hold: Some(Hold::Offline),
            sessions: 4,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "unreachable is never promoted to adverse",
            sample: with(|s| s.reachability = Reachability::Unreachable),
            link: ADVERSE,
            class: ConnectionClass::Offline,
            admission: TierAdmission::Nothing,
            hold: Some(Hold::Offline),
            sessions: 4,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "adverse link holds bulk and shortens requests",
            sample: sample(),
            link: ADVERSE,
            class: ConnectionClass::Adverse,
            admission: TierAdmission::IndexOnly,
            hold: Some(Hold::Adverse),
            sessions: 4,
            chunk_target: short,
            range_window: window,
        },
        Row {
            name: "adverse over metered is adverse",
            sample: with(|s| s.cost = PathCost::Metered),
            link: ADVERSE,
            class: ConnectionClass::Adverse,
            admission: TierAdmission::IndexOnly,
            hold: Some(Hold::Adverse),
            sessions: 4,
            chunk_target: short,
            range_window: window,
        },
        Row {
            name: "low-power mode holds bulk on one session",
            sample: with(|s| s.power = PowerState::LowPower),
            link: CALM,
            class: ConnectionClass::Unmetered,
            admission: TierAdmission::IndexOnly,
            hold: Some(Hold::LowPower),
            sessions: 1,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "battery alone changes nothing",
            sample: with(|s| s.power = PowerState::Battery),
            link: CALM,
            class: ConnectionClass::Unmetered,
            admission: TierAdmission::All,
            hold: None,
            sessions: 4,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "fair thermal changes nothing",
            sample: with(|s| s.thermal = ThermalState::Fair),
            link: CALM,
            class: ConnectionClass::Unmetered,
            admission: TierAdmission::All,
            hold: None,
            sessions: 4,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "serious thermal holds bulk on one session",
            sample: with(|s| s.thermal = ThermalState::Serious),
            link: CALM,
            class: ConnectionClass::Unmetered,
            admission: TierAdmission::IndexOnly,
            hold: Some(Hold::ThermalSerious),
            sessions: 1,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "critical thermal runs nothing",
            sample: with(|s| s.thermal = ThermalState::Critical),
            link: CALM,
            class: ConnectionClass::Unmetered,
            admission: TierAdmission::Nothing,
            hold: Some(Hold::ThermalCritical),
            sessions: 1,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "unreachable outranks critical thermal",
            sample: with(|s| {
                s.reachability = Reachability::Unreachable;
                s.thermal = ThermalState::Critical;
            }),
            link: CALM,
            class: ConnectionClass::Offline,
            admission: TierAdmission::Nothing,
            hold: Some(Hold::Offline),
            sessions: 1,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "a short background deadline holds bulk",
            sample: with(|s| {
                s.execution = Execution::Background {
                    deadline: Duration::from_secs(30),
                };
            }),
            link: CALM,
            class: ConnectionClass::Unmetered,
            admission: TierAdmission::IndexOnly,
            hold: Some(Hold::Deadline),
            sessions: 4,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "a background deadline at the minimum runs everything",
            sample: with(|s| {
                s.execution = Execution::Background {
                    deadline: MIN_BULK_DEADLINE,
                };
            }),
            link: CALM,
            class: ConnectionClass::Unmetered,
            admission: TierAdmission::All,
            hold: None,
            sessions: 4,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "cost outranks low power as the hold",
            sample: with(|s| {
                s.cost = PathCost::Metered;
                s.power = PowerState::LowPower;
            }),
            link: CALM,
            class: ConnectionClass::Metered,
            admission: TierAdmission::IndexOnly,
            hold: Some(Hold::Cost),
            sessions: 1,
            chunk_target: normal,
            range_window: None,
        },
        Row {
            name: "the interface alone changes nothing",
            sample: with(|s| s.interface = PathInterface::Cellular),
            link: CALM,
            class: ConnectionClass::Unmetered,
            admission: TierAdmission::All,
            hold: None,
            sessions: 4,
            chunk_target: normal,
            range_window: None,
        },
    ]
}

#[test]
fn the_plan_matches_every_golden_row() {
    for row in golden_rows() {
        let plan = plan_transfer(
            &snapshot(row.sample),
            row.link,
            HeldAdvice::none(),
            bounds(),
        );
        let expected = TransferPlan {
            generation: PathGeneration(7),
            class: row.class,
            admission: row.admission,
            hold: row.hold,
            windows: LaneWindows::new(row.sessions, 4),
            chunk_target: row.chunk_target,
            suggested_chunk: None,
            range_window: row.range_window,
        };
        assert_eq!(plan, expected, "row: {}", row.name);
    }
}

#[test]
fn the_plan_is_pure() {
    for row in golden_rows() {
        let first = plan_transfer(
            &snapshot(row.sample),
            row.link,
            HeldAdvice::none(),
            bounds(),
        );
        let second = plan_transfer(
            &snapshot(row.sample),
            row.link,
            HeldAdvice::none(),
            bounds(),
        );
        assert_eq!(first, second, "row: {}", row.name);
    }
}

#[test]
fn fresh_advice_only_lowers_the_windows() {
    let advice = HeldAdvice {
        advice: ServerAdvice::parse(Some("sessions=2, ranges=1, chunk=1048576, ttl=60")),
        age: Duration::from_secs(10),
    };
    let plan = plan_transfer(&snapshot(sample()), CALM, advice, bounds());
    assert_eq!(plan.windows, LaneWindows::new(2, 1));
    assert_eq!(plan.suggested_chunk, Some(1_048_576));

    // Advice above the client's own bounds cannot raise them.
    let generous = HeldAdvice {
        advice: ServerAdvice::parse(Some("sessions=16, ranges=16")),
        age: Duration::ZERO,
    };
    let plan = plan_transfer(&snapshot(sample()), CALM, generous, LaneWindows::new(3, 2));
    assert_eq!(plan.windows, LaneWindows::new(3, 2));
}

#[test]
fn stale_advice_falls_back_to_the_bounds_except_ranges() {
    let stale = HeldAdvice {
        advice: ServerAdvice::parse(Some("sessions=2, ranges=1, chunk=1048576, ttl=60")),
        age: Duration::from_secs(61),
    };
    let plan = plan_transfer(&snapshot(sample()), CALM, stale, bounds());
    // `ranges` holds for the session's life; `sessions` and `chunk` expire with the ttl.
    assert_eq!(plan.windows, LaneWindows::new(4, 1));
    assert_eq!(plan.suggested_chunk, None);
}

#[test]
fn force_sync_lifts_only_cost_and_adverse_holds() {
    let lifts = [
        (with(|s| s.cost = PathCost::Metered), CALM, true),
        (sample(), ADVERSE, true),
        (with(|s| s.power = PowerState::LowPower), CALM, false),
        (with(|s| s.thermal = ThermalState::Serious), CALM, false),
        (
            with(|s| {
                s.execution = Execution::Background {
                    deadline: Duration::from_secs(5),
                };
            }),
            CALM,
            false,
        ),
        (with(|s| s.thermal = ThermalState::Critical), CALM, false),
        (
            with(|s| s.reachability = Reachability::Unreachable),
            CALM,
            false,
        ),
    ];
    for (sample, link, lifted) in lifts {
        let plan = plan_transfer(&snapshot(sample), link, HeldAdvice::none(), bounds());
        assert_eq!(
            plan.permits_tier(UploadTier::Original, true),
            lifted,
            "{sample:?} {link:?}"
        );
        assert!(!plan.permits_tier(UploadTier::Original, false) || plan.hold.is_none());
    }
}

#[test]
fn a_plan_that_holds_bulk_stages_the_push() {
    let metered = plan_transfer(
        &snapshot(with(|s| s.cost = PathCost::Metered)),
        CALM,
        HeldAdvice::none(),
        bounds(),
    );
    assert_eq!(
        metered.upload_policy(UploadPolicy::Full),
        UploadPolicy::Staged
    );
    let open = plan_transfer(&snapshot(sample()), CALM, HeldAdvice::none(), bounds());
    assert_eq!(open.upload_policy(UploadPolicy::Full), UploadPolicy::Full);
    assert_eq!(
        open.upload_policy(UploadPolicy::Staged),
        UploadPolicy::Staged
    );
}

#[test]
fn from_class_round_trips_every_reported_class() {
    for class in [
        ConnectionClass::Unmetered,
        ConnectionClass::Metered,
        ConnectionClass::Constrained,
        ConnectionClass::Offline,
    ] {
        assert_eq!(PathSample::from_class(class).base_class(), class);
    }
    // Adverse is behavioural: no OS reports it.
    assert_eq!(
        PathSample::from_class(ConnectionClass::Adverse).base_class(),
        ConnectionClass::Unmetered
    );
}

#[test]
fn the_class_admission_is_the_class_tier_gate() {
    for class in [
        ConnectionClass::Unmetered,
        ConnectionClass::Metered,
        ConnectionClass::Constrained,
        ConnectionClass::Adverse,
        ConnectionClass::Offline,
    ] {
        let (admission, hold) = TierAdmission::for_class(class);
        for tier in [UploadTier::Index, UploadTier::Preview, UploadTier::Original] {
            for force_sync in [false, true] {
                assert_eq!(
                    admission.permits(hold, tier, force_sync),
                    class.permits_tier(tier, force_sync),
                    "{class:?} {tier:?} force_sync={force_sync}"
                );
            }
        }
    }
}

// ─── Generations ─────────────────────────────────────────────────────────────

#[test]
fn only_reachability_interface_or_cost_is_a_new_path() {
    let base = sample();
    let new_paths = [
        with(|s| s.cost = PathCost::Metered),
        with(|s| s.reachability = Reachability::Unreachable),
        with(|s| s.interface = PathInterface::Wifi),
    ];
    for other in new_paths {
        assert!(base.is_new_path(&other), "{other:?}");
    }
    let minor = [
        with(|s| s.power = PowerState::LowPower),
        with(|s| s.thermal = ThermalState::Critical),
        with(|s| {
            s.execution = Execution::Background {
                deadline: Duration::from_secs(1),
            };
        }),
    ];
    for other in minor {
        assert!(!base.is_new_path(&other), "{other:?}");
    }
}

fn adverse_now(monitor: &PathMonitor) {
    for _ in 0..3 {
        monitor.record(TransferSignal::Reset);
    }
    assert_eq!(monitor.plan().class, ConnectionClass::Adverse);
}

#[test]
fn a_new_path_advances_the_generation_and_resets_the_detector() {
    let monitor = PathMonitor::new(sample(), bounds());
    assert_eq!(monitor.snapshot().generation, PathGeneration(1));
    adverse_now(&monitor);

    let update = monitor.observe(with(|s| s.interface = PathInterface::Cellular));
    let PathUpdate::NewPath(plan) = update else {
        panic!("an interface change is a new path: {update:?}");
    };
    assert_eq!(plan.generation, PathGeneration(2));
    assert_eq!(
        plan.class,
        ConnectionClass::Unmetered,
        "the old path's resets do not carry over"
    );
}

#[test]
fn a_minor_update_keeps_the_generation_and_the_detector() {
    let monitor = PathMonitor::new(sample(), bounds());
    adverse_now(&monitor);

    let update = monitor.observe(with(|s| s.thermal = ThermalState::Fair));
    assert!(matches!(update, PathUpdate::SamePath(_)), "{update:?}");
    assert_eq!(update.plan().generation, PathGeneration(1));
    assert_eq!(update.plan().class, ConnectionClass::Adverse);
}

#[test]
fn subscribers_wake_only_when_the_plan_changes() {
    let monitor = PathMonitor::new(sample(), bounds());
    let mut plans = monitor.subscribe();
    plans.mark_unchanged();

    // The same sample again: no change, no wake.
    monitor.observe(sample());
    assert!(!plans.has_changed().unwrap());

    monitor.observe(with(|s| s.cost = PathCost::Metered));
    assert!(plans.has_changed().unwrap());
    let plan = *plans.borrow_and_update();
    assert_eq!(plan.admission, TierAdmission::IndexOnly);
    assert_eq!(plan.generation, PathGeneration(2));
}

#[test]
fn plan_with_applies_advice_to_the_current_path() {
    let monitor = PathMonitor::new(sample(), bounds());
    let advice = HeldAdvice {
        advice: ServerAdvice::parse(Some("sessions=1")),
        age: Duration::ZERO,
    };
    assert_eq!(monitor.plan_with(advice).windows.sessions, 1);
    assert_eq!(monitor.plan().windows.sessions, 4);
}

// ─── Pools ───────────────────────────────────────────────────────────────────

#[test]
fn a_new_generation_builds_a_new_client() {
    let built = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = built.clone();
    let pool = GenerationPool::new(move || {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        crate::net::http_client().expect("client builds")
    });
    assert_eq!(pool.generation(), None);

    let _ = pool.client(PathGeneration(1));
    let _ = pool.client(PathGeneration(1));
    assert_eq!(built.load(std::sync::atomic::Ordering::SeqCst), 1);

    let _ = pool.client(PathGeneration(2));
    assert_eq!(built.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(pool.generation(), Some(PathGeneration(2)));
}
