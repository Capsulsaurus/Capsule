//! The client cache budget driven by the connection class (SSoT: design/filesystem/client §
//! Automatic cache management, and design/networking § Connection Classes).
//!
//! Three pieces, each with one job:
//!
//! - [`ConnectionClassSource`] — the **port** a client samples its current [`ConnectionClass`]
//!   through. [`InMemoryConnectionClass`] is the deterministic adapter tests (and any caller that
//!   learns the class out of band, such as a mobile shell pushing OS callbacks) set by hand;
//!   [`DesktopConnectionClass`] is the desktop adapter, which classifies whatever [`PathSignals`]
//!   its probe can read and reports `unmetered` when it can tell nothing.
//! - [`plan_for`] — the **pure policy**: a class and the user's base budget in, a [`CachePlan`]
//!   (the retention byte budget plus whether bulk transfers defer) out.
//! - [`CacheBudget`] — the **orchestration** that samples the port, re-plans when the class
//!   changes mid-session, and hands the planned budget to the eviction sweep
//!   ([`capsule_core::library::cache_sweep`]). The sweep is passed in as a closure so this module
//!   names no storage type; the sweep alone decides what is evictable, so pinned representations
//!   and device-owned originals survive every re-plan.

use std::sync::Mutex;

use capsule_core::import::UploadPolicy;
use capsule_core::library::EvictionReport;
use tracing::{debug, info, instrument};

use crate::net::{ConnectionClass, PathSignals};

/// The default base cache budget, 2 GiB: the reclaimable bytes an unmetered client retains
/// before the sweep evicts. The user setting overrides it; other classes scale it through
/// [`ConnectionClass::cache_retention_budget`].
pub const DEFAULT_BASE_CACHE_BUDGET: u64 = 2 * 1024 * 1024 * 1024;

/// The port a client samples its current connection class through.
pub trait ConnectionClassSource: Send + Sync {
    /// The class of the link right now.
    fn current(&self) -> ConnectionClass;
}

/// A deterministic, settable class source. Tests drive class changes through it; a platform
/// shell that is told the class by OS callbacks can push them through it as well.
#[derive(Debug)]
pub struct InMemoryConnectionClass {
    class: Mutex<ConnectionClass>,
}

impl InMemoryConnectionClass {
    /// A source that reports `class` until [`set`](Self::set) changes it.
    #[must_use]
    pub fn new(class: ConnectionClass) -> Self {
        Self {
            class: Mutex::new(class),
        }
    }

    /// Change the reported class.
    pub fn set(&self, class: ConnectionClass) {
        *self
            .class
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = class;
    }
}

impl ConnectionClassSource for InMemoryConnectionClass {
    fn current(&self) -> ConnectionClass {
        *self
            .class
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Reads the OS path signals on desktop, or `None` when the OS exposes nothing usable.
pub type PathProbe = Box<dyn Fn() -> Option<PathSignals> + Send + Sync>;

/// The desktop class source. Desktop operating systems expose no portable metered or data-saver
/// signal, so the default probe reads nothing and the source reports
/// [`ConnectionClass::Unmetered`]: a wired or Wi-Fi desktop is the networking table's
/// "OS reports not expensive / not metered" case. A platform that can read real signals installs
/// a probe with [`with_probe`](Self::with_probe), and whatever it reports is classified by
/// [`ConnectionClass::from_signals`].
pub struct DesktopConnectionClass {
    probe: PathProbe,
}

impl DesktopConnectionClass {
    /// The default desktop source: no OS probe, so always unmetered.
    #[must_use]
    pub fn new() -> Self {
        Self {
            probe: Box::new(|| None),
        }
    }

    /// A desktop source that classifies what `probe` reads, falling back to unmetered whenever it
    /// returns `None`.
    #[must_use]
    pub fn with_probe(probe: PathProbe) -> Self {
        Self { probe }
    }
}

impl Default for DesktopConnectionClass {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for DesktopConnectionClass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopConnectionClass")
            .finish_non_exhaustive()
    }
}

impl ConnectionClassSource for DesktopConnectionClass {
    fn current(&self) -> ConnectionClass {
        (self.probe)().map_or(ConnectionClass::Unmetered, ConnectionClass::from_signals)
    }
}

/// What the client does with its cache and bulk transfers on one connection class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePlan {
    /// The class the plan was made for.
    pub class: ConnectionClass,
    /// The reclaimable bytes retained before the sweep evicts.
    pub budget_bytes: u64,
    /// Whether bulk transfers (prefetch, original fetches, above-index uploads) wait for an
    /// unmetered link. Everything but `unmetered` defers, as the networking table states for
    /// `metered` and as the byte-conscious `constrained`, unreliable `adverse` and path-less
    /// `offline` classes require.
    pub defer_bulk: bool,
}

