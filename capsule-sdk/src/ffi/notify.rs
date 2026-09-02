//! The local-alert surface across the FFI boundary (slice `S-D29`, core half): the flattened
//! mirror of [`capsule_core::notify`] plus the two functions the apps call.
//!
//! # Why free functions
//!
//! [`evaluate_alerts`] and [`next_alert_deadline`] are free `#[uniffi::export]` functions rather
//! than methods on
//! [`FfiWorkspace`](crate::ffi::FfiWorkspace). The workspace holds none of the predicate's
//! inputs — there is no persisted last-sync instant, no client-side quota type, and no
//! quarantine table — so a method would take the same [`FfiNotifyInput`] and then lock a mutex
//! it never reads. The predicate is a pure function of caller-supplied state, and the boundary
//! says so.
//!
//! # Shape
//!
//! [`FfiNotifyInput`] is **flat**: uniffi records nest, but a foreign caller assembling five
//! optional sub-records to ask one question is worse than a struct whose fields are each
//! independently `None`. Presence is explicit — `last_completed_sync` present means the sync
//! facts are known, `recovery_next_due` present means recovery is set up, `quota_state` present
//! means a quota response has been seen.
//!
//! Timestamps cross as **RFC 3339 strings**, following the `changed_at` precedent on
//! [`FfiSyncApplyOutcome`](crate::ffi::FfiSyncApplyOutcome): Kotlin and Swift each have their own
//! instant type and no shared integer convention worth guessing at. A string that does not parse
//! is [`FfiError::InvalidArgument`], never a panic.

use std::collections::HashMap;

use capsule_core::notify::{
    self, Alert, AlertClass, AlertSeverity, NotifyInput, QuotaAdvisory, QuotaFacts, RecoveryFacts,
    SyncFacts,
};
use jiff::Timestamp;

use super::FfiError;

/// Every alert class, flattened for the bindings. A closed enum on both sides of the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiAlertClass {
    /// Two weeks without a completed sync while changes remain un-synced.
    SyncStale,
    /// A recovery-secret verification check is due.
    RecoveryCheckDue,
    /// Storage crossed the soft limit and is below the hard limit.
    QuotaSoft,
    /// Storage is over the hard limit — the grace window is counting, or has closed.
    QuotaGraceExpiring,
    /// Items are sitting on a quarantine surface awaiting a human.
    QuarantinePending,
    /// Guest drops are awaiting review and adoption.
    DropPending,
}

impl From<AlertClass> for FfiAlertClass {
    fn from(class: AlertClass) -> Self {
        match class {
            AlertClass::SyncStale => Self::SyncStale,
            AlertClass::RecoveryCheckDue => Self::RecoveryCheckDue,
            AlertClass::QuotaSoft => Self::QuotaSoft,
            AlertClass::QuotaGraceExpiring => Self::QuotaGraceExpiring,
            AlertClass::QuarantinePending => Self::QuarantinePending,
            AlertClass::DropPending => Self::DropPending,
        }
    }
}

/// How prominently a client presents an alert. **Neither variant gates anything** — no alert
/// blocks sync, unlock, upload, or any critical flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiAlertSeverity {
    /// Informational: nothing is failing, and nothing is about to.
    Advisory,
    /// Something is already degraded or is being refused.
    Warning,
}

impl From<AlertSeverity> for FfiAlertSeverity {
    fn from(severity: AlertSeverity) -> Self {
        match severity {
            AlertSeverity::Advisory => Self::Advisory,
            AlertSeverity::Warning => Self::Warning,
        }
    }
}

/// The quota state, as `GET /v1/quota` reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiQuotaAdvisory {
    /// Below the soft limit. All uploads succeed normally.
    Ok,
    /// At or above the soft limit and below the hard limit. Uploads succeed; the client warns.
    SoftWarning,
    /// At or above the hard limit. New uploads are rejected; the grace window is counting.
    HardExceeded,
    /// Over the hard limit for longer than the grace window; metadata-growth writes are
    /// refused too.
    GraceExpired,
    /// An admin or billing action rather than a threshold. Raises no quota alert.
    Suspended,
}

impl From<FfiQuotaAdvisory> for QuotaAdvisory {
    fn from(state: FfiQuotaAdvisory) -> Self {
        match state {
            FfiQuotaAdvisory::Ok => Self::Ok,
            FfiQuotaAdvisory::SoftWarning => Self::SoftWarning,
            FfiQuotaAdvisory::HardExceeded => Self::HardExceeded,
            FfiQuotaAdvisory::GraceExpired => Self::GraceExpired,
            FfiQuotaAdvisory::Suspended => Self::Suspended,
        }
    }
}

