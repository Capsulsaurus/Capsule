//! The local-alert surface across the FFI boundary (slice `S-D29`, core half): the flattened
//! mirror of [`capsule_core::notify`] plus the two functions the apps call.
//!
//! # Why free functions
//!
//! [`evaluate_alerts`], [`pre_arm_deadlines`] and [`next_alert_deadline`] are free
//! `#[uniffi::export]` functions rather
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

use std::collections::{BTreeMap, BTreeSet, HashMap};

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
    /// Catalog parameters — `count`, `days_behind`, `grace`, and the pair
    /// `recovery_check_due` always carries: `snooze_budget` (`available` / `spent`) and
    /// `recovery` (`check` / `rewrap`).
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

/// One class and the instant its local notification should be armed for.
///
/// A class absent from [`pre_arm_deadlines`]'s result has no timer to hold: cancel whatever it
/// has.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiClassDeadline {
    /// The class whose timer this is — also the `notification.*` catalog key the app renders
    /// when it fires.
    pub class: FfiAlertClass,
    /// When to fire it (RFC 3339).
    pub deadline: String,
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
    #[uniffi(default = None)]
    pub last_completed_sync: Option<String>,
    /// Changes still waiting to reach the server, including originals still pending under a
    /// staged upload policy.
    #[uniffi(default = 0)]
    pub unsynced_changes: u64,
    /// When the next recovery-verification prompt becomes due (RFC 3339). Project it from the
    /// scheduler with
    /// [`RecoveryCadence::notify_facts`](crate::recovery::RecoveryCadence::notify_facts) rather
    /// than computing it here. `None` before recovery is set up, which ignores the other three
    /// `recovery_*` fields.
    #[uniffi(default = None)]
    pub recovery_next_due: Option<String>,
    /// When an active snooze on the recovery prompt expires (RFC 3339), if one is active. A
    /// snooze ending *before* `recovery_next_due` does not make the check due earlier. This is
    /// the canonical place to snooze the recovery check — not
    /// [`suppressed_until`](Self::suppressed_until), which is for the other five classes.
    #[uniffi(default = None)]
    pub recovery_snoozed_until: Option<String>,
    /// Whether the consecutive-snooze budget is spent — the class has degraded to a persistent,
    /// non-blocking badge: still reported, no longer pre-armed.
    #[uniffi(default = false)]
    pub recovery_snooze_budget_spent: bool,
    /// Whether the scheduler has escalated to the guided re-wrap (repeated failures, or the user
    /// declaring the secret lost). Surfaces as the alert's `recovery` parameter, so the app
    /// routes into the re-wrap flow instead of rendering a routine verification prompt.
    #[uniffi(default = false)]
    pub recovery_rewrap_due: bool,
    /// The state from the last `GET /v1/quota`. `None` before the first one.
    #[uniffi(default = None)]
    pub quota_state: Option<FfiQuotaAdvisory>,
    /// How many items sit on the client's quarantine surfaces awaiting a human. **Excludes
    /// pending drops** — they have their own class, so counting them here raises both.
    #[uniffi(default = 0)]
    pub quarantine_pending: u64,
    /// How many guest drops are awaiting review and adoption.
    #[uniffi(default = 0)]
    pub drops_pending: u64,
    /// Per-class **snooze**: class wire name (`sync_stale`, …) to the RFC 3339 instant the
    /// snooze runs until, exclusive. A class snoozed past `now` reports nothing, and its alarm
    /// is **deferred to the snooze end** rather than cancelled — a class snoozed after it fired
    /// must fire again when the snooze expires. Use [`disabled`](Self::disabled) to turn a class
    /// off; do not encode that as a far-future instant here. An unrecognized class name is an
    /// [`FfiError::InvalidArgument`].
    ///
    /// **This map is for the other five classes.** Recovery snoozing belongs in
    /// [`recovery_snoozed_until`](Self::recovery_snoozed_until), which the cadence scheduler
    /// owns together with the budget that bounds it. An entry here for `recovery_check_due` is
    /// honoured as a fallback (the later of the two wins) but is not the canonical channel.
    #[uniffi(default)]
    pub suppressed_until: HashMap<String, String>,
    /// Per-class **disable**: the wire names of the classes the user turned off. They report
    /// nothing and hold no alarm, at any instant. Disabling suppresses the warning and never
    /// the behavior. An unrecognized class name is an [`FfiError::InvalidArgument`].
    #[uniffi(default)]
    pub disabled: Vec<String>,
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

        // Parsed unconditionally, and *before* the `recovery_next_due` branch: a malformed
        // snooze instant is a malformed field whether or not the due date happens to be present,
        // and a validation that only runs on one code path is the one that lets a typo through.
        let snoozed_until = self
            .recovery_snoozed_until
            .as_deref()
            .map(|raw| parse_instant(raw, "recovery_snoozed_until"))
            .transpose()?;
        let recovery = self
            .recovery_next_due
            .map(|raw| {
                Ok::<_, FfiError>(RecoveryFacts {
                    next_due: parse_instant(&raw, "recovery_next_due")?,
                    snoozed_until,
                    snooze_budget_spent: self.recovery_snooze_budget_spent,
                    rewrap_due: self.recovery_rewrap_due,
                })
            })
            .transpose()?;

        let mut suppressed = BTreeMap::new();
        for (name, raw) in self.suppressed_until {
            suppressed.insert(
                parse_class(&name, "suppressed_until")?,
                parse_instant(&raw, "suppressed_until")?,
            );
        }
        let mut disabled = BTreeSet::new();
        for name in self.disabled {
            disabled.insert(parse_class(&name, "disabled")?);
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
            disabled,
        })
    }
}

