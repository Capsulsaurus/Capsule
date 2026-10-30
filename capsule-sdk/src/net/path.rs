//! The normalized path snapshot and the pure transfer plan (networking doc, Path Snapshot).
//!
//! A platform shell (or the CLI's flags) reports what it sees as a [`PathSample`]: cost,
//! reachability, interface, power, thermal state and how long the app may run. The
//! [`PathMonitor`] stamps each sample with a [`PathGeneration`] — which advances only when
//! reachability, interface or cost changes, never on a minor update — and keeps the latest
//! [`PathSnapshot`].
//!
//! [`plan_transfer`] is the whole of the policy: a pure function of the snapshot, the link
//! behaviour the [`AdverseDetector`] observed, the server's advice, and the client's compiled
//! window bounds. The server is never in the loop; its advice can only lower a window.
//!
//! A new generation drops what belonged to the old path: the monitor resets its
//! [`AdverseDetector`] (resets on the old network say nothing about the new one), a
//! [`GenerationPool`] builds a fresh HTTP client so no pooled connection outlives its path, and
//! [`crate::staged::run_under_path`] cancels in-flight work the new plan forbids and re-plans.

use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use capsule_library::import::{UploadPolicy, UploadTier};
use tokio::sync::watch;
use tracing::instrument;

use super::{AdverseDetector, AdverseThresholds, ConnectionClass, TransferSignal};
use crate::upload::advice::ServerAdvice;
use crate::upload::queue::LaneWindows;
use crate::upload::{ADVERSE_TARGET_REQUEST_DURATION, TARGET_REQUEST_DURATION};

/// What the OS says the path costs. `constrained` is the OS data saver (iOS Low Data Mode,
/// Android Data Saver) and wins over a merely metered link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathCost {
    /// Bulk transfer is acceptable.
    Unmetered,
    /// The link is byte-counted (iOS `isExpensive`, Android without `NOT_METERED`).
    Metered,
    /// The OS data saver is on.
    Constrained,
}

/// Whether the OS reports a usable path at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reachability {
    /// A usable path exists.
    Reachable,
    /// No usable path.
    Unreachable,
}

/// The kind of interface the path runs over. It plans nothing by itself — cost is what the OS
/// reports about money — but a change of interface is a new path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathInterface {
    /// Ethernet or another wired link.
    Wired,
    /// Wi-Fi.
    Wifi,
    /// A cellular radio.
    Cellular,
    /// Anything else, or unknown (a VPN, a loopback, a desktop without a probe).
    Other,
}

/// The device's power source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerState {
    /// On external power.
    External,
    /// On battery.
    Battery,
    /// On battery with the OS low-power mode on.
    LowPower,
}

/// The device's thermal state, in the OS's four steps (iOS `ProcessInfo.ThermalState`,
/// Android `PowerManager` thermal status folded into the same four).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ThermalState {
    /// No thermal pressure.
    Nominal,
    /// Slightly elevated.
    Fair,
    /// High: the OS asks apps to reduce work.
    Serious,
    /// Critical: the OS asks apps to stop discretionary work.
    Critical,
}

/// How the app is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Execution {
    /// In the foreground, with no deadline.
    Foreground,
    /// In a background task the OS ends `deadline` from the sample.
    Background {
        /// The time left before the OS ends the task.
        deadline: Duration,
    },
}

/// One sample of the path as a platform reports it, before the monitor stamps a generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathSample {
    /// What the path costs.
    pub cost: PathCost,
    /// Whether a usable path exists.
    pub reachability: Reachability,
    /// The interface the path runs over.
    pub interface: PathInterface,
    /// The device's power source.
    pub power: PowerState,
    /// The device's thermal state.
    pub thermal: ThermalState,
    /// How the app is running.
    pub execution: Execution,
}

impl PathSample {
    /// The desktop default: a reachable, unmetered path of unknown interface, on external
    /// power, cool, in the foreground.
    #[must_use]
    pub const fn desktop() -> Self {
        Self {
            cost: PathCost::Unmetered,
            reachability: Reachability::Reachable,
            interface: PathInterface::Other,
            power: PowerState::External,
            thermal: ThermalState::Nominal,
            execution: Execution::Foreground,
        }
    }

