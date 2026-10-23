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

/// `from_wire` is exactly the inverse of `as_str`, and rejects everything else.
#[test]
fn from_wire_round_trips_and_rejects_the_rest() {
    for class in AlertClass::ALL {
        assert_eq!(AlertClass::from_wire(class.as_str()), Some(class));
    }
    for name in [
        "",
        "SyncStale",
        "sync-stale",
        "sync_stale ",
        "telemetry_ready",
    ] {
        assert_eq!(AlertClass::from_wire(name), None, "{name:?}");
    }
}

/// SSoT: unknown classes are rejected as structural errors.
#[test]
fn unknown_class_is_a_structural_error() {
    assert!(serde_json::from_str::<AlertClass>("\"telemetry_ready\"").is_err());
    assert!(serde_json::from_str::<AlertSeverity>("\"critical\"").is_err());
}

/// Every class's severity is pinned, so changing one arm of the mapping fails here rather
/// than silently changing how loudly a shipped client presents an alert. The three
/// `Warning`s are the states where something is already degraded or being refused; the
/// three `Advisory`s are the states where nothing is failing yet.
#[test]
fn severity_is_pinned_for_every_class() {
    let expected = [
        (AlertClass::SyncStale, AlertSeverity::Warning),
        (AlertClass::RecoveryCheckDue, AlertSeverity::Advisory),
        (AlertClass::QuotaSoft, AlertSeverity::Advisory),
        (AlertClass::QuotaGraceExpiring, AlertSeverity::Warning),
        (AlertClass::QuarantinePending, AlertSeverity::Warning),
        (AlertClass::DropPending, AlertSeverity::Advisory),
    ];
    // Zipping against ALL also pins the delivery order the table is written in.
    for (class, (expected_class, severity)) in AlertClass::ALL.into_iter().zip(expected) {
        assert_eq!(class, expected_class, "class order changed");
        assert_eq!(class.severity(), severity, "{}", class.as_str());
    }
    assert_eq!(AlertClass::ALL.len(), expected.len());
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