/// Parse one alert-class wire name against the closed enum, naming the field it came from.
fn parse_class(name: &str, field: &str) -> Result<AlertClass, FfiError> {
    AlertClass::from_wire(name).ok_or_else(|| FfiError::InvalidArgument {
        message: format!("{field}: `{name}` is not an alert class"),
    })
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

/// The instant to arm a local notification for, **per class** — the call an app schedules its
/// `UNCalendarNotificationTrigger` / `AlarmManager` alarms from.
///
/// Reconcile your timers against the result: a class present here should hold exactly one alarm
/// at the returned instant, and a class absent from it should hold none. Recompute after **any**
/// state change; that is the whole of the arm / re-arm / cancel rule on the client side.
///
/// Only the two classes whose deadline a device can compute alone ever appear. The other three
/// depend on server state and surface at next app launch — a real gap the design accepts.
///
/// # Errors
///
/// As [`evaluate_alerts`].
#[uniffi::export]
pub fn pre_arm_deadlines(
    input: FfiNotifyInput,
    now: String,
) -> Result<Vec<FfiClassDeadline>, FfiError> {
    let now = parse_instant(&now, "now")?;
    Ok(notify::pre_arm_deadlines(&input.parse()?, now)
        .into_iter()
        .map(|(class, deadline)| FfiClassDeadline {
            class: class.into(),
            deadline: deadline.to_string(),
        })
        .collect())
}

/// The earliest instant in [`pre_arm_deadlines`] (RFC 3339), or `None` when there is nothing to
/// arm.
///
/// For a host that can hold only one timer. **An app that schedules per class wants
/// [`pre_arm_deadlines`]**: the minimum discards the later class's alarm, and on a device the app
/// never runs on again that alert is simply lost.
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
        assert!(
            pre_arm_deadlines(FfiNotifyInput::default(), BASE.to_owned())
                .unwrap()
                .is_empty()
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

    /// A disabled class crosses as a wire name and leaves every answer empty.
    #[test]
    fn a_disabled_class_crosses_as_a_wire_name() {
        let mut input = stale_input();
        input.disabled.push("sync_stale".to_owned());
        assert!(
            evaluate_alerts(input.clone(), BASE_PLUS_14D.to_owned())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            next_alert_deadline(input.clone(), BASE_PLUS_14D.to_owned()).unwrap(),
            None
        );
        assert!(
            pre_arm_deadlines(input, BASE_PLUS_14D.to_owned())
                .unwrap()
                .is_empty()
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
                    disabled: vec!["telemetry_ready".to_owned()],
                    ..FfiNotifyInput::default()
                },
                BASE.to_owned(),
                "disabled",
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

    /// Each class gets its own armed instant. The single-value convenience keeps only the
    /// earliest, which is why an app that schedules per class must not use it.
    #[test]
    fn pre_arm_deadlines_arms_each_class_independently_across_the_boundary() {
        let recovery_due = "2024-02-12T22:13:20Z"; // ~90 d after BASE, well after the 14 d mark
        let input = FfiNotifyInput {
            recovery_next_due: Some(recovery_due.to_owned()),
            ..stale_input()
        };

        let armed = pre_arm_deadlines(input.clone(), BASE.to_owned()).unwrap();
        assert_eq!(
            armed,
            vec![
                FfiClassDeadline {
                    class: FfiAlertClass::SyncStale,
                    deadline: BASE_PLUS_14D.to_owned(),
                },
                FfiClassDeadline {
                    class: FfiAlertClass::RecoveryCheckDue,
                    deadline: recovery_due.to_owned(),
                },
            ]
        );
        assert_eq!(
            next_alert_deadline(input, BASE.to_owned())
                .unwrap()
                .as_deref(),
            Some(BASE_PLUS_14D),
            "the convenience collapses to the earliest and loses the recovery alarm"
        );
    }

    /// A snooze defers the alarm to its end; a disable cancels it outright.
    #[test]
    fn a_snooze_defers_the_alarm_and_a_disable_cancels_it() {
        let snooze_end = "2023-12-01T22:13:20Z"; // 3 days after the threshold
        let mut snoozed = stale_input();
        snoozed
            .suppressed_until
            .insert("sync_stale".to_owned(), snooze_end.to_owned());
        let armed = pre_arm_deadlines(snoozed, BASE_PLUS_14D.to_owned()).unwrap();
        assert_eq!(armed.len(), 1);
        assert_eq!(armed[0].deadline, snooze_end, "the snooze end is re-armed");

        let mut disabled = stale_input();
        disabled.disabled.push("sync_stale".to_owned());
        assert!(
            pre_arm_deadlines(disabled, BASE.to_owned())
                .unwrap()
                .is_empty(),
            "a disabled class holds no alarm"
        );
    }

    /// A malformed snooze instant is rejected whether or not the due date is present — the
    /// leniency a one-code-path validation would have allowed.
    #[test]
    fn a_malformed_snooze_is_rejected_without_a_due_date() {
        let input = FfiNotifyInput {
            recovery_next_due: None,
            recovery_snoozed_until: Some("later".to_owned()),
            ..FfiNotifyInput::default()
        };
        let err = evaluate_alerts(input.clone(), BASE.to_owned())
            .expect_err("a malformed field is malformed with or without its neighbour");
        assert!(matches!(err, FfiError::InvalidArgument { .. }));
        assert!(matches!(
            pre_arm_deadlines(input, BASE.to_owned()),
            Err(FfiError::InvalidArgument { .. })
        ));
    }

    /// The re-wrap escalation crosses as a parameter, so an app never renders "time for your
    /// periodic check" at the moment the user has declared the secret lost.
    #[test]
    fn the_rewrap_escalation_crosses_the_boundary() {
        for (rewrap, expected) in [(false, "check"), (true, "rewrap")] {
            let input = FfiNotifyInput {
                recovery_next_due: Some(BASE.to_owned()),
                recovery_rewrap_due: rewrap,
                ..FfiNotifyInput::default()
            };
            let alerts = evaluate_alerts(input, BASE.to_owned()).unwrap();
            assert_eq!(alerts.len(), 1);
            assert_eq!(alerts[0].class, FfiAlertClass::RecoveryCheckDue);
            assert_eq!(alerts[0].params["recovery"], expected);
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
            recovery_rewrap_due: facts.rewrap_due,
            ..FfiNotifyInput::default()
        };
        let alerts = evaluate_alerts(input, due.to_string()).unwrap();
        assert_eq!(alerts.len(), 1);
        assert_eq!(alerts[0].class, FfiAlertClass::RecoveryCheckDue);
        assert_eq!(alerts[0].params["snooze_budget"], "available");
        assert_eq!(alerts[0].params["recovery"], "check");
    }
}