    /// The desktop default on a path whose cost and reachability are `class`'s: the bridge
    /// from a [`ConnectionClass`] source that reports nothing else. `adverse` is behavioural,
    /// never reported, so it reads as an unmetered path.
    #[must_use]
    pub const fn from_class(class: ConnectionClass) -> Self {
        let mut sample = Self::desktop();
        match class {
            ConnectionClass::Unmetered | ConnectionClass::Adverse => {}
            ConnectionClass::Metered => sample.cost = PathCost::Metered,
            ConnectionClass::Constrained => sample.cost = PathCost::Constrained,
            ConnectionClass::Offline => sample.reachability = Reachability::Unreachable,
        }
        sample
    }

    /// Whether `other` is a different path: its reachability, interface or cost differs.
    /// Power, thermal state and execution change the plan, never the path.
    #[must_use]
    pub fn is_new_path(&self, other: &Self) -> bool {
        self.reachability != other.reachability
            || self.interface != other.interface
            || self.cost != other.cost
    }

    /// The class the OS reports, before behavioural promotion to `adverse`.
    #[must_use]
    pub const fn base_class(&self) -> ConnectionClass {
        match (self.reachability, self.cost) {
            (Reachability::Unreachable, _) => ConnectionClass::Offline,
            (Reachability::Reachable, PathCost::Constrained) => ConnectionClass::Constrained,
            (Reachability::Reachable, PathCost::Metered) => ConnectionClass::Metered,
            (Reachability::Reachable, PathCost::Unmetered) => ConnectionClass::Unmetered,
        }
    }
}

/// Which path a snapshot belongs to. Advances by one each time reachability, interface or cost
/// changes; everything planned or pooled under an older generation belongs to a path that is
/// gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PathGeneration(pub u64);

/// The normalized path: the latest sample, stamped with its generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PathSnapshot {
    /// What the platform last reported.
    pub sample: PathSample,
    /// The path the sample belongs to.
    pub generation: PathGeneration,
}

/// What the [`AdverseDetector`] has observed of the link: the resets and stalls it counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkBehaviour {
    /// The detector has promoted the link to `adverse`.
    pub adverse: bool,
}

/// Server advice as the plan reads it: the advice and how long ago it arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HeldAdvice {
    /// The advice, clamped when it was read.
    pub advice: ServerAdvice,
    /// Its age now, against its `ttl`.
    pub age: Duration,
}

impl HeldAdvice {
    /// No advice: every window is the client's own.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            advice: ServerAdvice::absent(),
            age: Duration::ZERO,
        }
    }
}

/// The shortest background deadline bulk work starts under: one minute. A preview or original
/// started with less is likely cut by the OS before it lands, so only the index tier — a few
/// KB per asset — runs.
pub const MIN_BULK_DEADLINE: Duration = Duration::from_mins(1);

/// Which tiers the plan admits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TierAdmission {
    /// Every tier.
    All,
    /// Only the index tier; the bulk tiers wait.
    IndexOnly,
    /// Nothing: no transfer runs.
    Nothing,
}

impl TierAdmission {
    /// Whether `tier` is admitted.
    #[must_use]
    pub fn admits(self, tier: UploadTier) -> bool {
        match self {
            Self::All => true,
            Self::IndexOnly => tier == UploadTier::Index,
            Self::Nothing => false,
        }
    }

    /// Whether `tier` may run under this admission and its `hold`, given the user's
    /// force-sync consent, which lifts an index-only hold that
    /// [yields to it](Hold::yields_to_force_sync).
    #[must_use]
    pub fn permits(self, hold: Option<Hold>, tier: UploadTier, force_sync: bool) -> bool {
        self.admits(tier)
            || (force_sync
                && self == Self::IndexOnly
                && hold.is_some_and(Hold::yields_to_force_sync))
    }

    /// The admission and hold `class` alone implies, for a caller that knows nothing else of
    /// the path: the tier gate [`ConnectionClass::permits_tier`] states.
    #[must_use]
    pub const fn for_class(class: ConnectionClass) -> (Self, Option<Hold>) {
        match class {
            ConnectionClass::Unmetered => (Self::All, None),
            ConnectionClass::Metered | ConnectionClass::Constrained => {
                (Self::IndexOnly, Some(Hold::Cost))
            }
            ConnectionClass::Adverse => (Self::IndexOnly, Some(Hold::Adverse)),
            ConnectionClass::Offline => (Self::Nothing, Some(Hold::Offline)),
        }
    }
}

