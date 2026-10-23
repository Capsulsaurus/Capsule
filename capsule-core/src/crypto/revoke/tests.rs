use super::*;
use crate::crypto::keys::HybridSigningKey;

#[test]
fn a_proof_verifies_for_its_own_challenge_and_no_other() {
    let ik = HybridSigningKey::generate();
    let signature = ik.sign(&revoke_all_signing_bytes("challenge-one"));

    assert!(verify_revoke_all_proof(
        &ik.verifying_key(),
        "challenge-one",
        &signature
    ));
    assert!(
        !verify_revoke_all_proof(&ik.verifying_key(), "challenge-two", &signature),
        "a proof must not be replayable against a different challenge"
    );
}

#[test]
fn a_proof_does_not_verify_under_another_key() {
    let ik = HybridSigningKey::generate();
    let other = HybridSigningKey::generate();
    let signature = ik.sign(&revoke_all_signing_bytes("challenge"));

    assert!(!verify_revoke_all_proof(
        &other.verifying_key(),
        "challenge",
        &signature
    ));
}

#[test]
fn the_domain_separator_is_part_of_the_message() {
    // Without it, a signature over a bare challenge — a short opaque string — would be a
    // signature over anything else of that shape the IK is ever asked to sign.
    let ik = HybridSigningKey::generate();
    let bare = ik.sign(b"challenge");

    assert!(
        !verify_revoke_all_proof(&ik.verifying_key(), "challenge", &bare),
        "a signature over the undomained challenge must not pass as a revoke-all proof"
    );
    assert!(revoke_all_signing_bytes("challenge").starts_with(REVOKE_ALL_DOMAIN));
}
