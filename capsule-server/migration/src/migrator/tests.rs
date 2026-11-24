use super::{MigrationStatus, statuses, to_apply, to_roll_back};

const APPLIED: MigrationStatus = MigrationStatus::Applied;
const PENDING: MigrationStatus = MigrationStatus::Pending;

fn recorded(versions: &[&str]) -> Vec<String> {
    versions.iter().map(|&version| version.to_owned()).collect()
}

#[test]
fn a_migration_is_applied_exactly_when_the_ledger_records_it() {
    assert_eq!(
        statuses(&["a", "b", "c"], &recorded(&["a", "c"])).unwrap(),
        [APPLIED, PENDING, APPLIED]
    );
}

#[test]
fn an_empty_ledger_leaves_every_migration_pending() {
    assert_eq!(statuses(&["a", "b"], &[]).unwrap(), [PENDING, PENDING]);
}

/// A database migrated by a build that knows a migration this one does not is refused, naming
/// the version, rather than treated as up to date or as something to roll back.
#[test]
fn a_recorded_version_no_migration_defines_is_refused() {
    let error = statuses(&["a"], &recorded(&["a", "z_from_a_newer_build"])).unwrap_err();
    assert!(
        error.to_string().contains("z_from_a_newer_build"),
        "{error}"
    );
}

#[test]
fn up_applies_the_pending_migrations_oldest_first() {
    assert_eq!(to_apply(&[APPLIED, PENDING, PENDING], None), [1, 2]);
}

#[test]
fn up_with_steps_applies_only_that_many() {
    assert_eq!(to_apply(&[PENDING, PENDING, PENDING], Some(2)), [0, 1]);
    assert!(to_apply(&[PENDING], Some(0)).is_empty());
}

#[test]
fn down_rolls_back_the_applied_migrations_newest_first() {
    assert_eq!(to_roll_back(&[APPLIED, APPLIED, PENDING], None), [1, 0]);
}

#[test]
fn down_with_steps_rolls_back_only_the_newest() {
    assert_eq!(to_roll_back(&[APPLIED, APPLIED, APPLIED], Some(1)), [2]);
}

#[test]
fn nothing_to_do_is_an_empty_plan() {
    assert!(to_apply(&[APPLIED, APPLIED], None).is_empty());
    assert!(to_roll_back(&[PENDING, PENDING], None).is_empty());
}