/// Why the plan holds back work it would otherwise run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hold {
    /// No usable path: nothing runs.
    Offline,
    /// The thermal state is critical: nothing runs.
    ThermalCritical,
    /// The path is metered or the data saver is on: bulk waits. A user's force-sync consent
    /// overrides it.
    Cost,
    /// The link is behaving adversely: bulk waits. A user's force-sync consent overrides it.
    Adverse,
    /// Low-power mode is on: bulk waits.
    LowPower,
    /// The thermal state is serious: bulk waits.
    ThermalSerious,
    /// The background deadline is shorter than [`MIN_BULK_DEADLINE`]: bulk waits.
    Deadline,
}

impl Hold {
    /// Whether a user's force-sync consent lifts this hold. It overrides what the link costs or
    /// how it behaves; it never resurrects a missing path or overrules the device's own power
    /// and thermal limits.
    #[must_use]
    pub const fn yields_to_force_sync(self) -> bool {
        matches!(self, Self::Cost | Self::Adverse)
    }
}

/// What the client does on the current path. Produced only by [`plan_transfer`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferPlan {
    /// The path this plan was made for.
    pub generation: PathGeneration,
    /// The effective connection class: the OS's, promoted to `adverse` by the link behaviour.
    pub class: ConnectionClass,
    /// The tiers that may run.
    pub admission: TierAdmission,
    /// Why the plan admits less than every tier, if it does.
    pub hold: Option<Hold>,
    /// The sessions and ranges windows: the compiled bounds, lowered by fresh advice and by
    /// the device's power and thermal state.
    pub windows: LaneWindows,
    /// How long one chunk request should take.
    pub chunk_target: Duration,
    /// The chunk size the server suggested, while its advice is fresh.
    pub suggested_chunk: Option<u64>,
    /// The bounded `Range` window a fetch uses, or `None` for the open remainder.
    pub range_window: Option<u64>,
}

impl TransferPlan {
    /// Whether `tier` may run, given the user's force-sync consent.
    #[must_use]
    pub fn permits_tier(&self, tier: UploadTier, force_sync: bool) -> bool {
        self.admission.permits(self.hold, tier, force_sync)
    }

    /// Whether the plan holds bulk work back.
    #[must_use]
    pub fn defers_bulk(&self) -> bool {
        self.admission != TierAdmission::All
    }

    /// The upload policy a push runs under, given the one the user `requested`: a plan that
    /// holds bulk back stages the push, so the index tier still publishes each asset.
    #[must_use]
    pub fn upload_policy(&self, requested: UploadPolicy) -> UploadPolicy {
        if self.defers_bulk() {
            UploadPolicy::Staged
        } else {
            requested
        }
    }
}

/// Plan transfers on `snapshot`, given what the link has shown (`link`), the server's `advice`,
/// and the client's compiled window `bounds`. Pure: the same inputs always plan the same.
///
/// The rules, in precedence order:
///
/// 1. An unreachable path admits nothing ([`Hold::Offline`]); a critical thermal state admits
///    nothing ([`Hold::ThermalCritical`]).
/// 2. The class is the cost's, promoted to `adverse` while the link behaves adversely.
/// 3. Bulk tiers wait on anything but an unmetered, well-behaved link ([`Hold::Cost`],
///    [`Hold::Adverse`]), then on low-power mode, a serious thermal state, and a background
///    deadline under [`MIN_BULK_DEADLINE`], in that order. The index tier still runs.
/// 4. The windows are `bounds`, lowered by fresh advice, and narrowed to one session in
///    low-power mode or from a serious thermal state up.
/// 5. Under `adverse` a chunk targets [`ADVERSE_TARGET_REQUEST_DURATION`] and a fetch uses the
///    bounded range window; otherwise [`TARGET_REQUEST_DURATION`] and the open remainder.
#[must_use]
pub fn plan_transfer(
    snapshot: &PathSnapshot,
    link: LinkBehaviour,
    advice: HeldAdvice,
    bounds: LaneWindows,
) -> TransferPlan {
    let sample = &snapshot.sample;
    let base = sample.base_class();
    let class = if link.adverse && base != ConnectionClass::Offline {
        ConnectionClass::Adverse
    } else {
        base
    };

    let stop = if base == ConnectionClass::Offline {
        Some(Hold::Offline)
    } else if sample.thermal == ThermalState::Critical {
        Some(Hold::ThermalCritical)
    } else {
        None
    };
    let bulk_hold = match class {
        ConnectionClass::Metered | ConnectionClass::Constrained => Some(Hold::Cost),
        ConnectionClass::Adverse => Some(Hold::Adverse),
        ConnectionClass::Offline => Some(Hold::Offline),
        ConnectionClass::Unmetered if sample.power == PowerState::LowPower => Some(Hold::LowPower),
        ConnectionClass::Unmetered if sample.thermal >= ThermalState::Serious => {
            Some(Hold::ThermalSerious)
        }
        ConnectionClass::Unmetered => match sample.execution {
            Execution::Background { deadline } if deadline < MIN_BULK_DEADLINE => {
                Some(Hold::Deadline)
            }
            _ => None,
        },
    };
    let (admission, hold) = match (stop, bulk_hold) {
        (Some(stop), _) => (TierAdmission::Nothing, Some(stop)),
        (None, Some(hold)) => (TierAdmission::IndexOnly, Some(hold)),
        (None, None) => (TierAdmission::All, None),
    };

    let mut sessions = advice.advice.sessions_window(bounds.sessions, advice.age);
    if sample.power == PowerState::LowPower || sample.thermal >= ThermalState::Serious {
        sessions = 1;
    }
    let windows = LaneWindows::new(sessions, advice.advice.ranges_window(bounds.ranges));

    let chunk_target = if class == ConnectionClass::Adverse {
        ADVERSE_TARGET_REQUEST_DURATION
    } else {
        TARGET_REQUEST_DURATION
    };

    TransferPlan {
        generation: snapshot.generation,
        class,
        admission,
        hold,
        windows,
        chunk_target,
        suggested_chunk: advice.advice.chunk(advice.age),
        range_window: class.range_window(),
    }
}

