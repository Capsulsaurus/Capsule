use super::*;

#[test]
fn software_key_signs_through_the_trait_object() {
    let key = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let signer: &dyn Signer = &key;
    let sig = signer.sign(b"manifest bytes").unwrap();
    assert!(signer.verifying_key().verify(b"manifest bytes", &sig));
    // The trait object's public key matches the concrete key's.
    assert_eq!(signer.verifying_key(), key.verifying_key());
}
