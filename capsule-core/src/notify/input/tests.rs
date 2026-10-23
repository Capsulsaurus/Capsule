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
