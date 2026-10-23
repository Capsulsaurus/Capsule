use super::*;
use crate::crypto::keys::kem::{DEK_CIPHERTEXT_LEN, DEK_PUBLIC_LEN, DekKeypair};

/// The acceptance: encapsulate to the hybrid public key, decapsulate through the
/// hardware-held P-256 half (via the mock element), and recover the identical shared secret.
#[test]
fn p256_hybrid_dek_round_trips_through_hardware_ecdh() {
    let element = Arc::new(MockP256KeyAgreement::new([7u8; 32], false));
    let dek = P256HybridDek::enroll(element.clone(), "device-dek".into(), &[3u8; 32]).unwrap();

    assert_eq!(dek.public_bytes().len(), DEK_P256_PUBLIC_LEN, "ek_M ‖ pk_P");
    let (ct, k_send) = encapsulate_to_p256_public(&dek.public_bytes()).unwrap();
    assert_eq!(ct.len(), DEK_P256_CIPHERTEXT_LEN, "ct_M ‖ ct_P");
    let k_recv = dek.decapsulate(&ct).unwrap();
    assert_eq!(
        k_send, k_recv,
        "sender and hardware-decapsulated secrets must match"
    );

    // Non-exportability contract (mirrors the DSK smoke).
    assert!(element.assert_non_exportable("device-dek".into()).is_ok());
    assert!(
        MockP256KeyAgreement::new([7; 32], true)
            .assert_non_exportable("x".into())
            .is_err()
    );
}

#[test]
fn hardware_p256_half_is_load_bearing() {
    // Two DEKs share the same software ML-KEM seed but different hardware P-256 keys: a
    // ciphertext sealed to one must NOT decapsulate to the same secret under the other — so the
    // hardware P-256 ECDH half genuinely gates, not just the shared software ML-KEM half.
    let a = P256HybridDek::enroll(
        Arc::new(MockP256KeyAgreement::new([1u8; 32], false)),
        "a".into(),
        &[9u8; 32],
    )
    .unwrap();
    let b = P256HybridDek::enroll(
        Arc::new(MockP256KeyAgreement::new([2u8; 32], false)),
        "b".into(),
        &[9u8; 32],
    )
    .unwrap();
    let (ct, k_for_a) = encapsulate_to_p256_public(&a.public_bytes()).unwrap();
    assert_ne!(
        b.decapsulate(&ct).unwrap(),
        k_for_a,
        "the hardware P-256 half must gate, not just the software ML-KEM half"
    );
}

#[test]
fn corrupting_the_p256_ciphertext_tail_changes_the_secret() {
    // Flipping a bit in `ct_P` (the ephemeral public key) must change the recovered secret —
    // proof the classical half genuinely contributes (defence in depth against a broken ML-KEM).
    let dek = P256HybridDek::enroll(
        Arc::new(MockP256KeyAgreement::new([5u8; 32], false)),
        "d".into(),
        &[6u8; 32],
    )
    .unwrap();
    let (mut ct, k) = encapsulate_to_p256_public(&dek.public_bytes()).unwrap();
    let last = ct.len() - 1;
    ct[last] ^= 0x01;
    // A corrupted point may fail to decode (hardware rejects) or decode to a different point
    // (different secret); either way the original secret must not be recovered.
    if let Ok(k2) = dek.decapsulate(&ct) {
        assert_ne!(k2, k);
    }
}

#[test]
fn derand_encapsulation_is_deterministic() {
    let dek = P256HybridDek::enroll(
        Arc::new(MockP256KeyAgreement::new([8u8; 32], false)),
        "d".into(),
        &[4u8; 32],
    )
    .unwrap();
    let coin = [0x5Cu8; 32];
    let eph = || p256::SecretKey::from_slice(&[11u8; 32]).unwrap();
    let (ct1, ss1) = encapsulate_to_p256_public_derand(&dek.public_bytes(), &coin, eph()).unwrap();
    let (ct2, ss2) = encapsulate_to_p256_public_derand(&dek.public_bytes(), &coin, eph()).unwrap();
    assert_eq!(ct1, ct2);
    assert_eq!(ss1, ss2);
    assert_eq!(dek.decapsulate(&ct1).unwrap(), ss1);
}

#[test]
fn malformed_ciphertext_is_rejected() {
    let dek = P256HybridDek::enroll(
        Arc::new(MockP256KeyAgreement::new([5u8; 32], false)),
        "d".into(),
        &[6u8; 32],
    )
    .unwrap();
    assert!(matches!(
        dek.decapsulate(b"too short"),
        Err(CryptoError::Malformed(_))
    ));
}

#[test]
fn encapsulate_rejects_malformed_public_key() {
    assert!(matches!(
        encapsulate_to_p256_public(b"too short"),
        Err(CryptoError::Malformed(_))
    ));
    // Right length but a garbage ML-KEM/P-256 half fails decoding.
    let garbage = vec![0xFFu8; DEK_P256_PUBLIC_LEN];
    assert!(encapsulate_to_p256_public(&garbage).is_err());
}

/// Cross-algorithm rejection, both directions: the P-256 variant and the software X-Wing use
/// distinct public/ciphertext lengths, so neither KEM's ciphertext is accepted by the other's
/// decapsulator — the length tag keeps the two byte-disjoint.
#[test]
fn cross_algorithm_ciphertexts_are_rejected_by_length() {
    // Lengths differ (the whole point of the length tag), so the two forms never alias.
    assert_ne!(DEK_P256_PUBLIC_LEN, DEK_PUBLIC_LEN);
    assert_ne!(DEK_P256_CIPHERTEXT_LEN, DEK_CIPHERTEXT_LEN);

    let p256_dek = P256HybridDek::enroll(
        Arc::new(MockP256KeyAgreement::new([5u8; 32], false)),
        "d".into(),
        &[6u8; 32],
    )
    .unwrap();
    let xwing = DekKeypair::generate();

    // An X-Wing ciphertext presented to the P-256 decapsulator is rejected on length.
    let (xwing_ct, _) = xwing.encapsulate_to_self();
    assert!(matches!(
        p256_dek.decapsulate(&xwing_ct),
        Err(CryptoError::Malformed(_))
    ));

    // A P-256 ciphertext presented to the X-Wing decapsulator is likewise rejected on length.
    let (p256_ct, _) = encapsulate_to_p256_public(&p256_dek.public_bytes()).unwrap();
    assert!(xwing.decapsulate(&p256_ct).is_err());
}
