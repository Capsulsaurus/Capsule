//! [`NotifyInput`] — the snapshot of device-held state the predicate is evaluated against.
//!
//! Every field is caller-supplied and `Option`/zero where the client has not learned it yet.
//! Nothing here reads a clock, a socket, or SQLite: this crate holds none of the trigger state
//! (see the [module docs](super) for why), so the only honest shape is a struct the client
//! fills.
//!
//! The security boundary is the shape itself: counts and instants only. No album id, no title,
//! no asset id, nothing a server could author.

use std::collections::{BTreeMap, BTreeSet};

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

use super::class::AlertClass;

/// The state [`super::evaluate()`] and [`super::next_deadline()`] decide from.
///
/// [`Default`] is the "a client that has just installed and learned nothing" input, and it
/// yields no alerts and no deadline.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotifyInput {
    /// Sync progress, persisted by the client at the end of each successful sync. `None` on a
    /// device that has never completed one — which raises no `sync_stale`, because the alert is
    /// about a *stale* sync and not a missing one.
    pub sync: Option<SyncFacts>,
    /// The recovery-verification cadence, projected into flat facts. `None` before recovery is
    /// set up.
    pub recovery: Option<RecoveryFacts>,
    /// The last quota response the client received. `None` before the first
    /// `GET /v1/quota` — quota state is server-held, so it is only ever as current as that call.
    pub quota: Option<QuotaFacts>,
    /// How many items sit on the client's quarantine surfaces awaiting a human.
    ///
    /// **Excludes pending drops.** A pending drop is a quarantine surface in the threat model's
    /// own inventory, but it has its own alert class here, so a client that filled this field
    /// from that inventory unfiltered would raise both classes for the same items. Count drops
    /// in [`drops_pending`](Self::drops_pending) and nowhere else.
    pub quarantine_pending: u64,
    /// How many guest drops are awaiting review and adoption.
    pub drops_pending: u64,
    /// Per-class **snooze**: the instant a deferred class becomes due again. A class whose entry
    /// is strictly after `now` emits nothing from [`super::evaluate()`].
    ///
    /// This is how a client applies a snooze without this crate owning that state machine — the
    /// bounded-snooze-then-badge mechanic has one owner already
    /// ([`RecoveryCadence`](https://docs/design/backup-recovery/#recovery-verification-cadence)),
    /// and a second copy here would be two owners of one mechanic.
    ///
    /// A snooze **defers** the class's pre-arm deadline rather than cancelling it
    /// ([`super::pre_arm_deadlines()`]): a class snoozed after it fired must fire again when the
    /// snooze ends, and that end is a deadline the device can compute. Cancelling instead would
    /// leave the alert reachable only in-app, which for `sync_stale` defeats the entire pre-arm
    /// rule the class exists under.
    pub suppressed: BTreeMap<AlertClass, Timestamp>,
    /// Per-class **disable**: the user turned this alert off. Emits nothing and arms nothing, at
    /// any instant.
    ///
    /// A separate field from [`suppressed`](Self::suppressed) rather than a far-future instant in
    /// it, because they are different mechanics with different effects on the timer — a snooze
    /// defers, a disable cancels — and because a sentinel instant is exactly the kind of
    /// convention that does not survive a string-typed FFI boundary: a client writing "the year
    /// 2999" would mean *disabled* and get a timer armed 975 years out.
    ///
    /// **Disabling suppresses the warning, never the behavior.** Turning off `sync_stale` does
    /// not turn off auto-sync; turning off `recovery_check_due` does not stop the recovery check
    /// mattering. An alert is a report about a condition, never the mechanism managing it.
    pub disabled: BTreeSet<AlertClass>,
}

impl NotifyInput {
    /// Whether `class` is snoozed or disabled at `now`, and therefore reports nothing.
    ///
    /// A snooze entry exactly at `now` has expired: a snooze is `until`, exclusive, so a class
    /// snoozed to `now` is due again at `now` — the same boundary convention as every other
    /// threshold in this module. A disable never expires.
    #[must_use]
    pub fn is_suppressed(&self, class: AlertClass, now: Timestamp) -> bool {
        self.disabled.contains(&class)
            || self
                .suppressed
                .get(&class)
                .is_some_and(|until| *until > now)
    }
}

/// What the client knows about its own sync progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFacts {
    /// When the last **completed** sync finished. This is the epoch the two-week deadline is
    /// measured from, and the instant at which a client re-arms the timer.
    pub last_completed_sync: Timestamp,
    /// Changes still waiting to reach the server — including originals still pending under a
    /// staged upload policy. Zero means nothing is behind, so nothing is stale.
    pub unsynced_changes: u64,
}

