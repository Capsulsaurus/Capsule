use std::sync::Arc;

use super::*;
use crate::crypto::keys::{HardwareBackedSigner, Signer as _};

#[test]
fn software_signer_smoke() {
    // The Linux software-signer smoke (keys.md Validation, software fallback): enroll the
    // device key, sign a payload, verify it against the published hybrid key. This bypasses
    // the FFI and all hardware, so it is the backend gated in CI.
    let hw = Arc::new(SoftwareSigner::from_seed([5u8; 32]));
    let signer = HardwareBackedSigner::enroll(hw.clone(), "device-dsk".into(), &[6u8; 32]).unwrap();

    let msg = b"asset manifest bytes";
    let sig = signer.sign(msg).unwrap();
    assert!(
        signer.verifying_key().verify(msg, &sig),
        "the software-composed hybrid signature must verify against the published key"
    );
    assert!(!signer.verifying_key().verify(b"tampered", &sig));

    // Determinism: the same sealed seed reproduces the same device key across restarts.
    let again = HardwareBackedSigner::enroll(
        Arc::new(SoftwareSigner::from_seed([5u8; 32])),
        "device-dsk".into(),
        &[6u8; 32],
    )
    .unwrap();
    assert_eq!(signer.verifying_key(), again.verifying_key());

    // It is honest about offering no hardware guarantee — unlike a real element.
    assert!(matches!(
        hw.assert_non_exportable("device-dsk".into()),
        Err(HardwareSignerError::Exportable)
    ));
}

#[test]
fn distinct_aliases_get_distinct_keys() {
    let hw = SoftwareSigner::from_seed([1u8; 32]);
    assert_ne!(
        hw.classical_public_key("dsk-a".into()).unwrap(),
        hw.classical_public_key("dsk-b".into()).unwrap(),
        "per-alias derivation must not collide"
    );
}
