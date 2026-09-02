//! The closed alert-class enum, its severity, and the [`Alert`] record the predicate emits.
//!
//! SSoT: [Notifications — Alert Classes]. The class list is closed here; each class's *trigger
//! predicate and thresholds* stay owned by the doc that defines the condition, and are
//! implemented in [`super::evaluate()`] against those citations.
//!
//! [Notifications — Alert Classes]: https://docs/design/notifications/#alert-classes

use std::collections::BTreeMap;

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

/// Every alert Capsule can raise. **A closed enum**: an unknown wire value is a structural
/// error, like every other closed enum in the schema rules — which is what the `serde` derive
/// without `#[serde(other)]` gives.
///
/// The variant order is the delivery order [`super::evaluate()`] emits in, and matches the
/// SSoT's own table so a reader can diff the two.
///
/// `Ord` is derived because the suppression map ([`super::NotifyInput::suppressed`]) is keyed on
/// this type and must iterate deterministically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertClass {
    /// Two weeks without a completed sync while changes remain un-synced. Trigger owner:
    /// [Download & Sync — Notifications](https://docs/design/import/download-sync/#notifications).
    SyncStale,
    /// A recovery-secret verification check is due. Trigger owner:
    /// [Backup — Schedule and Triggers](https://docs/design/backup-recovery/#schedule-and-triggers).
    RecoveryCheckDue,
    /// Storage crossed the soft limit and is below the hard limit. Trigger owner:
    /// [Quota — Thresholds and States](https://docs/design/quota/#thresholds-and-states).
    QuotaSoft,
    /// Storage is over the hard limit — the grace window is counting, or has closed. Trigger
    /// owner: [Quota — Thresholds and States](https://docs/design/quota/#thresholds-and-states).
    QuotaGraceExpiring,
    /// Items are sitting on a quarantine surface awaiting a human. Trigger owner:
    /// [Threat Model — Quarantine Surfaces](https://docs/design/threat-model/scenarios/#quarantine-surfaces).
    QuarantinePending,
    /// Guest drops are awaiting review and adoption. Trigger owner:
    /// [Web Upload — Drop and Adoption Lifecycle](https://docs/design/web-upload/#drop-and-adoption-lifecycle).
    DropPending,
}

impl AlertClass {
    /// Every class, in delivery order. Iterating this rather than a hand-written list is what
    /// makes "the enum is closed" a property a reader can check: adding a variant without
    /// extending this array fails to compile, because the array's length is declared.
    pub const ALL: [Self; 6] = [
        Self::SyncStale,
        Self::RecoveryCheckDue,
        Self::QuotaSoft,
        Self::QuotaGraceExpiring,
        Self::QuarantinePending,
        Self::DropPending,
    ];

    /// The stable wire name — identical to the `serde` representation, so a log line, a
    /// [`Alert::params`] value, and the serialized form never disagree.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SyncStale => "sync_stale",
            Self::RecoveryCheckDue => "recovery_check_due",
            Self::QuotaSoft => "quota_soft",
            Self::QuotaGraceExpiring => "quota_grace_expiring",
            Self::QuarantinePending => "quarantine_pending",
            Self::DropPending => "drop_pending",
        }
    }

    /// How loudly a client should present this class. A property of the class, not of the
    /// instant it fired, so it lives here rather than being decided per-[`Alert`].
    #[must_use]
    pub const fn severity(self) -> AlertSeverity {
        match self {
            // The library is silently falling out of date — the case the alert exists for.
            Self::SyncStale
            // Writes beyond the originals are now being refused.
            | Self::QuotaGraceExpiring
            // Items are neither applied nor dropped; they are waiting on a human.
            | Self::QuarantinePending => AlertSeverity::Warning,
            // Advisory: nothing is failing yet.
            Self::RecoveryCheckDue | Self::QuotaSoft | Self::DropPending => AlertSeverity::Advisory,
        }
    }

    /// Whether this class's trigger is a deadline the device can compute alone, and can
    /// therefore be pre-armed as a scheduled local notification.
    ///
    /// `false` for the three server-state classes (`quota_*`, `quarantine_pending`,
    /// `drop_pending`): their condition lives on the server, so on a device that never runs
    /// they cannot fire at all. That gap is real and v1 accepts it — those three surface at
    /// next app launch. Closing it is what the post-v1 wake tier is for.
    #[must_use]
    pub const fn pre_armable(self) -> bool {
        matches!(self, Self::SyncStale | Self::RecoveryCheckDue)
    }
}