/// The recovery-verification cadence, flattened to the facts the predicate needs.
///
/// The 7 d → 90 d → 180 d ladder, its re-arm triggers, and its snooze accounting stay owned by
/// the cadence scheduler; this module consumes the already-computed `next_due` and never
/// recomputes the ladder. `capsule-sdk` depends on this crate and not the reverse, so the
/// scheduler cannot be named here — the SDK projects into this struct instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryFacts {
    /// When the next verification prompt becomes due.
    pub next_due: Timestamp,
    /// When an active snooze expires, if one is active.
    ///
    /// A snooze set *before* the due date does not make the check due earlier: the class is due
    /// at the later of this and [`next_due`](Self::next_due).
    pub snoozed_until: Option<Timestamp>,
    /// Whether the consecutive-snooze budget is spent. When it is, the class has degraded to a
    /// persistent, non-blocking badge: it is still reported (a client cannot render a badge for
    /// a condition it was not told about), but it is no longer pre-armed as a notification —
    /// the badge never escalates back into an alert on its own.
    pub snooze_budget_spent: bool,
    /// Whether the scheduler has escalated to the guided re-wrap — repeated verification
    /// failures, or the user explicitly declaring the secret lost.
    ///
    /// Carried because the alert class set is closed and `recovery_check_due` is the only class
    /// that can report it: without this fact the alert for "you told us you lost your recovery
    /// secret" would be indistinguishable from the routine ninety-day check. It surfaces as the
    /// `recovery` parameter (`"rewrap"` / `"check"`) so a client routes into the guided re-wrap
    /// instead of a verification prompt. `#[serde(default)]` so a snapshot persisted before this
    /// field existed still loads.
    #[serde(default)]
    pub rewrap_due: bool,
}

/// The last quota answer the client holds.
///
/// A one-field struct on purpose: the wire response carries no `over_since` and no grace
/// deadline, which is exactly why the quota classes are not pre-armable. When it grows one, the
/// deadline field lands here without changing [`NotifyInput`]'s shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotaFacts {
    /// The state the server reported.
    pub state: QuotaAdvisory,
}

/// The quota state, as `GET /v1/quota` reports it.
///
/// Mirrors the SSoT's state table one-for-one rather than collapsing it, so a client can hand
/// the server's answer straight through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuotaAdvisory {
    /// `used < soft_limit`. All uploads succeed normally.
    Ok,
    /// `soft_limit <= used < hard_limit`. Uploads succeed; the client warns.
    SoftWarning,
    /// `used >= hard_limit`. New uploads are rejected at session creation; the grace window is
    /// counting.
    HardExceeded,
    /// Over the hard limit for longer than the grace window. Additive to
    /// [`HardExceeded`](Self::HardExceeded): metadata-growth writes are refused too.
    GraceExpired,
    /// An admin or billing action, not a threshold. Server-defined and owned by moderation, so
    /// it raises no quota alert class here.
    Suspended,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ts(secs: i64) -> Timestamp {
        Timestamp::from_second(secs).unwrap()
    }

    /// A blank client has learned nothing, and nothing is suppressed.
    #[test]
    fn default_input_suppresses_nothing() {
        let input = NotifyInput::default();
        assert_eq!(input.sync, None);
        assert_eq!(input.recovery, None);
        assert_eq!(input.quota, None);
        assert_eq!(input.quarantine_pending, 0);
        assert_eq!(input.drops_pending, 0);
        assert!(input.disabled.is_empty());
        for class in AlertClass::ALL {
            assert!(!input.is_suppressed(class, ts(0)));
        }
    }

    /// Suppression is `until`, exclusive: before / at / after the instant.
    #[test]
    fn suppression_boundary_is_exclusive() {
        let mut input = NotifyInput::default();
        input.suppressed.insert(AlertClass::SyncStale, ts(1_000));

        assert!(input.is_suppressed(AlertClass::SyncStale, ts(999)));
        assert!(!input.is_suppressed(AlertClass::SyncStale, ts(1_000)));
        assert!(!input.is_suppressed(AlertClass::SyncStale, ts(1_001)));
        // Suppression is per class and never leaks to a neighbour.
        assert!(!input.is_suppressed(AlertClass::RecoveryCheckDue, ts(999)));
    }

    /// A disabled class is suppressed at every instant, and only that class.
    #[test]
    fn disabled_is_suppressed_forever() {
        let mut input = NotifyInput::default();
        input.disabled.insert(AlertClass::DropPending);
        for at in [i64::from(i32::MIN), 0, 1_700_000_000, i64::from(i32::MAX)] {
            assert!(input.is_suppressed(AlertClass::DropPending, ts(at)), "{at}");
        }
        assert!(!input.is_suppressed(AlertClass::SyncStale, ts(0)));
    }

    /// `#[serde(default)]` means a client may send only the fields it has.
    #[test]
    fn partial_json_deserializes_to_defaults() {
        let input: NotifyInput = serde_json::from_str(r#"{"drops_pending":3}"#).unwrap();
        assert_eq!(input.drops_pending, 3);
        assert_eq!(
            input,
            NotifyInput {
                drops_pending: 3,
                ..NotifyInput::default()
            }
        );
    }

    /// The quota states are a closed enum with stable wire names.
    #[test]
    fn quota_advisory_wire_names() {
        for (state, name) in [
            (QuotaAdvisory::Ok, "ok"),
            (QuotaAdvisory::SoftWarning, "soft_warning"),
            (QuotaAdvisory::HardExceeded, "hard_exceeded"),
            (QuotaAdvisory::GraceExpired, "grace_expired"),
            (QuotaAdvisory::Suspended, "suspended"),
        ] {
            assert_eq!(
                serde_json::to_string(&state).unwrap(),
                format!("\"{name}\"")
            );
        }
        assert!(serde_json::from_str::<QuotaAdvisory>("\"over\"").is_err());
    }
}
