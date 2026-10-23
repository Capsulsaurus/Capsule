use super::*;

const ID: &str = "9774d56d682e549c";

#[test]
fn deterministic_across_invocations() {
    let u = Uuid::from_u128(0x1234);
    assert_eq!(
        cohort_hash(u, PlatformTag::Android, ID),
        cohort_hash(u, PlatformTag::Android, ID),
    );
}

#[test]
fn user_id_fold_makes_accounts_unlinkable() {
    // Same physical device (same primary id), two accounts → distinct
    // hashes: the cross-account correlation surface is removed at the source.
    let a = cohort_hash(Uuid::from_u128(1), PlatformTag::Android, ID);
    let b = cohort_hash(Uuid::from_u128(2), PlatformTag::Android, ID);
    assert_ne!(a, b);
}

#[test]
fn platform_and_label_are_domain_separating() {
    let u = Uuid::from_u128(0x1234);
    assert_ne!(
        cohort_hash(u, PlatformTag::Android, ID),
        cohort_hash(u, PlatformTag::Linux, ID),
    );
    // Structured encoding, not concatenation: shifting bytes between
    // components must change the hash.
    assert_ne!(
        cohort_hash(u, PlatformTag::Android, "ab"),
        cohort_hash(u, PlatformTag::Android, "a"),
    );
}