// ─── The monitor ─────────────────────────────────────────────────────────────

/// What one [`PathMonitor::observe`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathUpdate {
    /// Reachability, interface or cost changed: a new generation, the detector reset, and
    /// this plan made for the new path.
    NewPath(TransferPlan),
    /// The same path; power, thermal state or execution may have changed this plan.
    SamePath(TransferPlan),
}

impl PathUpdate {
    /// The plan after the update.
    #[must_use]
    pub const fn plan(&self) -> TransferPlan {
        match self {
            Self::NewPath(plan) | Self::SamePath(plan) => *plan,
        }
    }
}

#[derive(Debug)]
struct MonitorState {
    snapshot: PathSnapshot,
    detector: AdverseDetector,
}

impl MonitorState {
    fn link(&self) -> LinkBehaviour {
        LinkBehaviour {
            adverse: self.detector.is_adverse(),
        }
    }
}

/// The current path, the link behaviour observed on it, and the plan they make, shared by
/// everything that transfers.
///
/// Platform shells call [`observe`](Self::observe) with each sample, transfers report their
/// outcomes through [`record`](Self::record), and a long transfer [`subscribe`](Self::subscribe)s
/// to learn — within the tick the change lands — that the plan it runs under has changed.
#[derive(Debug)]
pub struct PathMonitor {
    state: Mutex<MonitorState>,
    bounds: LaneWindows,
    plans: watch::Sender<TransferPlan>,
}

impl PathMonitor {
    /// A monitor whose first path is `sample`, generation 1, with the default adverse
    /// thresholds and `bounds` as the compiled windows.
    #[must_use]
    pub fn new(sample: PathSample, bounds: LaneWindows) -> Self {
        Self::with_thresholds(sample, bounds, AdverseThresholds::default())
    }

