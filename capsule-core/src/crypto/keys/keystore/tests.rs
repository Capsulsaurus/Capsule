use serde::Serialize;

use super::*;
use crate::ambient::testing::TestIds;
use crate::crypto::keys::kem_p256::{
    DEK_P256_PUBLIC_LEN, MockP256KeyAgreement, encapsulate_to_p256_public,
};

// Fast Argon2 params keep keystore tests quick; the production tier table is asserted
// in `primitives` without paying the 128–512 MiB hashing cost.
fn fast() -> Argon2Params {
    Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    }
}

#[test]
fn account_create_save_unlock_round_trip() {
    let acct = Account::create(TestIds::new(101).ids());
    let ik_vk = acct.user_ik.verifying_key();
    let dsk_vk = acct.device.dsk.verifying_key();
    let default_album = acct.master.derive_default_album_id();

    let file = acct.to_file_with(b"passphrase", fast()).unwrap();
    let restored = file.unlock(b"passphrase").unwrap();

    assert_eq!(restored.user_id, acct.user_id);
    assert_eq!(restored.device.device_id, acct.device.device_id);
    // Identity and device verifying keys survive the round trip.
    assert_eq!(restored.user_ik.verifying_key(), ik_vk);
    assert_eq!(restored.device.dsk.verifying_key(), dsk_vk);
    // The master key still derives the same default album id.
    assert_eq!(restored.master.derive_default_album_id(), default_album);
}

/// **#467.** An account assembled from recovered parts keeps every account-level key —
/// `user_id`, master (proved by the default album id), user IK — and mints a fresh device,
/// and it persists and unlocks like any other.
#[test]
fn from_parts_keeps_the_account_and_mints_a_new_device() {
    let original = Account::create(TestIds::new(102).ids());
    let sealed = original.sealed_identity();
    let user_ik = Account::open_sealed_identity(&original.master, &sealed).unwrap();

    let recovered = Account::from_parts(
        TestIds::new(103).ids(),
        original.user_id,
        MasterKey::from_bytes(*original.master.as_bytes()),
        user_ik,
        DeviceDek::Software(DekKeypair::generate()),
    );
    assert_eq!(recovered.user_id, original.user_id);
    assert_eq!(
        recovered.master.derive_default_album_id(),
        original.master.derive_default_album_id()
    );
    assert_eq!(
        recovered.user_ik.verifying_key(),
        original.user_ik.verifying_key()
    );
    assert_ne!(recovered.device.device_id, original.device.device_id);
    assert_ne!(
        recovered.device.dsk.verifying_key(),
        original.device.dsk.verifying_key()
    );

    let file = recovered.to_file_with(b"pw", fast()).unwrap();
    let reopened = file.unlock(b"pw").unwrap();
    assert_eq!(reopened.user_id, original.user_id);
    assert_eq!(
        reopened.user_ik.verifying_key(),
        original.user_ik.verifying_key()
    );
}

/// A sealed identity opens only under the master key that sealed it.
#[test]
fn a_sealed_identity_refuses_a_foreign_master_key() {
    let acct = Account::create(TestIds::new(104).ids());
    let sealed = acct.sealed_identity();
    assert!(matches!(
        Account::open_sealed_identity(&MasterKey::generate(), &sealed),
        Err(CryptoError::Auth(_))
    ));
}

/// The domain prefix keeps another 64-byte seed sealed under the same master key — the
/// account file's sealed DSK — from being accepted as the user IK.
#[test]
fn a_sealed_device_key_is_not_accepted_as_an_identity() {
    let acct = Account::create(TestIds::new(105).ids());
    let file = acct.to_file_with(b"pw", fast()).unwrap();
    for not_an_identity in [&file.sealed_dsk, &file.sealed_ik] {
        assert!(matches!(
            Account::open_sealed_identity(&acct.master, not_an_identity),
            Err(CryptoError::Malformed(_))
        ));
    }
}

#[test]
fn wrong_passphrase_fails_to_unlock() {
    let acct = Account::create(TestIds::new(106).ids());
    let file = acct.to_file_with(b"right", fast()).unwrap();
    assert!(file.unlock(b"wrong").is_err());
}

// ── the DEK's classical half in a secure element ──────────────────────────────

/// **The keystore half of the acceptance.** An account created against an element seals and
/// unlocks: the published DEK public key is byte-identical across the round trip, and a
/// secret encapsulated to it is recovered *through the element's ECDH*. That is the whole
/// claim — the classical half was never in the account file, yet the DEK still works.
#[test]
fn hardware_bound_account_round_trips_lock_and_unlock_through_the_element() {
    let element = Arc::new(MockP256KeyAgreement::new([0x5F; 32], false));
    let acct = Account::create_with_hardware_dek(
        TestIds::new(107).ids(),
        element.clone(),
        "device-dek".into(),
    )
    .unwrap();
    assert!(acct.device.dek.is_hardware_bound());
    let published = acct.device.dek.public_bytes();
    assert_eq!(
        published.len(),
        DEK_P256_PUBLIC_LEN,
        "the P-256 hybrid form"
    );

    let file = acct.to_file_with(b"pw", fast()).unwrap();
    assert_eq!(
        file.dek_binding,
        DekBinding::Hardware {
            key_alias: "device-dek".into()
        }
    );

    let restored = file.unlock_with_element(b"pw", Some(element)).unwrap();
    assert!(restored.device.dek.is_hardware_bound());
    assert_eq!(
        restored.device.dek.public_bytes(),
        published,
        "the same element + the same sealed ML-KEM seed republish the same DEK"
    );

    // The recovered DEK genuinely decapsulates: sender-side encapsulation is pure software,
    // the receiver side runs the classical half inside the element.
    let (ct, sent) = encapsulate_to_p256_public(&published).unwrap();
    assert_eq!(restored.device.dek.decapsulate(&ct).unwrap(), sent);
}

