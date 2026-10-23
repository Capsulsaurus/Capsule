use capsule_core::crypto::keys::{DirectoryError, MasterKey};
use tempfile::TempDir;

use super::super::{FAST_PARAMS, LifecycleError, fast_workspace};
use super::*;

/// A and a second device B of the same account, B built from A's master key and sealed
/// identity the way a recovered device is.
fn two_devices() -> (TempDir, TempDir, Workspace, Workspace) {
    let lib_a = TempDir::new().unwrap();
    let lib_b = TempDir::new().unwrap();
    let a = fast_workspace(lib_a.path());
    let b = Workspace::create_from_master(
        lib_b.path(),
        b"device b passphrase",
        FAST_PARAMS,
        MasterKey::from_bytes(*a.account.master.as_bytes()),
        a.user_id(),
        &a.sealed_identity(),
    )
    .unwrap();
    (lib_a, lib_b, a, b)
}

/// Relay a payload through its wire form, as the server would carry it.
fn relay(payload: &EnrollmentPayload) -> EnrollmentPayload {
    EnrollmentPayload::decode(&payload.encode().unwrap()).unwrap()
}

/// **The ceremony, end to end, with nothing on the relay.** The commitment opens, both
/// codes match, A cross-signs B, and B verifies the directory it receives under the identity
/// key A advertised — finding itself and A in it.
#[test]
fn an_untampered_ceremony_admits_the_new_device() {
    let (_la, _lb, mut a, b) = two_devices();
    let before = a.device_directory().core.directory_version;

    let a_keys = a.enrollment_initiator_keys();
    let b_keys = b.enrollment_enrollee_keys();
    let EnrollmentPayload::Commitment(commitment) =
        relay(&EnrollmentPayload::Commitment(b_keys.commitment()))
    else {
        panic!("commitment did not round-trip");
    };
    let EnrollmentPayload::InitiatorKeys(a_seen_by_b) =
        relay(&EnrollmentPayload::InitiatorKeys(a_keys.clone()))
    else {
        panic!("initiator keys did not round-trip");
    };
    let EnrollmentPayload::EnrolleeKeys(b_seen_by_a) =
        relay(&EnrollmentPayload::EnrolleeKeys(b_keys.clone()))
    else {
        panic!("enrollee keys did not round-trip");
    };
    assert!(b_seen_by_a.matches_commitment(&commitment));

    assert_eq!(a_seen_by_b, a_keys);
    let code_on_a = enrollment_safety_code(&a_keys, &b_seen_by_a);
    let code_on_b = enrollment_safety_code(&a_seen_by_b, &b_keys);
    assert_eq!(code_on_a, code_on_b);

    let directory = a
        .cross_sign_device(b_seen_by_a.into_device_entry("2026-06-01T00:00:00Z".into()))
        .unwrap();
    assert_eq!(directory.core.directory_version, before + 1);
    assert_eq!(a.device_directory(), &directory);

    let EnrollmentPayload::Directory(received) = relay(&EnrollmentPayload::Directory(directory))
    else {
        panic!("directory did not round-trip");
    };
    assert!(received.verify(&a_seen_by_b.user_ik_public));
    assert_eq!(received.core.user_id, a_seen_by_b.user_id);
    let entry = received
        .device(&b.device_id())
        .expect("B is in the directory");
    assert_eq!(entry.dsk_public, b.enrollment_enrollee_keys().dsk_public);
    assert_eq!(
        entry.dek_public.as_ref().map(DekPublic::as_bytes),
        Some(b.device_dek_public().as_slice())
    );
    assert!(received.device(&a.device_id()).is_some());
    // A peer holding only the directory can now wrap a key to B.
    let (ciphertext, sent) = entry.encapsulate().unwrap();
    assert_eq!(b.device_dek_decapsulate(&ciphertext).unwrap(), sent);
}

/// **The MITM abort.** A relay that swaps in its own device's keys toward A leaves the two
/// devices showing different codes.
#[test]
fn a_relay_that_swaps_the_enrollee_keys_diverges_the_codes() {
    let (_la, _lb, a, b) = two_devices();
    let lib_m = TempDir::new().unwrap();
    let mallory = fast_workspace(lib_m.path());

    let a_keys = a.enrollment_initiator_keys();
    let b_keys = b.enrollment_enrollee_keys();
    let swapped = mallory.enrollment_enrollee_keys();
    assert_ne!(
        enrollment_safety_code(&a_keys, &swapped),
        enrollment_safety_code(&a_keys, &b_keys)
    );
    // And keys revealed after the fact do not open B's commitment.
    assert!(!swapped.matches_commitment(&b_keys.commitment()));
    // Nor does a relay that swaps A's keys toward B go unnoticed.
    assert_ne!(
        enrollment_safety_code(&mallory.enrollment_initiator_keys(), &b_keys),
        enrollment_safety_code(&a_keys, &b_keys)
    );
}

