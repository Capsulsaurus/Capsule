use super::*;

const KEY: [u8; 32] = [0x42; 32];

#[test]
fn seal_open_round_trip() {
    let plaintext = b"deterministic CBOR sidecar bytes";
    let wire = seal_blob(&KEY, plaintext);
    assert_eq!(open_blob(&KEY, &wire).unwrap(), plaintext);
}

#[test]
fn wire_format_layout_is_exact() {
    let wire = seal_blob(&KEY, b"abc");
    // suite(2) big-endian = 0x0001.
    assert_eq!(&wire[0..2], &[0x00, 0x01]);
    // total = 2 + 12 + 3 (ct) + 16 (tag).
    assert_eq!(wire.len(), 2 + 12 + 3 + 16);
    assert_eq!(blob_suite_id(&wire), Some(0x0001));
}

#[test]
fn empty_plaintext_is_well_formed() {
    let wire = seal_blob(&KEY, b"");
    assert_eq!(wire.len(), MIN_BLOB_LEN);
    assert_eq!(open_blob(&KEY, &wire).unwrap(), b"");
}

#[test]
fn ciphertext_hash_covers_entire_wire() {
    let wire = seal_blob(&KEY, b"payload");
    assert_eq!(blob_ciphertext_hash(&wire), hash::hash_bytes(&wire));
    // Flipping any byte (even in the header) changes the content hash.
    let mut t = wire.clone();
    t[0] ^= 0x01;
    assert_ne!(blob_ciphertext_hash(&t), blob_ciphertext_hash(&wire));
}

#[test]
fn fresh_nonce_per_seal() {
    let a = seal_blob(&KEY, b"same");
    let b = seal_blob(&KEY, b"same");
    assert_ne!(a, b, "each seal must draw a fresh nonce");
    // Nonce field differs.
    assert_ne!(&a[2..14], &b[2..14]);
}

#[test]
fn tamper_and_wrong_key_rejected() {
    let wire = seal_blob(&KEY, b"secret");
    let mut t = wire.clone();
    let last = t.len() - 1;
    t[last] ^= 0x01;
    assert!(open_blob(&KEY, &t).is_err());
    assert!(open_blob(&[0u8; 32], &wire).is_err());
}

#[test]
fn unknown_suite_id_fails_closed_before_decrypt() {
    let mut wire = seal_blob(&KEY, b"x");
    wire[0] = 0xff;
    wire[1] = 0xff;
    assert_eq!(
        open_blob(&KEY, &wire),
        Err(CryptoError::UnknownSuite(0xffff))
    );
}

#[test]
fn blob_nonce_reads_the_header_nonce() {
    let nonce = [0xA5u8; NONCE_LEN];
    let wire = seal_blob_with_nonce(&KEY, nonce, b"payload");
    assert_eq!(blob_nonce(&wire), Some(nonce));
    // Too short to carry a full nonce → None (fail-closed at the caller).
    assert_eq!(blob_nonce(&[0u8; SUITE_LEN + NONCE_LEN - 1]), None);
}

#[test]
fn too_short_is_malformed() {
    assert!(matches!(
        open_blob(&KEY, &[0u8; 5]),
        Err(CryptoError::Malformed(_))
    ));
}
