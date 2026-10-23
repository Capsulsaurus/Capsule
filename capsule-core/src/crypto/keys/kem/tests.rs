use super::*;

fn hex32(s: &str) -> [u8; 32] {
    shared_to_32(&hex::decode(s).unwrap())
}

#[test]
fn encapsulate_decapsulate_round_trip() {
    let dek = DekKeypair::generate();
    let (ct, k_send) = dek.encapsulate_to_self();
    let k_recv = dek.decapsulate(&ct).unwrap();
    assert_eq!(
        k_send, k_recv,
        "encapsulated and decapsulated secrets must match"
    );
}

#[test]
fn seed_reconstructs_identical_keypair() {
    let dek = DekKeypair::generate();
    let seed = dek.to_seed_bytes();
    let restored = DekKeypair::from_seed(&seed);
    // A ciphertext sealed to the original decapsulates under the restored key.
    let (ct, k) = dek.encapsulate_to_self();
    assert_eq!(restored.decapsulate(&ct).unwrap(), k);
    assert_eq!(restored.public_bytes(), dek.public_bytes());
}

#[test]
fn wrong_key_recovers_a_different_secret() {
    // X-Wing inherits ML-KEM's implicit rejection and binds the decapsulator's own X25519
    // key into the combiner, so a foreign key decapsulates to a *different* secret rather
    // than erroring — still cryptographically safe.
    let alice = DekKeypair::generate();
    let bob = DekKeypair::generate();
    let (ct, k_for_alice) = alice.encapsulate_to_self();
    assert_ne!(bob.decapsulate(&ct).unwrap(), k_for_alice);
}

#[test]
fn malformed_ciphertext_is_rejected() {
    let dek = DekKeypair::generate();
    assert!(dek.decapsulate(b"too short").is_err());
}

#[test]
fn public_key_and_ciphertext_have_xwing_lengths() {
    let dek = DekKeypair::generate();
    assert_eq!(dek.public_bytes().len(), DEK_PUBLIC_LEN, "pk_M ‖ pk_X");
    let (ct, _) = dek.encapsulate_to_self();
    assert_eq!(ct.len(), DEK_CIPHERTEXT_LEN, "ct_M ‖ ct_X");
}

#[test]
fn x25519_half_is_bound_into_the_secret() {
    // Corrupting only the X25519 ciphertext tail must change the recovered secret — proof
    // the classical half genuinely contributes (defence in depth against a broken ML-KEM).
    let dek = DekKeypair::generate();
    let (mut ct, k) = dek.encapsulate_to_self();
    let last = ct.len() - 1;
    ct[last] ^= 0x01;
    assert_ne!(dek.decapsulate(&ct).unwrap(), k);
}

#[test]
fn encapsulate_to_public_matches_self_and_decapsulates() {
    // A sender holding only the recipient's public bytes encapsulates; the recipient
    // decapsulates with its private half and recovers the identical shared secret.
    let recipient = DekKeypair::generate();
    let (ct, k_send) = encapsulate_to_public(&recipient.public_bytes()).unwrap();
    assert_eq!(ct.len(), DEK_CIPHERTEXT_LEN, "ct_M ‖ ct_X");
    assert_eq!(recipient.decapsulate(&ct).unwrap(), k_send);
}

#[test]
fn encapsulate_to_public_derand_agrees_with_self_derand() {
    // Encapsulating to a keypair's own public bytes with a fixed eseed must produce the
    // same ciphertext + secret as the keypair's own derandomized self-encapsulation.
    let dek = DekKeypair::generate();
    let eseed = [0x5Cu8; ESEED_LEN];
    let (ct_pub, ss_pub) = encapsulate_to_public_derand(&dek.public_bytes(), &eseed).unwrap();
    let (ct_self, ss_self) = dek.encapsulate_to_self_derand(&eseed);
    assert_eq!(ct_pub, ct_self);
    assert_eq!(ss_pub, ss_self);
}

#[test]
fn encapsulate_to_public_rejects_malformed_key() {
    assert!(matches!(
        encapsulate_to_public(b"too short"),
        Err(CryptoError::Malformed(_))
    ));
    // Right length but a garbage ML-KEM half fails decoding.
    let garbage = vec![0xFFu8; DEK_PUBLIC_LEN];
    assert!(encapsulate_to_public(&garbage).is_err());
}

#[test]
fn xwing_known_answer_vector() {
    // First test vector from draft-connolly-cfrg-xwing-kem, Appendix C: a 32-byte seed and
    // 64-byte encapsulation seed pin the public key, ciphertext, and shared secret. This is
    // the conformance gate proving the combiner byte-order, label, and seed expansion match
    // the spec (SSoT: Primitives § KEM / Validation — known-answer parity).
    let seed = hex32("7f9c2ba4e88f827d616045507605853ed73b8093f6efbc88eb1a6eacfa66ef26");
    let eseed = hex::decode(
        "3cb1eea988004b93103cfb0aeefd2a686e01fa4a58e8a3639ca8a1e3f9ae57e2\
             35b8cc873c23dc62b8d260169afa2f75ab916a58d974918835d25e6a435085b2",
    )
    .unwrap();
    let eseed: [u8; ESEED_LEN] = eseed.try_into().unwrap();
    let expected_ss = hex32("d2df0522128f09dd8e2c92b1e905c793d8f57a54c3da25861f10bf4ca613e384");

    let dek = DekKeypair::from_seed(&seed);
    let (ct, ss) = dek.encapsulate_to_self_derand(&eseed);
    assert_eq!(
        ss, expected_ss,
        "X-Wing shared secret must match the draft KAT"
    );
    assert_eq!(ct.len(), DEK_CIPHERTEXT_LEN);
    // And the receiver side recovers the same secret from the ciphertext.
    assert_eq!(dek.decapsulate(&ct).unwrap(), expected_ss);
}
