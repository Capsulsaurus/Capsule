use capsule_library::db::{CachedRepresentationRow, DatabaseDriver};
use capsule_library::import::UploadTier;
use capsule_library::library::cache_sweep;
use capsule_sdk::cache_budget::InMemoryConnectionClass;
use capsule_sdk::staged::StagedScheduler;

use super::*;

const ALL: [ConnectionClass; 5] = [
    ConnectionClass::Unmetered,
    ConnectionClass::Metered,
    ConnectionClass::Constrained,
    ConnectionClass::Adverse,
    ConnectionClass::Offline,
];

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-cli-cache-{}", nanoid::nanoid!()));
        std::fs::create_dir_all(&path).expect("create scratch dir");
        Self(path)
    }

    fn settings(&self) -> CacheSettingsStore {
        CacheSettingsStore::new(self.0.join("config").join(SETTINGS_FILE_NAME))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn setting(bytes: u64) -> BaseBudget {
    BaseBudget {
        bytes,
        source: BudgetSource::Setting,
    }
}

// ─── The base budget setting ──────────────────────────────────────────────────

#[test]
fn a_missing_settings_file_yields_the_default_budget() {
    let scratch = Scratch::new();
    assert_eq!(
        scratch.settings().base_budget().expect("read"),
        BaseBudget {
            bytes: DEFAULT_BASE_CACHE_BUDGET,
            source: BudgetSource::Default,
        }
    );
}

#[test]
fn a_set_budget_is_read_back_and_a_reset_restores_the_default() {
    let scratch = Scratch::new();
    let store = scratch.settings();
    store.set_base_budget(Some(4096)).expect("set");
    assert_eq!(store.base_budget().expect("read"), setting(4096));

    store.set_base_budget(Some(0)).expect("set zero");
    assert_eq!(store.base_budget().expect("read"), setting(0));

    store.set_base_budget(None).expect("reset");
    assert_eq!(
        store.base_budget().expect("read").source,
        BudgetSource::Default
    );
}

#[test]
fn writing_the_budget_keeps_every_other_setting() {
    let scratch = Scratch::new();
    let store = scratch.settings();
    std::fs::create_dir_all(store.path().parent().expect("parent")).expect("mkdir");
    std::fs::write(store.path(), br#"{"other": {"kept": true}}"#).expect("seed");

    store.set_base_budget(Some(10)).expect("set");
    store.set_base_budget(None).expect("reset");

    let written: Value =
        serde_json::from_slice(&std::fs::read(store.path()).expect("read")).expect("json");
    assert_eq!(written, serde_json::json!({"other": {"kept": true}}));
}

#[test]
fn a_malformed_budget_is_refused_not_defaulted() {
    let scratch = Scratch::new();
    let store = scratch.settings();
    std::fs::create_dir_all(store.path().parent().expect("parent")).expect("mkdir");
    for bad in [
        r#"{"cache_base_budget_bytes": -1}"#,
        r#"{"cache_base_budget_bytes": "2GiB"}"#,
        "[1]",
        "not json",
    ] {
        std::fs::write(store.path(), bad).expect("seed");
        assert!(
            matches!(store.base_budget(), Err(SettingsError::Malformed { .. })),
            "{bad} was accepted"
        );
    }
}

/// A file that is not a settings object is never overwritten, since it may hold settings this
/// module cannot read; a bad budget inside a good object is simply replaced by a new one.
#[test]
fn setting_the_budget_refuses_to_overwrite_an_unreadable_file() {
    let scratch = Scratch::new();
    let store = scratch.settings();
    std::fs::create_dir_all(store.path().parent().expect("parent")).expect("mkdir");
    for bad in ["[1]", "not json"] {
        std::fs::write(store.path(), bad).expect("seed");
        assert!(
            matches!(
                store.set_base_budget(Some(1)),
                Err(SettingsError::Malformed { .. })
            ),
            "{bad} was overwritten"
        );
        assert_eq!(std::fs::read_to_string(store.path()).expect("read"), bad);
    }
    std::fs::write(store.path(), r#"{"cache_base_budget_bytes": -1}"#).expect("seed");
    store
        .set_base_budget(Some(1))
        .expect("replace the bad value");
    assert_eq!(store.base_budget().expect("read"), setting(1));
}

// ─── The sweep ────────────────────────────────────────────────────────────────

fn row(uuid: &str, bytes: i64, at: i64) -> CachedRepresentationRow {
    CachedRepresentationRow {
        uuid: uuid.to_string(),
        tier: "preview".to_string(),
        format: None,
        bytes,
        path: format!("/nonexistent/capsule-cli-cache/{uuid}"),
        last_accessed_at: at,
        pinned: false,
        is_owned_original: false,
    }
}

/// Five 100-byte reclaimable previews, `a` the most recently accessed.
fn seeded() -> DatabaseDriver {
    let db = DatabaseDriver::open_in_memory().expect("db");
    for (uuid, at) in [("a", 50), ("b", 40), ("c", 30), ("d", 20), ("e", 10)] {
        db.upsert_representation(&row(uuid, 100, at)).expect("seed");
    }
    db
}

/// The sweep runs under the configured base budget scaled by the class multiplier: 100 bytes
/// retains one preview on `unmetered`, two on `metered`, three on `adverse`, four `offline`.
#[test]
fn the_sweep_honours_the_configured_base_budget_and_the_class_multiplier() {
    for (class, retained) in [
        (ConnectionClass::Unmetered, 1),
        (ConnectionClass::Metered, 2),
        (ConnectionClass::Constrained, 2),
        (ConnectionClass::Adverse, 3),
        (ConnectionClass::Offline, 4),
    ] {
        let db = seeded();
        let report = sweep(
            InMemoryConnectionClass::new(class),
            setting(100),
            |budget| cache_sweep(&db, budget),
        )
        .expect("sweep");
        assert_eq!(report.plan.connection_class, class_name(class));
        assert_eq!(report.plan.base_budget_bytes, 100);
        assert_eq!(report.plan.base_budget_source, BudgetSource::Setting);
        assert_eq!(report.plan.budget_bytes, 100 * retained, "{class:?}");
        assert_eq!(report.evicted_count, 5 - retained as usize, "{class:?}");
        assert_eq!(report.bytes_reclaimed, 100 * (5 - retained as i64));
        // The least-recently-accessed previews go, reported in the sweep's own order.
        let evicted: Vec<&str> = report.evicted.iter().map(|e| e.asset_id.as_str()).collect();
        let expected = &["b", "c", "d", "e"][retained as usize - 1..];
        assert_eq!(evicted, expected, "{class:?}");
    }
}

/// The same library swept with two different settings evicts down to each one.
#[test]
fn a_larger_setting_retains_more() {
    let db = seeded();
    let unmetered = || InMemoryConnectionClass::new(ConnectionClass::Unmetered);
    let report = sweep(unmetered(), setting(400), |b| cache_sweep(&db, b)).expect("sweep");
    assert_eq!(report.evicted_count, 1);
    let report = sweep(unmetered(), setting(200), |b| cache_sweep(&db, b)).expect("sweep");
    assert_eq!(report.evicted_count, 2);
}

#[test]
fn a_sweep_error_is_returned() {
    let error = sweep(
        InMemoryConnectionClass::new(ConnectionClass::Adverse),
        setting(7),
        |budget| -> Result<EvictionReport, i64> { Err(budget) },
    )
    .expect_err("the sweep failed");
    assert_eq!(error, 21, "the sweep received the adverse budget");
}

#[test]
fn the_plan_report_names_the_class_and_its_deferral() {
    for class in ALL {
        let report = current_plan(InMemoryConnectionClass::new(class), setting(10));
        assert_eq!(report.connection_class, class_name(class));
        assert_eq!(report.budget_bytes, class.cache_retention_budget(10));
        assert_eq!(report.defer_bulk, class != ConnectionClass::Unmetered);
    }
}

#[test]
fn the_sweep_report_serializes_its_contract_fields() {
    let db = seeded();
    let report = sweep(
        InMemoryConnectionClass::new(ConnectionClass::Metered),
        BaseBudget {
            bytes: 150,
            source: BudgetSource::Default,
        },
        |b| cache_sweep(&db, b),
    )
    .expect("sweep");
    assert_eq!(
        serde_json::to_value(&report).expect("json"),
        serde_json::json!({
            "plan": {
                "connection_class": "metered",
                "base_budget_bytes": 150,
                "base_budget_source": "default",
                "budget_bytes": 300,
                "defer_bulk": true,
            },
            "evicted_count": 2,
            "bytes_reclaimed": 200,
            "evicted": [
                {"asset_id": "d", "tier": "preview", "bytes": 100},
                {"asset_id": "e", "tier": "preview", "bytes": 100},
            ],
        })
    );
}

// ─── Bulk scheduling ──────────────────────────────────────────────────────────

/// A push sampled through the in-memory adapter on any class but `unmetered` defers its bulk
/// tiers: even a full push is staged, and the scheduler it drives opens no preview or original
/// session, while the index tier still opens on every usable link.
#[test]
fn a_push_defers_bulk_tiers_off_unmetered() {
    for class in ALL {
        let source = InMemoryConnectionClass::new(class);
        for staged in [false, true] {
            let options = push_options(&transfer_plan(&source), true, false, staged);
            assert_eq!(options.connection, class);
            let scheduler = StagedScheduler::new(options.policy, options.connection);
            let bulk_opens = class == ConnectionClass::Unmetered;
            assert_eq!(
                scheduler.tier_opens_now(UploadTier::Original),
                bulk_opens,
                "{class:?} staged={staged}"
            );
            assert_eq!(
                scheduler.tier_opens_now(UploadTier::Preview),
                bulk_opens,
                "{class:?} staged={staged}"
            );
            assert_eq!(
                scheduler.tier_opens_now(UploadTier::Index),
                class != ConnectionClass::Offline,
                "{class:?} staged={staged}"
            );
        }
    }
}

/// On `unmetered` the user's choice stands, and the flags pass through.
#[test]
fn an_unmetered_push_keeps_the_requested_policy() {
    let plan = transfer_plan(&InMemoryConnectionClass::new(ConnectionClass::Unmetered));
    let full = push_options(&plan, true, true, false);
    assert_eq!(full.policy, UploadPolicy::Full);
    assert!(full.dry_run && full.force);
    assert_eq!(
        push_options(&plan, false, false, true).policy,
        UploadPolicy::Staged
    );
}

/// A class change between samples is seen by the next transfer.
#[test]
fn the_transfer_plan_follows_the_current_class() {
    let source = InMemoryConnectionClass::new(ConnectionClass::Unmetered);
    assert!(!transfer_plan(&source).defer_bulk);
    source.set(ConnectionClass::Constrained);
    let plan = transfer_plan(&source);
    assert_eq!(plan.class, ConnectionClass::Constrained);
    assert!(plan.defer_bulk);
}