impl CachePlan {
    /// The budget in the signed width the sweep takes, saturating at `i64::MAX`.
    #[must_use]
    pub fn sweep_budget(&self) -> i64 {
        i64::try_from(self.budget_bytes).unwrap_or(i64::MAX)
    }

    /// The upload policy a push runs under on this plan, given the one the user `requested`.
    ///
    /// A plan that defers bulk transfers demotes [`UploadPolicy::Full`] to
    /// [`UploadPolicy::Staged`], so the bulk tiers (preview and original) wait for an unmetered
    /// link through the staged scheduler's tier gate while the index tier still publishes the
    /// asset. A plan that permits bulk work keeps whatever was requested.
    #[must_use]
    pub fn upload_policy(&self, requested: UploadPolicy) -> UploadPolicy {
        if self.defer_bulk {
            UploadPolicy::Staged
        } else {
            requested
        }
    }
}

/// The cache plan for `class` from the user's `base_budget_bytes`. Pure.
#[must_use]
pub fn plan_for(class: ConnectionClass, base_budget_bytes: u64) -> CachePlan {
    CachePlan {
        class,
        budget_bytes: class.cache_retention_budget(base_budget_bytes),
        defer_bulk: !class.is_non_metered(),
    }
}

/// What one [`CacheBudget::sweep`] did.
#[derive(Debug, PartialEq)]
pub struct CacheSweepOutcome {
    /// The plan the sweep ran under.
    pub plan: CachePlan,
    /// Whether the class changed since the previous sweep, forcing a re-plan.
    pub replanned: bool,
    /// What the sweep reclaimed.
    pub report: EvictionReport,
}

/// Keeps the cache within the budget the current connection class allows.
pub struct CacheBudget<S: ConnectionClassSource> {
    source: S,
    base_budget_bytes: u64,
    plan: Option<CachePlan>,
}

impl<S: ConnectionClassSource> CacheBudget<S> {
    /// A budget over `source` with the user's base budget.
    #[must_use]
    pub fn new(source: S, base_budget_bytes: u64) -> Self {
        Self {
            source,
            base_budget_bytes,
            plan: None,
        }
    }

    /// The class source this budget samples.
    pub fn source(&self) -> &S {
        &self.source
    }

    /// Sample the class and return the current plan, re-planning when the class changed. The
    /// boolean is `true` on a re-plan (including the first one).
    #[instrument(level = "debug", skip(self), fields(base = self.base_budget_bytes))]
    pub fn current_plan(&mut self) -> (CachePlan, bool) {
        let class = self.source.current();
        match self.plan {
            Some(plan) if plan.class == class => (plan, false),
            previous => {
                let plan = plan_for(class, self.base_budget_bytes);
                info!(
                    from = ?previous.map(|p| p.class),
                    to = ?class,
                    budget_bytes = plan.budget_bytes,
                    defer_bulk = plan.defer_bulk,
                    "cache budget re-planned for connection class"
                );
                self.plan = Some(plan);
                (plan, true)
            }
        }
    }

    /// Whether bulk transfers should wait right now.
    pub fn defer_bulk(&mut self) -> bool {
        self.current_plan().0.defer_bulk
    }

    /// Re-plan if the class changed, then run `sweep` (normally
    /// `|budget| capsule_core::library::cache_sweep(&db, budget)`) under the planned budget.
    ///
    /// # Errors
    ///
    /// Whatever the sweep returns; the plan is kept either way.
    #[instrument(level = "debug", skip(self, sweep))]
    pub fn sweep<E>(
        &mut self,
        sweep: impl FnOnce(i64) -> Result<EvictionReport, E>,
    ) -> Result<CacheSweepOutcome, E> {
        let (plan, replanned) = self.current_plan();
        let report = sweep(plan.sweep_budget())?;
        debug!(
            class = ?plan.class,
            budget_bytes = plan.budget_bytes,
            evicted = report.evicted.len(),
            bytes_reclaimed = report.bytes_reclaimed,
            "cache sweep finished"
        );
        Ok(CacheSweepOutcome {
            plan,
            replanned,
            report,
        })
    }
}

#[cfg(test)]
mod tests;