/// One alert that is true at an instant.
///
/// Never a localized string: `class` selects the app's own `notification.*` catalog key and
/// `params` are interpolated into it, so a server can neither supply nor influence the words a
/// user reads.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiAlert {
    /// Which alert this is.
    pub class: FfiAlertClass,
    /// How prominently to present it.
    pub severity: FfiAlertSeverity,
    /// The instant whose passing made this alert true (RFC 3339), for the two pre-armable
    /// classes; `None` for the three whose condition is server-held.
    pub deadline: Option<String>,
    /// Catalog parameters — `count`, `days_behind`, `grace`, `snooze_budget`.
    pub params: HashMap<String, String>,
}

impl From<Alert> for FfiAlert {
    fn from(alert: Alert) -> Self {
        Self {
            class: alert.class.into(),
            severity: alert.severity.into(),
            deadline: alert.deadline.map(|d| d.to_string()),
            params: alert.params.into_iter().collect(),
        }
    }
}

/// The device-held state the predicate decides from, flattened for the bindings.
///
/// Counts and instants only — no album id, no title, no asset id. An all-default value (every
/// `Option` `None`, every count `0`, an empty map) is the "just installed, learned nothing"
/// input, and yields no alerts and no deadline.
#[derive(Debug, Clone, Default, uniffi::Record)]
pub struct FfiNotifyInput {
    /// When the last **completed** sync finished (RFC 3339). `None` on a device that has never
    /// completed one — which raises no `sync_stale`, because the alert is about a *stale* sync
    /// and not a missing one. When `None`, `unsynced_changes` is ignored.
    pub last_completed_sync: Option<String>,
    /// Changes still waiting to reach the server, including originals still pending under a
    /// staged upload policy.
    #[uniffi(default = 0)]
    pub unsynced_changes: u64,
    /// When the next recovery-verification prompt becomes due (RFC 3339). Project it from the
    /// scheduler with
    /// [`RecoveryCadence::notify_facts`](crate::recovery::RecoveryCadence::notify_facts) rather
    /// than computing it here. `None` before recovery is set up, which ignores the other two
    /// `recovery_*` fields.
    pub recovery_next_due: Option<String>,
    /// When an active snooze on the recovery prompt expires (RFC 3339), if one is active.
    pub recovery_snoozed_until: Option<String>,
    /// Whether the consecutive-snooze budget is spent — the class has degraded to a persistent,
    /// non-blocking badge: still reported, no longer pre-armed.
    #[uniffi(default = false)]
    pub recovery_snooze_budget_spent: bool,
    /// The state from the last `GET /v1/quota`. `None` before the first one.
    pub quota_state: Option<FfiQuotaAdvisory>,
    /// How many items sit on the client's quarantine surfaces awaiting a human.
    #[uniffi(default = 0)]
    pub quarantine_pending: u64,
    /// How many guest drops are awaiting review and adoption.
    #[uniffi(default = 0)]
    pub drops_pending: u64,
    /// Per-class suppression: class wire name (`sync_stale`, …) to the RFC 3339 instant the
    /// snooze or disable runs until, exclusive. A class suppressed past `now` reports nothing
    /// and arms nothing. Disabling a class is a far-future instant; it suppresses the warning
    /// and never the behavior. An unrecognized class name is an
    /// [`FfiError::InvalidArgument`].
    pub suppressed_until: HashMap<String, String>,
}

impl FfiNotifyInput {
    /// Parse the foreign record into the core input, rejecting every malformed field rather
    /// than defaulting past it: a mistyped instant that silently became "never" would suppress
    /// an alert forever, which is exactly the failure this alert surface exists to prevent.
    fn parse(self) -> Result<NotifyInput, FfiError> {
        let sync = self
            .last_completed_sync
            .map(|raw| {
                Ok::<_, FfiError>(SyncFacts {
                    last_completed_sync: parse_instant(&raw, "last_completed_sync")?,
                    unsynced_changes: self.unsynced_changes,
                })
            })
            .transpose()?;

        let recovery = self
            .recovery_next_due
            .map(|raw| {
                Ok::<_, FfiError>(RecoveryFacts {
                    next_due: parse_instant(&raw, "recovery_next_due")?,
                    snoozed_until: self
                        .recovery_snoozed_until
                        .as_deref()
                        .map(|raw| parse_instant(raw, "recovery_snoozed_until"))
                        .transpose()?,
                    snooze_budget_spent: self.recovery_snooze_budget_spent,
                })
            })
            .transpose()?;

        let mut suppressed = std::collections::BTreeMap::new();
        for (name, raw) in self.suppressed_until {
            let class = AlertClass::from_wire(&name).ok_or_else(|| FfiError::InvalidArgument {
                message: format!("suppressed_until: `{name}` is not an alert class"),
            })?;
            suppressed.insert(class, parse_instant(&raw, "suppressed_until")?);
        }

        Ok(NotifyInput {
            sync,
            recovery,
            quota: self.quota_state.map(|state| QuotaFacts {
                state: state.into(),
            }),
            quarantine_pending: self.quarantine_pending,
            drops_pending: self.drops_pending,
            suppressed,
        })
    }
}