    /// [`new`](Self::new), with the detector's `thresholds`.
    #[must_use]
    pub fn with_thresholds(
        sample: PathSample,
        bounds: LaneWindows,
        thresholds: AdverseThresholds,
    ) -> Self {
        let state = MonitorState {
            snapshot: PathSnapshot {
                sample,
                generation: PathGeneration(1),
            },
            detector: AdverseDetector::new(thresholds),
        };
        let plan = plan_transfer(&state.snapshot, state.link(), HeldAdvice::none(), bounds);
        Self {
            state: Mutex::new(state),
            bounds,
            plans: watch::Sender::new(plan),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, MonitorState> {
        // A panic while holding the lock left plain values behind; they stay usable.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The latest snapshot.
    #[must_use]
    pub fn snapshot(&self) -> PathSnapshot {
        self.state().snapshot
    }

    /// The current plan, without advice.
    #[must_use]
    pub fn plan(&self) -> TransferPlan {
        *self.plans.borrow()
    }

    /// The current plan with `advice` applied to its windows and chunk size.
    #[must_use]
    pub fn plan_with(&self, advice: HeldAdvice) -> TransferPlan {
        let state = self.state();
        plan_transfer(&state.snapshot, state.link(), advice, self.bounds)
    }

    /// A receiver that wakes each time the plan changes.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<TransferPlan> {
        self.plans.subscribe()
    }

    /// Take a new sample. A change of reachability, interface or cost advances the
    /// generation and resets the adverse detector; the plan is remade either way and published
    /// when it changed.
    #[instrument(level = "debug", skip(self))]
    pub fn observe(&self, sample: PathSample) -> PathUpdate {
        let mut state = self.state();
        let new_path = state.snapshot.sample.is_new_path(&sample);
        if new_path {
            state.snapshot.generation = PathGeneration(state.snapshot.generation.0 + 1);
            state.detector.reset();
        }
        state.snapshot.sample = sample;
        let plan = plan_transfer(
            &state.snapshot,
            state.link(),
            HeldAdvice::none(),
            self.bounds,
        );
        // Published under the lock, so two samples racing publish in the order they planned.
        self.publish(plan);
        drop(state);
        if new_path {
            tracing::info!(
                generation = plan.generation.0,
                class = ?plan.class,
                admission = ?plan.admission,
                hold = ?plan.hold,
                "path: a new path; the adverse detector is reset and transfers re-plan"
            );
            PathUpdate::NewPath(plan)
        } else {
            PathUpdate::SamePath(plan)
        }
    }

    /// Fold one transfer outcome into the adverse detector, and return the plan after it.
    pub fn record(&self, signal: TransferSignal) -> TransferPlan {
        let mut state = self.state();
        state.detector.record(signal);
        let plan = plan_transfer(
            &state.snapshot,
            state.link(),
            HeldAdvice::none(),
            self.bounds,
        );
        self.publish(plan);
        drop(state);
        plan
    }

    fn publish(&self, plan: TransferPlan) {
        self.plans.send_if_modified(|current| {
            if *current == plan {
                return false;
            }
            tracing::debug!(
                generation = plan.generation.0,
                class = ?plan.class,
                admission = ?plan.admission,
                hold = ?plan.hold,
                "path: the transfer plan changed"
            );
            *current = plan;
            true
        });
    }
}

// ─── Pooled connections per generation ───────────────────────────────────────

/// An HTTP client whose connection pool belongs to one path generation.
///
/// `reqwest` has no way to empty a pool, so a new generation builds a new client and drops the
/// old one: no request on the new path rides a connection dialled over the old one. In-flight
/// requests keep the clone they hold until they end.
pub struct GenerationPool {
    build: Box<dyn Fn() -> reqwest::Result<reqwest::Client> + Send + Sync>,
    current: Mutex<Option<(PathGeneration, reqwest::Client)>>,
}

impl std::fmt::Debug for GenerationPool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenerationPool")
            .field("generation", &self.generation())
            .finish_non_exhaustive()
    }
}

impl GenerationPool {
    /// A pool whose clients come from `build`, which must start from
    /// [`http_builder`](super::http_builder) so every client carries the handshake.
    #[must_use]
    pub fn new(
        build: impl Fn() -> reqwest::Result<reqwest::Client> + Send + Sync + 'static,
    ) -> Self {
        Self {
            build: Box::new(build),
            current: Mutex::new(None),
        }
    }

    /// A pool of [`http_client`](super::http_client)s: the SDK's one client shape.
    #[must_use]
    pub fn sdk() -> Self {
        Self::new(super::http_client)
    }

    /// The generation the held client was built for, if one has been built.
    #[must_use]
    pub fn generation(&self) -> Option<PathGeneration> {
        self.current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .map(|(generation, _)| *generation)
    }

    /// The client for `generation`: the held one while the generation is unchanged, a fresh one
    /// — dropping the old pool — once it moves.
    ///
    /// # Errors
    ///
    /// Whatever building the fresh client refused with; the held client, if any, is kept, so
    /// the next call tries again.
    pub fn client(&self, generation: PathGeneration) -> reqwest::Result<reqwest::Client> {
        let mut current = self.current.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((held, client)) = current.as_ref()
            && *held == generation
        {
            return Ok(client.clone());
        }
        let client = (self.build)()?;
        if let Some((old, _)) = current.replace((generation, client.clone())) {
            tracing::info!(
                old = old.0,
                new = generation.0,
                "path: a new generation; dropping pooled connections"
            );
        }
        Ok(client)
    }
}

#[cfg(test)]
mod tests;
