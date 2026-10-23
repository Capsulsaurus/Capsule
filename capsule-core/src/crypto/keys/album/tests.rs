use super::*;

#[test]
fn versions_advance_monotonically() {
    assert_eq!(AmkVersion::FIRST, AmkVersion(1));
    assert_eq!(AmkVersion(3).next(), AmkVersion(4));
    assert!(AmkVersion(2) < AmkVersion(3));
}

const NP: [u8; 7] = [0xEE; 7];
const BN: [u8; 12] = [0xDD; 12];

#[test]
fn file_key_is_deterministic_per_file_and_nonce() {
    let amk = Amk::from_bytes([7u8; 32]);
    let f = Uuid::from_u128(0x1234);
    assert_eq!(amk.derive_file_key(&f, &NP), amk.derive_file_key(&f, &NP));
    assert_eq!(amk.derive_blob_key(&f, &BN), amk.derive_blob_key(&f, &BN));
}

#[test]
fn folded_nonce_rerolls_the_key_under_a_constant_id() {
    // The re-key fold: same file_id/blob_id, a fresh nonce → a different key.
    let amk = Amk::from_bytes([7u8; 32]);
    let f = Uuid::from_u128(0x1234);
    assert_ne!(
        amk.derive_file_key(&f, &[1u8; 7]),
        amk.derive_file_key(&f, &[2u8; 7])
    );
    assert_ne!(
        amk.derive_blob_key(&f, &[1u8; 12]),
        amk.derive_blob_key(&f, &[2u8; 12])
    );
}

#[test]
fn distinct_files_blobs_and_amks_yield_distinct_keys() {
    let amk = Amk::from_bytes([7u8; 32]);
    let f1 = Uuid::from_u128(1);
    let f2 = Uuid::from_u128(2);
    // Different file_id (same nonce) → different key.
    assert_ne!(amk.derive_file_key(&f1, &NP), amk.derive_file_key(&f2, &NP));
    // File vs blob domain separation for the *same* id and folded bytes.
    assert_ne!(amk.derive_file_key(&f1, &NP), amk.derive_blob_key(&f1, &NP));
    // Different AMK epoch → different key.
    assert_ne!(
        amk.derive_file_key(&f1, &NP),
        Amk::from_bytes([8u8; 32]).derive_file_key(&f1, &NP)
    );
}

#[test]
fn wrap_key_folds_the_nonce_and_separates_from_file_domain() {
    let amk = Amk::from_bytes([7u8; 32]);
    let f = Uuid::from_u128(0x1234);
    let n1 = [1u8; 12];
    let n2 = [2u8; 12];
    // Deterministic for a fixed (file_id, wrap_nonce).
    assert_eq!(amk.derive_wrap_key(&f, &n1), amk.derive_wrap_key(&f, &n1));
    // A fresh wrap_nonce re-rolls the wrap key under a constant file_id.
    assert_ne!(amk.derive_wrap_key(&f, &n1), amk.derive_wrap_key(&f, &n2));
    // Distinct info label → distinct domain from the derived file/blob keys, even when
    // the same nonce bytes are folded into every salt.
    assert_ne!(amk.derive_wrap_key(&f, &n1), amk.derive_file_key(&f, &n1));
    assert_ne!(amk.derive_wrap_key(&f, &n1), amk.derive_blob_key(&f, &n1));
}