/// **The offline-grinding defence.** A's device keys are already published, so a man in the
/// middle knows them before it commits toward A. A fresh nonce per ceremony means it still
/// cannot predict the code A will show: the same static keys under another nonce give
/// another code. Likewise B's nonce rides under its commitment, so B's revealed keys from an
/// earlier attempt do not predict the next attempt's code or open its commitment.
#[test]
fn each_ceremony_draws_fresh_nonces_that_the_code_and_commitment_cover() {
    let (_la, _lb, a, b) = two_devices();
    let a_first = a.enrollment_initiator_keys();
    let a_second = a.enrollment_initiator_keys();
    let b_first = b.enrollment_enrollee_keys();
    let b_second = b.enrollment_enrollee_keys();

    // The static keys are the same; only the nonce is fresh.
    assert_eq!(a_first.dsk_public, a_second.dsk_public);
    assert_eq!(b_first.dsk_public, b_second.dsk_public);
    assert_ne!(a_first.nonce, a_second.nonce);
    assert_ne!(b_first.nonce, b_second.nonce);

    // A man in the middle holding A's published keys and an old nonce mispredicts the code.
    assert_ne!(
        enrollment_safety_code(&a_first, &b_first),
        enrollment_safety_code(&a_second, &b_first)
    );
    assert_ne!(
        enrollment_safety_code(&a_first, &b_first),
        enrollment_safety_code(&a_first, &b_second)
    );
    // B's commitment binds its nonce, not just its keys.
    assert_ne!(b_first.commitment(), b_second.commitment());
    assert!(!b_second.matches_commitment(&b_first.commitment()));
}

/// A refused cross-sign surfaces the directory's reason and leaves A's directory as it was.
#[test]
fn cross_signing_a_listed_device_is_refused_and_changes_nothing() {
    let (_la, _lb, mut a, b) = two_devices();
    let entry = b
        .enrollment_enrollee_keys()
        .into_device_entry("2026-06-01T00:00:00Z".into());
    a.cross_sign_device(entry.clone()).unwrap();
    let admitted = a.device_directory().clone();

    let err = a.cross_sign_device(entry).unwrap_err();
    assert!(matches!(
        err,
        LifecycleError::Directory(DirectoryError::DeviceAlreadyListed(id)) if id == b.device_id()
    ));
    assert_eq!(a.device_directory(), &admitted);

    // A's own device cannot be re-enrolled either.
    let own = a
        .enrollment_enrollee_keys()
        .into_device_entry("2026-06-01T00:00:00Z".into());
    assert!(matches!(
        a.cross_sign_device(own),
        Err(LifecycleError::Directory(
            DirectoryError::DeviceAlreadyListed(_)
        ))
    ));
    assert_eq!(a.device_directory(), &admitted);
}

/// **#562.** A cross-signed device and the version it bumped survive a reopen of A, and the
/// next device A cross-signs lands on top of it rather than on a rebuilt version-1 directory.
#[test]
fn a_cross_signed_device_survives_a_reopen() {
    let (la, _lb, mut a, b) = two_devices();
    let admitted = a
        .cross_sign_device(
            b.enrollment_enrollee_keys()
                .into_device_entry("2026-06-01T00:00:00Z".into()),
        )
        .unwrap();
    let directory_path = Workspace::directory_path(la.path());
    assert!(
        !crate::utils::paths::tmp_path(&directory_path).exists(),
        "the staged directory was renamed into place, not left beside it"
    );
    let a_id = a.device_id();
    drop(a);

    let mut reopened = Workspace::open(la.path(), b"passphrase", FAST_PARAMS).unwrap();
    assert_eq!(reopened.device_id(), a_id);
    assert_eq!(reopened.device_directory(), &admitted);
    assert!(reopened.device_directory().device(&b.device_id()).is_some());

    let lib_c = TempDir::new().unwrap();
    let c = fast_workspace(lib_c.path());
    let next = reopened
        .cross_sign_device(
            c.enrollment_enrollee_keys()
                .into_device_entry("2026-06-02T00:00:00Z".into()),
        )
        .unwrap();
    assert_eq!(
        next.core.directory_version,
        admitted.core.directory_version + 1
    );
    assert!(next.device(&b.device_id()).is_some(), "B was not forgotten");
    assert!(next.device(&c.device_id()).is_some());
}

/// **#562.** A cross-sign that cannot be persisted is refused and adopts nothing, so memory
/// never runs ahead of the directory a reopen would restore.
#[test]
fn a_cross_sign_that_cannot_be_persisted_changes_nothing() {
    let (la, _lb, mut a, b) = two_devices();
    let before = a.device_directory().clone();
    // A directory where the file belongs makes the rename fail.
    let directory_path = Workspace::directory_path(la.path());
    std::fs::remove_file(&directory_path).unwrap();
    std::fs::create_dir(&directory_path).unwrap();

    let err = a
        .cross_sign_device(
            b.enrollment_enrollee_keys()
                .into_device_entry("2026-06-01T00:00:00Z".into()),
        )
        .unwrap_err();
    assert!(matches!(err, LifecycleError::Io(_)), "{err:?}");
    assert_eq!(a.device_directory(), &before);
}

/// The entry A signs carries A's clock and no revocation, whatever B might have hoped for.
#[test]
fn the_cross_signed_entry_is_live_and_dated_by_the_initiator() {
    let (_la, _lb, _a, b) = two_devices();
    let entry = b
        .enrollment_enrollee_keys()
        .into_device_entry("2026-06-01T00:00:00Z".into());
    assert_eq!(entry.added_at, "2026-06-01T00:00:00Z");
    assert_eq!(entry.revoked_at, None);
    assert_eq!(entry.device_id, b.device_id());
}
