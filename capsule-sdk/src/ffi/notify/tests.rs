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
                suppressed_until: HashMap::from([("telemetry_ready".to_owned(), BASE.to_owned())]),
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
                suppressed_until: HashMap::from([("sync_stale".to_owned(), "whenever".to_owned())]),
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