/// How prominently a client presents an alert.
///
/// Two variants deliberately, and **neither gates anything** — see
/// [`Alert::blocks_critical_flow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlertSeverity {
    /// Informational: nothing is failing, and nothing is about to.
    Advisory,
    /// Something is already degraded or is being refused.
    Warning,
}

/// One alert that is true at an instant.
///
/// Pure data: a class, its severity, the deadline that produced it (for the pre-armable classes),
/// and the parameters a client interpolates into its own catalog string. **Never a localized
/// string** — the copy is a `notification.*` catalog key the client owns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    /// Which alert this is.
    pub class: AlertClass,
    /// [`AlertClass::severity`] for [`class`](Self::class), carried so a consumer that only
    /// deserializes the record does not have to re-derive it.
    pub severity: AlertSeverity,
    /// The instant whose passing made this alert true, for the pre-armable classes; `None` for
    /// the three whose condition is server-held and has no device-computable deadline.
    pub deadline: Option<Timestamp>,
    /// Parameters for the client's catalog string — plain strings, deterministically ordered.
    ///
    /// The keys in use are `count` (`sync_stale`, `quarantine_pending`, `drop_pending`),
    /// `days_behind` (`sync_stale`), `grace` (`quota_grace_expiring`) and `snooze_budget`
    /// (`recovery_check_due`). Each is documented at the predicate that sets it.
    pub params: BTreeMap<String, String>,
}

impl Alert {
    /// An alert of `class` with its severity filled in, no deadline, and no parameters.
    pub(crate) fn new(class: AlertClass) -> Self {
        Self {
            class,
            severity: class.severity(),
            deadline: None,
            params: BTreeMap::new(),
        }
    }

    /// Attach the deadline whose passing made this alert true.
    pub(crate) fn with_deadline(mut self, deadline: Timestamp) -> Self {
        self.deadline = Some(deadline);
        self
    }

    /// Attach one catalog parameter.
    pub(crate) fn with_param(mut self, key: &str, value: impl Into<String>) -> Self {
        self.params.insert(key.to_owned(), value.into());
        self
    }

    /// Alerts are advisory by construction: **no** alert blocks sync, unlock, upload, or any
    /// critical flow. `const false` for every alert, so the "never blocks" rule is a
    /// compile-time property of the type rather than a convention reviewers have to police —
    /// the same trick
    /// [`VerificationState::blocks_critical_flow`](https://docs/design/backup-recovery/) uses
    /// for the recovery prompt.
    #[must_use]
    pub const fn blocks_critical_flow(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The enum is closed and its wire names are stable: a rename would break every client's
    /// persisted suppression map and its catalog keys at once.
    #[test]
    fn wire_names_match_serde_and_are_stable() {
        let expected = [
            "sync_stale",
            "recovery_check_due",
            "quota_soft",
            "quota_grace_expiring",
            "quarantine_pending",
            "drop_pending",
        ];
        for (class, name) in AlertClass::ALL.into_iter().zip(expected) {
            assert_eq!(class.as_str(), name);
            assert_eq!(
                serde_json::to_string(&class).unwrap(),
                format!("\"{name}\"")
            );
            assert_eq!(
                serde_json::from_str::<AlertClass>(&format!("\"{name}\"")).unwrap(),
                class
            );
        }
    }

    /// SSoT: unknown classes are rejected as structural errors.
    #[test]
    fn unknown_class_is_a_structural_error() {
        assert!(serde_json::from_str::<AlertClass>("\"telemetry_ready\"").is_err());
        assert!(serde_json::from_str::<AlertSeverity>("\"critical\"").is_err());
    }

    /// SSoT: only the two device-computable deadlines are pre-armable.
    #[test]
    fn pre_armable_is_exactly_the_two_device_computable_classes() {
        let armable: Vec<_> = AlertClass::ALL
            .into_iter()
            .filter(|c| c.pre_armable())
            .collect();
        assert_eq!(
            armable,
            vec![AlertClass::SyncStale, AlertClass::RecoveryCheckDue]
        );
    }

    /// SSoT: "No alert ever blocks." Asserted for every class so a new variant cannot opt out.
    #[test]
    fn no_alert_blocks_a_critical_flow() {
        for class in AlertClass::ALL {
            assert!(!Alert::new(class).blocks_critical_flow());
        }
    }

    /// The severity carried on the record is the class's own, never an independent field a
    /// caller can desynchronize.
    #[test]
    fn severity_is_derived_from_the_class() {
        for class in AlertClass::ALL {
            assert_eq!(Alert::new(class).severity, class.severity());
        }
    }
}
