use super::*;

#[test]
fn seal_open_round_trip() {
    let mk = MasterKey::generate();
    let secret = b"device signing key seeds";
    let blob = mk.seal(secret);
    assert_ne!(&blob[12..], secret, "ciphertext must not equal plaintext");
    assert_eq!(mk.open(&blob).unwrap(), secret);
}

#[test]
fn open_rejects_wrong_key_and_tamper() {
    let mk = MasterKey::generate();
    let blob = mk.seal(b"x");
    assert!(MasterKey::generate().open(&blob).is_err());

    let mut t = blob.clone();
    *t.last_mut().unwrap() ^= 0x01;
    assert!(mk.open(&t).is_err());
}

#[test]
fn default_album_id_is_deterministic_per_master() {
    let mk = MasterKey::from_bytes([5u8; 32]);
    let id1 = mk.derive_default_album_id();
    let id2 = MasterKey::from_bytes([5u8; 32]).derive_default_album_id();
    assert_eq!(
        id1, id2,
        "same master must recompute the same default album id"
    );
    // Different master → different id.
    assert_ne!(
        id1,
        MasterKey::from_bytes([6u8; 32]).derive_default_album_id()
    );
    // It is a well-formed v8 UUID.
    assert_eq!(id1.get_version(), Some(uuid::Version::Custom));
}

#[test]
fn seal_uses_fresh_nonce() {
    let mk = MasterKey::generate();
    assert_ne!(mk.seal(b"same"), mk.seal(b"same"));
}
