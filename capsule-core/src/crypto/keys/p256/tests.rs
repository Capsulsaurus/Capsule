use std::sync::Arc;

use super::*;
use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
use crate::crypto::keys::{ClassicalAlgorithm, HybridSigningKey};

/// The acceptance criterion: enroll a mock P-256 element, sign a fixed payload,
/// verify both halves through the algorithm-tagged hybrid verifying key, assert an
/// Ed25519-only verifier rejects it, assert non-exportability, and prove directory dispatch
/// on the entry's declared classical algorithm.
#[test]
fn p256_hybrid_round_trip_and_directory_dispatch() {
    use uuid::Uuid;

    // A mock P-256 secure element (software `p256`) stands in for the Secure Enclave.
    let element = Arc::new(MockP256Element::new([7u8; 32], false));
    let signer =
        P256HybridSigningKey::enroll(element.clone(), "device-dsk".into(), &[3u8; 32]).unwrap();

    // The published hybrid key is P-256-tagged; both halves verify.
    let vk = signer.verifying_key();
    assert_eq!(vk.classical_algorithm(), ClassicalAlgorithm::EcdsaP256);

    let msg = b"asset manifest bytes";
    let sig = signer.sign(msg).unwrap();
    assert_eq!(sig.classical_algorithm(), ClassicalAlgorithm::EcdsaP256);
    assert!(
        vk.verify(msg, &sig),
        "the hardware-composed P-256 hybrid signature must verify against the published key"
    );
    assert!(!vk.verify(b"tampered", &sig));

    // The hardware P-256 half is load-bearing: a different element (different P-256 key) with
    // the SAME software ML-DSA seed must not verify — so the P-256 half genuinely gates.
    let other = P256HybridSigningKey::enroll(
        Arc::new(MockP256Element::new([9u8; 32], false)),
        "other".into(),
        &[3u8; 32],
    )
    .unwrap();
    assert!(
        !other.verifying_key().verify(msg, &sig),
        "the hardware P-256 half must gate, not just the shared software PQ half"
    );

    // Algorithm dispatch: an Ed25519-only verifier must reject the P-256 signature.
    let ed_vk = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]).verifying_key();
    assert_eq!(ed_vk.classical_algorithm(), ClassicalAlgorithm::Ed25519);
    assert!(
        !ed_vk.verify(msg, &sig),
        "an Ed25519 verifier must reject a P-256 signature"
    );

    // Non-exportability contract (the existing `HardwareSigner` smoke): a conforming element
    // asserts Ok; an exportable one is detected.
    assert!(element.assert_non_exportable("device-dsk".into()).is_ok());
    assert!(
        MockP256Element::new([7; 32], true)
            .assert_non_exportable("x".into())
            .is_err()
    );

    // Serde keeps the P-256 tag recoverable across the wire, and the Ed25519 vectors below
    // (see hybrid_sig.rs) are unchanged because only the classical-half length differs.
    let sig_back: HybridSignature =
        crate::cbor::from_slice(&crate::cbor::to_canonical_vec(&sig).unwrap()).unwrap();
    assert_eq!(
        sig_back.classical_algorithm(),
        ClassicalAlgorithm::EcdsaP256
    );
    assert!(vk.verify(msg, &sig_back));
    let vk_back: HybridVerifyingKey =
        crate::cbor::from_slice(&crate::cbor::to_canonical_vec(&vk).unwrap()).unwrap();
    assert_eq!(vk_back, vk);

    // Directory dispatch: a signed device directory carries the P-256 device entry; lookup +
    // verify dispatches on the entry's declared classical algorithm.
    let ik = HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]);
    let directory = DirectoryCore {
        user_id: Uuid::from_u128(1),
        directory_version: 1,
        updated_at: "2026-05-31T00:00:00Z".into(),
        devices: vec![DeviceEntry {
            device_id: Uuid::from_u128(0xD1),
            dsk_public: vk.clone(),
            dek_public: None,
            added_at: "2026-05-30T00:00:00Z".into(),
            revoked_at: None,
            unknown: Default::default(),
        }],
        unknown: Default::default(),
    }
    .sign(&ik);
    assert!(directory.verify(&ik.verifying_key()));
    let entry = directory.device(&Uuid::from_u128(0xD1)).unwrap();
    assert_eq!(
        entry.dsk_public.classical_algorithm(),
        ClassicalAlgorithm::EcdsaP256
    );
    assert!(
        entry.dsk_public.verify(msg, &sig),
        "the directory's P-256 entry verifies the P-256 signature"
    );
    // A cross-algorithm Ed25519 signature does not pass the P-256 directory entry.
    let ed_sig = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]).sign(msg);
    assert!(!entry.dsk_public.verify(msg, &ed_sig));
}