/// Parse one RFC 3339 instant, naming the field so a foreign caller can find its own bug.
fn parse_instant(raw: &str, field: &str) -> Result<Timestamp, FfiError> {
    raw.parse::<Timestamp>()
        .map_err(|err| FfiError::InvalidArgument {
            message: format!("{field}: `{raw}` is not an RFC 3339 timestamp: {err}"),
        })
}

/// Every alert that is true at `now`, in delivery order.
///
/// Pure and offline: no network call, no library open, no clock read — `now` is the caller's, so
/// the same input always gives the same answer and an app can drive it from a test clock.
///
/// # Errors
///
/// [`FfiError::InvalidArgument`] if `now` or any instant in `input` is not RFC 3339, or if
/// `suppressed_until` names something that is not an alert class.
#[uniffi::export]
pub fn evaluate_alerts(input: FfiNotifyInput, now: String) -> Result<Vec<FfiAlert>, FfiError> {
    let now = parse_instant(&now, "now")?;
    Ok(notify::evaluate(&input.parse()?, now)
        .into_iter()
        .map(FfiAlert::from)
        .collect())
}

/// The next instant to arm a local notification for (RFC 3339), or `None` when there is nothing
/// to arm — in which case cancel any timer this class holds.
///
/// Recompute this after **any** state change and cancel-then-arm if the value moved. Only the
/// two classes whose deadline a device can compute alone are ever returned; the other three
/// depend on server state and surface at next app launch.
///
/// # Errors
///
/// As [`evaluate_alerts`].
#[uniffi::export]
pub fn next_alert_deadline(input: FfiNotifyInput, now: String) -> Result<Option<String>, FfiError> {
    let now = parse_instant(&now, "now")?;
    Ok(notify::next_deadline(&input.parse()?, now).map(|deadline| deadline.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed base instant, and the two-week threshold expressed against it.
    const BASE: &str = "2023-11-14T22:13:20Z";
    const BASE_PLUS_14D: &str = "2023-11-28T22:13:20Z";

    fn stale_input() -> FfiNotifyInput {
        FfiNotifyInput {
            last_completed_sync: Some(BASE.to_owned()),
            unsynced_changes: 3,
            ..FfiNotifyInput::default()
        }
    }

    /// The whole round trip: a foreign record in, alert classes and an RFC 3339 deadline out.
    #[test]
    fn evaluate_alerts_round_trips_the_boundary() {
        let alerts = evaluate_alerts(stale_input(), BASE_PLUS_14D.to_owned()).unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].class, FfiAlertClass::SyncStale);
        assert_eq!(alerts[0].severity, FfiAlertSeverity::Warning);
        assert_eq!(alerts[0].deadline.as_deref(), Some(BASE_PLUS_14D));
        assert_eq!(alerts[0].params["count"], "3");
        assert_eq!(alerts[0].params["days_behind"], "14");
    }

    /// One second before the threshold nothing fires, and the deadline to arm is the threshold.
    #[test]
    fn next_alert_deadline_arms_the_threshold() {
        let before = "2023-11-28T22:13:19Z".to_owned();
        assert!(
            evaluate_alerts(stale_input(), before.clone())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            next_alert_deadline(stale_input(), before)
                .unwrap()
                .as_deref(),
            Some(BASE_PLUS_14D)
        );
        // Once it has fired there is nothing left to schedule.
        assert_eq!(
            next_alert_deadline(stale_input(), BASE_PLUS_14D.to_owned()).unwrap(),
            None
        );
    }

    /// The default record is the "learned nothing" input on both functions.
    #[test]
    fn default_input_is_silent_across_the_boundary() {
        assert!(
            evaluate_alerts(FfiNotifyInput::default(), BASE.to_owned())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            next_alert_deadline(FfiNotifyInput::default(), BASE.to_owned()).unwrap(),
            None
        );
    }

    /// Quota and count classes cross with their parameters and without a deadline.
    #[test]
    fn quota_and_count_classes_cross_the_boundary() {
        let input = FfiNotifyInput {
            quota_state: Some(FfiQuotaAdvisory::GraceExpired),
            quarantine_pending: 2,
            drops_pending: 1,
            ..FfiNotifyInput::default()
        };
        let alerts = evaluate_alerts(input, BASE.to_owned()).unwrap();
        let classes: Vec<_> = alerts.iter().map(|a| a.class).collect();
        assert_eq!(
            classes,
            [
                FfiAlertClass::QuotaGraceExpiring,
                FfiAlertClass::QuarantinePending,
                FfiAlertClass::DropPending,
            ]
        );
        for alert in &alerts {
            assert_eq!(alert.deadline, None);
        }
        assert_eq!(alerts[0].params["grace"], "expired");
        assert_eq!(alerts[1].params["count"], "2");
        assert_eq!(alerts[2].params["count"], "1");
    }

    /// A suppressed class crosses as a wire name and removes the class from both answers.
    #[test]
    fn suppression_crosses_as_a_wire_name() {
        let mut input = stale_input();
        input
            .suppressed_until
            .insert("sync_stale".to_owned(), "2999-01-01T00:00:00Z".to_owned());
        assert!(
            evaluate_alerts(input.clone(), BASE_PLUS_14D.to_owned())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            next_alert_deadline(input, BASE_PLUS_14D.to_owned()).unwrap(),
            None
        );
    }

    /// Every malformed input is a typed `InvalidArgument`, never a panic and never a default.
    #[test]
    fn malformed_input_is_invalid_argument() {
        let cases: Vec<(FfiNotifyInput, String, &str)> = vec![
            (FfiNotifyInput::default(), "not-a-time".to_owned(), "now"),
            (
                FfiNotifyInput {
                    last_completed_sync: Some("yesterday".to_owned()),
                    ..FfiNotifyInput::default()
                },
                BASE.to_owned(),
                "last_completed_sync",
            ),
            (
                FfiNotifyInput {
                    recovery_next_due: Some("soon".to_owned()),
                    ..FfiNotifyInput::default()
                },
                BASE.to_owned(),
                "recovery_next_due",
            ),
            (
                FfiNotifyInput {
                    recovery_next_due: Some(BASE.to_owned()),
                    recovery_snoozed_until: Some("later".to_owned()),
                    ..FfiNotifyInput::default()
                },
                BASE.to_owned(),
                "recovery_snoozed_until",
            ),
            (
                FfiNotifyInput {
                    suppressed_until: HashMap::from([(
                        "telemetry_ready".to_owned(),
                        BASE.to_owned(),
                    )]),
                    ..FfiNotifyInput::default()
                },
                BASE.to_owned(),
                "not an alert class",
            ),
            (
                FfiNotifyInput {
                    suppressed_until: HashMap::from([(
                        "sync_stale".to_owned(),
                        "whenever".to_owned(),
                    )]),
                    ..FfiNotifyInput::default()
                },
                BASE.to_owned(),
                "suppressed_until",
            ),
        ];
        for (input, now, needle) in cases {
            let err = evaluate_alerts(input.clone(), now.clone())
                .expect_err("malformed input must not be accepted");
            match &err {
                FfiError::InvalidArgument { message } => {
                    assert!(message.contains(needle), "{message} does not name {needle}");
                }
                other => panic!("expected InvalidArgument, got {other:?}"),
            }
            // The same rejection on the deadline function; neither is the lenient one.
            assert!(matches!(
                next_alert_deadline(input, now),
                Err(FfiError::InvalidArgument { .. })
            ));
        }
    }

    /// The projection from the SDK's own scheduler composes with the exported function, which
    /// is the wiring an app actually uses.
    #[test]
    fn recovery_cadence_projection_composes_with_the_export() {
        use crate::recovery::RecoveryCadence;

        let base: Timestamp = BASE.parse().unwrap();
        let cadence = RecoveryCadence::armed_at_setup(base);
        let due = cadence.next_due();
        let facts = cadence.notify_facts(due);

        let input = FfiNotifyInput {
            recovery_next_due: Some(facts.next_due.to_string()),
            recovery_snoozed_until: facts.snoozed_until.map(|t| t.to_string()),
            recovery_snooze_budget_spent: facts.snooze_budget_spent,
            ..FfiNotifyInput::default()
        };
        let alerts = evaluate_alerts(input, due.to_string()).unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].class, FfiAlertClass::RecoveryCheckDue);
        assert_eq!(alerts[0].params["snooze_budget"], "available");
    }
}
