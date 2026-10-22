//! `capsule cache`: the local cache held within the connection-class budget (SSoT:
//! design/filesystem/client § Automatic cache management).
//!
//! The policy and its orchestration are `capsule-sdk::cache_budget`'s; this module is the
//! reference client's wiring of them:
//!
//! - [`CacheSettingsStore`] — the user's **base budget** setting, persisted in the CLI's
//!   `settings.json`, defaulting to [`DEFAULT_BASE_CACHE_BUDGET`] while unset.
//! - [`sweep`] — one eviction sweep through [`CacheBudget::sweep`], rendered as the
//!   [`SweepReport`] `capsule cache sweep --json` prints.
//! - [`transfer_plan`] and [`push_options`] — the bulk-transfer half: a push samples the class
//!   through the same port, and a plan that defers bulk work stages the push so its preview and
//!   original tiers wait for an unmetered link ([`CachePlan::upload_policy`]).
//!
//! The class source is a parameter everywhere, so tests drive every class through
//! [`InMemoryConnectionClass`](capsule_sdk::cache_budget::InMemoryConnectionClass) while the
//! binary passes [`DesktopConnectionClass`](capsule_sdk::cache_budget::DesktopConnectionClass).

use std::path::{Path, PathBuf};

use capsule_library::import::UploadPolicy;
use capsule_library::library::EvictionReport;
use capsule_sdk::cache_budget::{
    CacheBudget, CachePlan, ConnectionClassSource, DEFAULT_BASE_CACHE_BUDGET, plan_for,
};
use capsule_sdk::net::ConnectionClass;
use serde::Serialize;
use serde_json::{Map, Value};
use thiserror::Error;
use tracing::{debug, info, instrument};

use crate::remote;

/// The CLI settings file, beside `session.json` in the config directory.
pub const SETTINGS_FILE_NAME: &str = "settings.json";

/// The settings key holding the user's base cache budget, in bytes.
pub const BASE_BUDGET_KEY: &str = "cache_base_budget_bytes";

/// Everything the settings store can fail with.
#[derive(Debug, Error)]
pub enum SettingsError {
    /// Reading or writing the settings file failed.
    #[error("settings I/O error at {path}: {source}")]
    Io {
        /// The offending path.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The settings file exists but is not a JSON object, or its base budget is not a
    /// non-negative integer.
    #[error("malformed settings file at {path}: {reason}")]
    Malformed {
        /// The offending path.
        path: String,
        /// What was wrong with it.
        reason: String,
    },
}

/// Where the base budget a plan scaled from came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetSource {
    /// No setting: [`DEFAULT_BASE_CACHE_BUDGET`].
    Default,
    /// The user's `cache budget --set` value.
    Setting,
}

/// The user's base cache budget and where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BaseBudget {
    /// The base budget in bytes, before the connection-class multiplier.
    pub bytes: u64,
    /// Whether it is the user's setting or the default.
    pub source: BudgetSource,
}

/// The file-backed store for the CLI's settings, of which this module owns one key.
///
/// The file is a JSON object; keys this module does not own are preserved verbatim on every
/// write, so a later setting can share the file without a migration.
#[derive(Debug)]
pub struct CacheSettingsStore {
    path: PathBuf,
}

impl CacheSettingsStore {
    /// A store backed by `path` (typically `<config_dir>/settings.json`).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The backing file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The base budget: the user's setting, or [`DEFAULT_BASE_CACHE_BUDGET`] when none is
    /// stored (including when the file does not exist).
    pub fn base_budget(&self) -> Result<BaseBudget, SettingsError> {
        let settings = self.load()?;
        let budget = match settings.get(BASE_BUDGET_KEY) {
            None | Some(Value::Null) => BaseBudget {
                bytes: DEFAULT_BASE_CACHE_BUDGET,
                source: BudgetSource::Default,
            },
            Some(value) => BaseBudget {
                bytes: value.as_u64().ok_or_else(|| {
                    self.malformed(format!(
                        "`{BASE_BUDGET_KEY}` must be a non-negative integer, found {value}"
                    ))
                })?,
                source: BudgetSource::Setting,
            },
        };
        debug!(path = %self.path.display(), bytes = budget.bytes, source = ?budget.source, "read the base cache budget");
        Ok(budget)
    }

    /// Store `bytes` as the base budget, or clear the setting back to the default with `None`.
    /// Every other key in the file is kept.
    #[instrument(level = "debug", skip(self), fields(path = %self.path.display()))]
    pub fn set_base_budget(&self, bytes: Option<u64>) -> Result<(), SettingsError> {
        let mut settings = self.load()?;
        match bytes {
            Some(bytes) => {
                settings.insert(BASE_BUDGET_KEY.to_owned(), Value::from(bytes));
            }
            None => {
                settings.remove(BASE_BUDGET_KEY);
            }
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| self.io_error(source))?;
        }
        let json = serde_json::to_vec_pretty(&Value::Object(settings))
            .map_err(|error| self.malformed(error.to_string()))?;
        std::fs::write(&self.path, json).map_err(|source| self.io_error(source))?;
        info!(bytes = ?bytes, "base cache budget setting written");
        Ok(())
    }

    /// The settings object, empty when the file does not exist.
    fn load(&self) -> Result<Map<String, Value>, SettingsError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Map::new()),
            Err(source) => return Err(self.io_error(source)),
        };
        match serde_json::from_slice(&bytes) {
            Ok(Value::Object(settings)) => Ok(settings),
            Ok(other) => Err(self.malformed(format!("expected a JSON object, found {other}"))),
            Err(error) => Err(self.malformed(error.to_string())),
        }
    }

    fn io_error(&self, source: std::io::Error) -> SettingsError {
        SettingsError::Io {
            path: self.path.display().to_string(),
            source,
        }
    }

    fn malformed(&self, reason: String) -> SettingsError {
        SettingsError::Malformed {
            path: self.path.display().to_string(),
            reason,
        }
    }
}

