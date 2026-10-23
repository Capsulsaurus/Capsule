use super::*;

// Tiny parameters keep the password-hash tests fast; the production tier table is
// asserted in `primitives` without paying the 128–512 MiB hashing cost.
fn fast() -> Argon2Params {
    Argon2Params {
        mem_kib: 64,
        t_cost: 1,
        p_cost: 1,
    }
}

#[test]
fn wrap_unwrap_round_trip() {
    let secret = [0x42u8; 32];
    let blob = wrap_with(&secret, b"correct horse battery staple", fast()).unwrap();
    let out = unwrap(&blob, b"correct horse battery staple").unwrap();
    assert_eq!(out, secret);
}

#[test]
fn wrong_passphrase_is_rejected() {
    let blob = wrap_with(&[1u8; 32], b"right", fast()).unwrap();
    assert_eq!(
        unwrap(&blob, b"wrong"),
        Err(CryptoError::Auth(
            "unwrap: wrong passphrase or corrupt blob"
        ))
    );
}

#[test]
fn tampered_ciphertext_is_rejected() {
    let mut blob = wrap_with(&[9u8; 16], b"pw", fast()).unwrap();
    blob.ciphertext[0] ^= 0x01;
    assert!(unwrap(&blob, b"pw").is_err());
}

#[test]
fn parameters_recorded_in_band_enable_cross_tier_unwrap() {
    // Wrap with one parameter set; unwrap reads the params from the blob, not from a
    // caller-supplied tier — so a blob made on a "desktop" opens on a "phone".
    let desktopish = Argon2Params {
        mem_kib: 96,
        t_cost: 2,
        p_cost: 1,
    };
    let blob = wrap_with(b"master-key-bytes", b"passphrase", desktopish).unwrap();
    assert_eq!(blob.mem_kib, 96);
    assert_eq!(blob.t_cost, 2);
    assert_eq!(unwrap(&blob, b"passphrase").unwrap(), b"master-key-bytes");
}

#[test]
fn distinct_salts_make_blobs_unique() {
    let a = wrap_with(&[0u8; 32], b"pw", fast()).unwrap();
    let b = wrap_with(&[0u8; 32], b"pw", fast()).unwrap();
    assert_ne!(a.salt, b.salt);
    assert_ne!(a.ciphertext, b.ciphertext);
}
