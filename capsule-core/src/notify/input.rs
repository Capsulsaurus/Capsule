//! [`NotifyInput`] — the snapshot of device-held state the predicate is evaluated against.
//!
//! Every field is caller-supplied and `Option`/zero where the client has not learned it yet.
//! Nothing here reads a clock, a socket, or SQLite: this crate holds none of the trigger state
//! (see the [module docs](super) for why), so the only honest shape is a struct the client
//! fills.
//!
//! The security boundary is the shape itself: counts and instants only. No album id, no title,
//! no asset id, nothing a server could author.

use std::collections::BTreeMap;

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
    pub quarantine_pending: u64,
    /// How many guest drops are awaiting review and adoption.
    pub drops_pending: u64,
    /// Per-class suppression: a class whose entry is **strictly after** `now` emits nothing and
    /// contributes no deadline.
    ///
    /// This is how a client applies snooze and disable without this crate owning that state
    /// machine — the bounded-snooze-then-badge mechanic has one owner already
    /// ([`RecoveryCadence`](https://docs/design/backup-recovery/#recovery-verification-cadence)),
    /// and a second copy here would be two owners of one mechanic. A *disabled* class is an
    /// entry of [`Timestamp::MAX`]; suppressing the warning never suppresses the behavior —
    /// turning off `sync_stale` does not turn off auto-sync.
    pub suppressed: BTreeMap<AlertClass, Timestamp>,
}

impl NotifyInput {
    /// Whether `class` is snoozed or disabled at `now`.
    ///
    /// An entry exactly at `now` has expired: suppression is `until`, exclusive, so a class
    /// snoozed to `now` is due again at `now` — the same boundary convention as every other
    /// threshold in this module.
    #[must_use]
    pub fn is_suppressed(&self, class: AlertClass, now: Timestamp) -> bool {
        self.suppressed
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

/// The recovery-verification cadence, flattened to the three facts the predicate needs.
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
    pub snoozed_until: Option<Timestamp>,
    /// Whether the consecutive-snooze budget is spent. When it is, the class has degraded to a
    /// persistent, non-blocking badge: it is still reported (a client cannot render a badge for
    /// a condition it was not told about), but it is no longer pre-armed as a notification —
    /// the badge never escalates back into an alert on its own.
    pub snooze_budget_spent: bool,
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

    /// A disabled class is an entry at the far end of the representable range.
    #[test]
    fn disabled_is_suppressed_forever() {
        let mut input = NotifyInput::default();
        input
            .suppressed
            .insert(AlertClass::DropPending, Timestamp::MAX);
        assert!(input.is_suppressed(AlertClass::DropPending, ts(1_700_000_000)));
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