/// The connection class as `--json` spells it: the networking table's names.
#[must_use]
pub const fn class_name(class: ConnectionClass) -> &'static str {
    match class {
        ConnectionClass::Unmetered => "unmetered",
        ConnectionClass::Metered => "metered",
        ConnectionClass::Constrained => "constrained",
        ConnectionClass::Adverse => "adverse",
        ConnectionClass::Offline => "offline",
    }
}

/// One cache plan, as `capsule cache budget --json` prints it and `capsule cache sweep --json`
/// embeds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanReport {
    /// The connection class the plan was made for.
    pub connection_class: &'static str,
    /// The user's base budget, before the class multiplier.
    pub base_budget_bytes: u64,
    /// Whether the base budget is the user's setting or the default.
    pub base_budget_source: BudgetSource,
    /// The reclaimable bytes retained on this class.
    pub budget_bytes: u64,
    /// Whether bulk transfers wait for an unmetered link on this class.
    pub defer_bulk: bool,
}

impl PlanReport {
    /// The report for `plan`, scaled from `base`.
    #[must_use]
    pub fn new(plan: &CachePlan, base: BaseBudget) -> Self {
        Self {
            connection_class: class_name(plan.class),
            base_budget_bytes: base.bytes,
            base_budget_source: base.source,
            budget_bytes: plan.budget_bytes,
            defer_bulk: plan.defer_bulk,
        }
    }
}

/// One evicted representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EvictedRepresentation {
    /// The asset the representation belongs to.
    pub asset_id: String,
    /// Its tier (`thumbnail`, `preview` or `original`).
    pub tier: String,
    /// The bytes its eviction reclaimed.
    pub bytes: i64,
}

/// What one `capsule cache sweep` did: the `--json` document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SweepReport {
    /// The plan the sweep ran under.
    pub plan: PlanReport,
    /// How many representations were evicted.
    pub evicted_count: usize,
    /// Total bytes reclaimed.
    pub bytes_reclaimed: i64,
    /// The evicted representations, in the order the sweep reclaimed them.
    pub evicted: Vec<EvictedRepresentation>,
}

impl SweepReport {
    fn new(plan: &CachePlan, base: BaseBudget, report: EvictionReport) -> Self {
        let evicted: Vec<EvictedRepresentation> = report
            .evicted
            .into_iter()
            .map(|row| EvictedRepresentation {
                asset_id: row.uuid,
                tier: row.tier,
                bytes: row.bytes,
            })
            .collect();
        Self {
            plan: PlanReport::new(plan, base),
            evicted_count: evicted.len(),
            bytes_reclaimed: report.bytes_reclaimed,
            evicted,
        }
    }
}

/// The plan the current class allows from `base`, without sweeping.
pub fn current_plan<S: ConnectionClassSource>(source: S, base: BaseBudget) -> PlanReport {
    let (plan, _) = CacheBudget::new(source, base.bytes).current_plan();
    PlanReport::new(&plan, base)
}

/// Run one eviction sweep under the budget `source`'s class allows from `base`.
/// `run_sweep` is the sweep itself, normally
/// `|budget| capsule_library::library::cache_sweep(&library.db, budget)`.
///
/// # Errors
///
/// Whatever the sweep returns.
#[instrument(level = "debug", skip(source, run_sweep), fields(base = base.bytes))]
pub fn sweep<S: ConnectionClassSource, E>(
    source: S,
    base: BaseBudget,
    run_sweep: impl FnOnce(i64) -> Result<EvictionReport, E>,
) -> Result<SweepReport, E> {
    let outcome = CacheBudget::new(source, base.bytes).sweep(run_sweep)?;
    let report = SweepReport::new(&outcome.plan, base, outcome.report);
    info!(
        class = report.plan.connection_class,
        budget_bytes = report.plan.budget_bytes,
        evicted = report.evicted_count,
        bytes_reclaimed = report.bytes_reclaimed,
        "cache sweep complete"
    );
    Ok(report)
}

/// The plan a transfer schedules under: the class `source` reports now. Only the class and its
/// bulk deferral matter to a transfer, so the byte budget is planned from the default base and
/// no settings file is read.
pub fn transfer_plan<S: ConnectionClassSource>(source: &S) -> CachePlan {
    let plan = plan_for(source.current(), DEFAULT_BASE_CACHE_BUDGET);
    debug!(
        class = class_name(plan.class),
        defer_bulk = plan.defer_bulk,
        "sampled the transfer plan"
    );
    plan
}

/// The push configuration for the CLI's push flags under `plan`.
///
/// The push runs on the plan's class, and a plan that defers bulk work stages the push whatever
/// `staged` says: the index tier still publishes each asset, while the preview and original
/// tiers wait for an unmetered link through the staged scheduler's tier gate.
#[must_use]
pub fn push_options(
    plan: &CachePlan,
    dry_run: bool,
    force: bool,
    staged: bool,
) -> remote::PushOptions {
    let requested = if staged {
        UploadPolicy::Staged
    } else {
        UploadPolicy::Full
    };
    let policy = plan.upload_policy(requested);
    if policy != requested {
        info!(
            class = class_name(plan.class),
            "bulk transfers are deferred on this connection: staging the push"
        );
    }
    remote::PushOptions {
        dry_run,
        force,
        policy,
        connection: plan.class,
    }
}

#[cfg(test)]
mod tests;
