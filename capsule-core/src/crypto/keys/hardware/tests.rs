use super::*;

#[test]
fn hardware_backed_signer_round_trips_and_asserts_non_exportable() {
    // keys.md Validation (hardware-bound storage round-trip): generate the DSK in the
    // element, sign a payload, verify against the published pubkey, assert non-exportability.
    let hw = Arc::new(MockHardwareSigner::new([7; 32], false));
    let signer = HardwareBackedSigner::enroll(hw.clone(), "device-dsk".into(), &[3; 32]).unwrap();

    let msg = b"asset manifest bytes";
    let sig = signer.sign(msg).unwrap();
    assert!(
        signer.verifying_key().verify(msg, &sig),
        "the hardware-composed hybrid signature must verify against the published key"
    );
    assert!(!signer.verifying_key().verify(b"tampered", &sig));
    assert!(hw.assert_non_exportable("device-dsk".into()).is_ok());

    // A non-conforming (exportable) element is detected.
    let bad = MockHardwareSigner::new([7; 32], true);
    assert!(bad.assert_non_exportable("x".into()).is_err());
}

#[test]
fn the_hardware_ed25519_half_is_load_bearing() {
    // Two devices share the same software ML-DSA seed but different hardware Ed25519 keys: a
    // signature from one must not verify under the other's published key — so the hardware
    // half genuinely gates, not just the software PQ half.
    let a = HardwareBackedSigner::enroll(
        Arc::new(MockHardwareSigner::new([1; 32], false)),
        "a".into(),
        &[9; 32],
    )
    .unwrap();
    let b = HardwareBackedSigner::enroll(
        Arc::new(MockHardwareSigner::new([2; 32], false)),
        "b".into(),
        &[9; 32],
    )
    .unwrap();
    let sig = a.sign(b"m").unwrap();
    assert!(a.verifying_key().verify(b"m", &sig));
    assert!(!b.verifying_key().verify(b"m", &sig));
}
