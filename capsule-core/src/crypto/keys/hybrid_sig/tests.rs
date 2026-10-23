use super::*;

fn fixed_key() -> HybridSigningKey {
    HybridSigningKey::from_seed_bytes(&[1u8; 32], &[2u8; 32])
}

#[test]
fn sign_verify_round_trip() {
    let sk = HybridSigningKey::generate();
    let vk = sk.verifying_key();
    let msg = b"asset manifest bytes";
    let sig = sk.sign(msg);
    assert!(vk.verify(msg, &sig));
}

#[test]
fn software_key_is_ed25519_tagged() {
    let sk = fixed_key();
    assert_eq!(
        sk.verifying_key().classical_algorithm(),
        ClassicalAlgorithm::Ed25519
    );
    assert_eq!(
        sk.sign(b"msg").classical_algorithm(),
        ClassicalAlgorithm::Ed25519
    );
}

#[test]
fn rejects_wrong_message() {
    let sk = fixed_key();
    let sig = sk.sign(b"original");
    assert!(!sk.verifying_key().verify(b"tampered", &sig));
}

#[test]
fn rejects_wrong_key() {
    let sig = fixed_key().sign(b"msg");
    let other = HybridSigningKey::from_seed_bytes(&[9u8; 32], &[9u8; 32]);
    assert!(!other.verifying_key().verify(b"msg", &sig));
}

// ── Both halves are required (the load-bearing property) ─────────────────────

#[test]
fn corrupting_only_the_ed25519_half_is_rejected() {
    let sk = fixed_key();
    let mut sig = sk.sign(b"msg");
    match &mut sig.classical {
        ClassicalSignature::Ed25519(ed) => ed[0] ^= 0x01, // ML-DSA half still valid
        ClassicalSignature::EcdsaP256(_) => unreachable!("software key is Ed25519"),
    }
    assert!(
        !sk.verifying_key().verify(b"msg", &sig),
        "a valid ML-DSA half must not rescue a broken Ed25519 half"
    );
}

#[test]
fn corrupting_only_the_mldsa_half_is_rejected() {
    let sk = fixed_key();
    let mut sig = sk.sign(b"msg");
    let last = sig.ml.len() - 1;
    sig.ml[last] ^= 0x01; // Ed25519 half still valid
    assert!(
        !sk.verifying_key().verify(b"msg", &sig),
        "a valid Ed25519 half must not rescue a broken ML-DSA half"
    );
}

#[test]
fn swapping_halves_between_two_signatures_is_rejected() {
    let sk = fixed_key();
    let vk = sk.verifying_key();
    let sig_a = sk.sign(b"message A");
    let sig_b = sk.sign(b"message B");
    // Graft A's Ed25519 half onto B's ML-DSA half: neither message verifies.
    let frankenstein = HybridSignature {
        classical: sig_a.classical,
        ml: sig_b.ml,
    };
    assert!(!vk.verify(b"message A", &frankenstein));
    assert!(!vk.verify(b"message B", &frankenstein));
}

#[test]
fn truncated_mldsa_half_is_rejected_not_panicking() {
    let sk = fixed_key();
    let mut sig = sk.sign(b"msg");
    sig.ml.truncate(10);
    assert!(!sk.verifying_key().verify(b"msg", &sig));
}

// ── Determinism + serialization stability ────────────────────────────────────

#[test]
fn seeds_reconstruct_an_identical_key() {
    let sk = fixed_key();
    let seeds = sk.to_seed_bytes();
    let sk2 = HybridSigningKey::from_seed64(&seeds);
    assert_eq!(sk.verifying_key(), sk2.verifying_key());
    // And a signature from the reconstructed key verifies under the original's vk.
    let sig = sk2.sign(b"x");
    assert!(sk.verifying_key().verify(b"x", &sig));
}

#[test]
fn verifying_key_byte_round_trip() {
    let vk = fixed_key().verifying_key();
    let bytes = vk.to_bytes();
    assert_eq!(bytes.len(), 32 + 1952);
    assert_eq!(HybridVerifyingKey::from_bytes(&bytes).unwrap(), vk);
}

#[test]
fn signature_serde_uses_byte_strings_and_round_trips() {
    let sk = fixed_key();
    let sig = sk.sign(b"msg");
    let bytes = crate::cbor::to_canonical_vec(&sig).unwrap();
    // Map with byte-string values: map(2) head 0xa2; first key "ed" (text) -> 0x62 6564.
    assert_eq!(bytes[0], 0xa2);
    let back: HybridSignature = crate::cbor::from_slice(&bytes).unwrap();
    assert_eq!(back, sig);
    assert!(sk.verifying_key().verify(b"msg", &back));
}

#[test]
fn verifying_key_serde_round_trips() {
    let vk = fixed_key().verifying_key();
    let bytes = crate::cbor::to_canonical_vec(&vk).unwrap();
    let back: HybridVerifyingKey = crate::cbor::from_slice(&bytes).unwrap();
    assert_eq!(back, vk);
}