/// The binding is not decorative: without the element the account does not unlock at all,
/// rather than quietly falling back to a software key that would decapsulate nothing.
#[test]
fn hardware_bound_account_refuses_to_unlock_without_the_element() {
    let element = Arc::new(MockP256KeyAgreement::new([0x11; 32], false));
    let file =
        Account::create_with_hardware_dek(TestIds::new(108).ids(), element, "device-dek".into())
            .unwrap()
            .to_file_with(b"pw", fast())
            .unwrap();

    assert!(
        matches!(file.unlock(b"pw"), Err(CryptoError::Key(_))),
        "a hardware-bound DEK must not silently degrade to software"
    );
}

/// A *different* element cannot stand in for the enrolled one: it republishes a different
/// public key, and a ciphertext sealed to the real device does not open under it. This is
/// what "bound to hardware" means operationally.
#[test]
fn a_foreign_element_cannot_impersonate_the_enrolled_one() {
    let real = Arc::new(MockP256KeyAgreement::new([0x22; 32], false));
    let acct =
        Account::create_with_hardware_dek(TestIds::new(109).ids(), real, "device-dek".into())
            .unwrap();
    let published = acct.device.dek.public_bytes();
    let (ct, sent) = encapsulate_to_p256_public(&published).unwrap();
    let file = acct.to_file_with(b"pw", fast()).unwrap();

    let impostor = Arc::new(MockP256KeyAgreement::new([0x33; 32], false));
    let wrong = file.unlock_with_element(b"pw", Some(impostor)).unwrap();
    assert_ne!(wrong.device.dek.public_bytes(), published);
    assert_ne!(
        wrong.device.dek.decapsulate(&ct).unwrap(),
        sent,
        "the ML-KEM half alone must not recover the secret"
    );
}

/// The software fallback is untouched: it stays the default, records itself as such, needs no
/// element, and ignores one if handed a spurious one (the file's binding decides, not the
/// caller).
#[test]
fn software_dek_accounts_are_unaffected_by_the_hardware_seam() {
    let acct = Account::create(TestIds::new(110).ids());
    assert!(!acct.device.dek.is_hardware_bound());
    let published = acct.device.dek.public_bytes();
    let file = acct.to_file_with(b"pw", fast()).unwrap();
    assert_eq!(file.dek_binding, DekBinding::Software);

    let restored = file.unlock(b"pw").unwrap();
    assert_eq!(restored.device.dek.public_bytes(), published);
    let (ct, sent) = crate::crypto::keys::encapsulate_to_public(&published).unwrap();
    assert_eq!(restored.device.dek.decapsulate(&ct).unwrap(), sent);

    // Handing a software account an element changes nothing.
    let element = Arc::new(MockP256KeyAgreement::new([0x44; 32], false));
    let with_element = file.unlock_with_element(b"pw", Some(element)).unwrap();
    assert!(!with_element.device.dek.is_hardware_bound());
    assert_eq!(with_element.device.dek.public_bytes(), published);
}

/// **Migration.** An account file from before hardware DEKs has no `dek_binding` field at
/// all. It must still open, as software — proved by decoding a struct that literally lacks
/// the field, which is what those bytes on disk are.
#[test]
fn a_pre_s_f8_account_file_without_a_binding_field_opens_as_software() {
    /// The exact `AccountFile` shape from before hardware DEKs.
    #[derive(Serialize)]
    struct LegacyAccountFile {
        user_id: Uuid,
        device_id: Uuid,
        wrapped_master: pwkdf::WrappedSecret,
        #[serde(with = "serde_bytes")]
        sealed_ik: Vec<u8>,
        #[serde(with = "serde_bytes")]
        sealed_dsk: Vec<u8>,
        #[serde(with = "serde_bytes")]
        sealed_dek: Vec<u8>,
    }

    let acct = Account::create(TestIds::new(111).ids());
    let published = acct.device.dek.public_bytes();
    let file = acct.to_file_with(b"pw", fast()).unwrap();
    let legacy = LegacyAccountFile {
        user_id: file.user_id,
        device_id: file.device_id,
        wrapped_master: file.wrapped_master.clone(),
        sealed_ik: file.sealed_ik.clone(),
        sealed_dsk: file.sealed_dsk.clone(),
        sealed_dek: file.sealed_dek.clone(),
    };

    let bytes = crate::cbor::to_canonical_vec(&legacy).unwrap();
    let decoded: AccountFile = crate::cbor::from_slice(&bytes).unwrap();
    assert_eq!(decoded.dek_binding, DekBinding::Software);
    assert_eq!(
        decoded.unlock(b"pw").unwrap().device.dek.public_bytes(),
        published
    );
}

#[test]
fn account_file_serializes_canonically() {
    let acct = Account::create(TestIds::new(112).ids());
    let file = acct.to_file_with(b"pw", fast()).unwrap();
    let bytes = crate::cbor::to_canonical_vec(&file).unwrap();
    let back: AccountFile = crate::cbor::from_slice(&bytes).unwrap();
    assert_eq!(back.user_id, file.user_id);
    assert_eq!(back.sealed_dek, file.sealed_dek);
}
