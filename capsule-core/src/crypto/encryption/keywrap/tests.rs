use super::*;
use crate::crypto::encryption::{decrypt_asset_vec, encrypt_asset_vec_with_prefix};

const K: [u8; 32] = [0x77; 32];

#[test]
fn seal_unseal_round_trip() {
    let amk = Amk::from_bytes([0x11; 32]);
    let file_id = Uuid::from_u128(0xF11E);
    let wrapped = seal_file_key(&amk, &file_id, &K);
    assert_eq!(wrapped.len(), WRAPPED_FILE_KEY_LEN);
    assert_eq!(unseal_file_key(&amk, &file_id, &wrapped).unwrap(), K);
}

#[test]
fn fresh_wrap_nonce_per_seal_still_unseals() {
    let amk = Amk::from_bytes([0x11; 32]);
    let file_id = Uuid::from_u128(1);
    let a = seal_file_key(&amk, &file_id, &K);
    let b = seal_file_key(&amk, &file_id, &K);
    // Distinct wrap_nonce → distinct wrapped bytes even under a constant file_id + K.
    assert_ne!(a, b, "each seal must draw a fresh wrap_nonce");
    assert_ne!(&a[..WRAP_NONCE_LEN], &b[..WRAP_NONCE_LEN]);
    assert_eq!(unseal_file_key(&amk, &file_id, &a).unwrap(), K);
    assert_eq!(unseal_file_key(&amk, &file_id, &b).unwrap(), K);
}

#[test]
fn tampered_nonce_or_ciphertext_fails_auth() {
    let amk = Amk::from_bytes([0x11; 32]);
    let file_id = Uuid::from_u128(1);
    let wrapped = seal_file_key(&amk, &file_id, &K);

    // Flip a ciphertext/tag byte.
    let mut t = wrapped.clone();
    let last = t.len() - 1;
    t[last] ^= 0x01;
    assert!(matches!(
        unseal_file_key(&amk, &file_id, &t),
        Err(CryptoError::Auth(_))
    ));

    // Flip a nonce byte → a different wrap key is derived → open fails.
    let mut t = wrapped.clone();
    t[0] ^= 0x01;
    assert!(matches!(
        unseal_file_key(&amk, &file_id, &t),
        Err(CryptoError::Auth(_))
    ));
}

#[test]
fn wrong_amk_or_file_id_rejects() {
    let amk = Amk::from_bytes([0x11; 32]);
    let file_id = Uuid::from_u128(1);
    let wrapped = seal_file_key(&amk, &file_id, &K);
    // Wrong AMK epoch.
    assert!(matches!(
        unseal_file_key(&Amk::from_bytes([0x99; 32]), &file_id, &wrapped),
        Err(CryptoError::Auth(_))
    ));
    // Wrong file_id → salt divergence → wrong wrap key.
    assert!(matches!(
        unseal_file_key(&amk, &Uuid::from_u128(2), &wrapped),
        Err(CryptoError::Auth(_))
    ));
}

#[test]
fn malformed_length_is_rejected_before_decrypt() {
    let amk = Amk::from_bytes([0x11; 32]);
    let file_id = Uuid::from_u128(1);
    assert!(matches!(
        unseal_file_key(&amk, &file_id, &[0u8; 10]),
        Err(CryptoError::Malformed(_))
    ));
    assert!(matches!(
        unseal_file_key(&amk, &file_id, &[0u8; WRAPPED_FILE_KEY_LEN + 1]),
        Err(CryptoError::Malformed(_))
    ));
}

/// The doc's positive case: a member holding the AMK unwraps `K` and STREAM-decrypts the
/// unchanged ciphertext — decryption is identical to the derived case once `K` is back.
#[test]
fn member_unwrap_then_stream_decrypt_round_trip() {
    let amk = Amk::from_bytes([0x5A; 32]);
    let file_id = Uuid::from_u128(0xF11E);
    // A guest chose a random K and STREAM-encrypted the asset under it.
    let plaintext = b"adopted web-upload drop bytes, sealed under a guest-chosen key";
    let (enc, ct) = encrypt_asset_vec_with_prefix(&K, [1, 2, 3, 4, 5, 6, 7], plaintext);
    let wrapped = seal_file_key(&amk, &file_id, &K);

    // The adopting member recovers K and decrypts the unchanged ciphertext.
    let recovered = unseal_file_key(&amk, &file_id, &wrapped).unwrap();
    assert_eq!(recovered, K);
    let back = decrypt_asset_vec(&recovered, &enc.nonce_prefix, &ct).unwrap();
    assert_eq!(back, plaintext);
}

fn manifest_core(
    file_id: Uuid,
    key_mode: crate::crypto::provenance::KeyMode,
    wrapped: Option<Vec<u8>>,
) -> crate::crypto::provenance::ManifestCore {
    use crate::crypto::hash::Hash32;
    use crate::crypto::keys::AmkVersion;
    use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
    use crate::crypto::provenance::{ASSET_MANIFEST_VERSION, Action, ManifestCore, WrappedFileKey};
    ManifestCore {
        version: ASSET_MANIFEST_VERSION.into(),
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL_VERSION.into(),
        file_id,
        album_id: Uuid::from_u128(0xA1),
        amk_version: AmkVersion(1),
        ciphertext_hash: Hash32([0xCC; 32]),
        plaintext_size: 1,
        chunk_size: 65_520,
        nonce_prefix: [1, 2, 3, 4, 5, 6, 7],
        key_mode,
        wrapped_file_key: wrapped.map(WrappedFileKey),
        metadata_blob_hash: None,
        created_by_user: Uuid::from_u128(0x05E2),
        created_by_device: Uuid::from_u128(0xD1),
        client_version: "t".into(),
        timestamp: "2026-05-31T12:00:00Z".into(),
        action: Action::Create,
        prior_provenance_hash: None,
        retention_until: None,
        upgraded_from: None,
        unknown: Default::default(),
    }
}

/// `recover_file_key` follows the manifest's `key_mode`: `derived` re-derives under the
/// folded nonce prefix, `wrapped` unwraps the carried key, and a `wrapped` manifest that
/// lost its key — or carries one sealed under another AMK — fails closed.
#[test]
fn recover_file_key_honours_key_mode() {
    use crate::crypto::provenance::KeyMode;
    let amk = Amk::from_bytes([0x5A; 32]);
    let file_id = Uuid::from_u128(0xF11E);

    let derived = manifest_core(file_id, KeyMode::Derived, None);
    assert_eq!(
        recover_file_key(&amk, &derived).unwrap(),
        amk.derive_file_key(&file_id, &derived.nonce_prefix),
    );

    let wrapped = manifest_core(
        file_id,
        KeyMode::Wrapped,
        Some(seal_file_key(&amk, &file_id, &K)),
    );
    assert_eq!(recover_file_key(&amk, &wrapped).unwrap(), K);
    assert_ne!(K, amk.derive_file_key(&file_id, &wrapped.nonce_prefix));
    assert!(matches!(
        recover_file_key(&Amk::from_bytes([0x99; 32]), &wrapped),
        Err(CryptoError::Auth(_))
    ));

    let keyless = manifest_core(file_id, KeyMode::Wrapped, None);
    assert!(matches!(
        recover_file_key(&amk, &keyless),
        Err(CryptoError::Malformed(_))
    ));
}
