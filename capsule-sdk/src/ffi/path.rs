//! The path snapshot across the FFI boundary: the flattened mirror of
//! [`PathSample`](crate::net::path::PathSample) a platform shell reports, and the link signal a
//! finished push feeds the adverse detector.
//!
//! The app reports what its OS listeners see (`NWPathMonitor`, `ConnectivityManager`, the
//! thermal and power notifications) through
//! [`FfiSession::set_path`](crate::ffi::FfiSession::set_path). The SDK stamps the generation —
//! the app never counts paths — and every push the session drives plans from the result.

use std::time::Duration;

use crate::net::TransferSignal;
use crate::net::path::{
    Execution, PathCost, PathInterface, PathSample, PowerState, Reachability, ThermalState,
};
use crate::push::{AssetPushReport, PushError};
use crate::upload::UploadError;

/// FFI mirror of [`PathCost`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPathCost {
    /// Bulk transfer is acceptable.
    Unmetered,
    /// The link is byte-counted (iOS `isExpensive`, Android without `NOT_METERED`).
    Metered,
    /// The OS data saver is on (iOS Low Data Mode, Android Data Saver).
    Constrained,
}

/// FFI mirror of [`PathInterface`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPathInterface {
    /// Ethernet or another wired link.
    Wired,
    /// Wi-Fi.
    Wifi,
    /// A cellular radio.
    Cellular,
    /// Anything else, or unknown.
    Other,
}

/// FFI mirror of [`PowerState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiPowerState {
    /// On external power.
    External,
    /// On battery.
    Battery,
    /// On battery with the OS low-power mode on.
    LowPower,
}

/// FFI mirror of [`ThermalState`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiThermalState {
    /// No thermal pressure.
    Nominal,
    /// Slightly elevated.
    Fair,
    /// High: the OS asks apps to reduce work.
    Serious,
    /// Critical: the OS asks apps to stop discretionary work.
    Critical,
}

/// One sample of the path as the app's OS listeners report it.
///
/// Flat, like [`FfiNotifyInput`](crate::ffi::FfiNotifyInput): `background_deadline_ms` present
/// means the app runs in a background task the OS ends that many milliseconds from now; absent
/// means the foreground.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FfiPathSnapshot {
    /// What the path costs.
    pub cost: FfiPathCost,
    /// Whether the OS reports a usable path.
    pub reachable: bool,
    /// The interface the path runs over.
    pub interface: FfiPathInterface,
    /// The device's power source.
    pub power: FfiPowerState,
    /// The device's thermal state.
    pub thermal: FfiThermalState,
    /// Milliseconds before the OS ends the background task the app runs in; `None` in the
    /// foreground.
    pub background_deadline_ms: Option<u64>,
}

impl From<FfiPathSnapshot> for PathSample {
    fn from(snapshot: FfiPathSnapshot) -> Self {
        Self {
            cost: match snapshot.cost {
                FfiPathCost::Unmetered => PathCost::Unmetered,
                FfiPathCost::Metered => PathCost::Metered,
                FfiPathCost::Constrained => PathCost::Constrained,
            },
            reachability: if snapshot.reachable {
                Reachability::Reachable
            } else {
                Reachability::Unreachable
            },
            interface: match snapshot.interface {
                FfiPathInterface::Wired => PathInterface::Wired,
                FfiPathInterface::Wifi => PathInterface::Wifi,
                FfiPathInterface::Cellular => PathInterface::Cellular,
                FfiPathInterface::Other => PathInterface::Other,
            },
            power: match snapshot.power {
                FfiPowerState::External => PowerState::External,
                FfiPowerState::Battery => PowerState::Battery,
                FfiPowerState::LowPower => PowerState::LowPower,
            },
            thermal: match snapshot.thermal {
                FfiThermalState::Nominal => ThermalState::Nominal,
                FfiThermalState::Fair => ThermalState::Fair,
                FfiThermalState::Serious => ThermalState::Serious,
                FfiThermalState::Critical => ThermalState::Critical,
            },
            execution: match snapshot.background_deadline_ms {
                None => Execution::Foreground,
                Some(ms) => Execution::Background {
                    deadline: Duration::from_millis(ms),
                },
            },
        }
    }
}

/// What a finished push says about the link, for the adverse detector: a push that completed
/// is a clean transfer, one that gave up on the transport a reset, and one that exhausted its
/// recovery budget a stall. A refusal, an auth failure or a local error says nothing about the
/// link, so it records nothing.
pub(crate) fn link_signal(pushed: &Result<AssetPushReport, PushError>) -> Option<TransferSignal> {
    match pushed {
        Ok(_) => Some(TransferSignal::Clean),
        Err(PushError::Upload(UploadError::Transport(_))) => Some(TransferSignal::Reset),
        Err(PushError::Upload(UploadError::RetriesExhausted(_))) => Some(TransferSignal::Stall),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests;
