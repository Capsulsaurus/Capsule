//! The master-key proof that authorizes a global sign-out (`S-C23`; SSoT:
//! [Authentication — Explicit Revocation](https://docs/design/authentication/#explicit-revocation)).
//!
//! # The asymmetry this exists to create
//!
//! Revoking one session is authenticated by any live session token. Revoking *every* session is
//! not, and deliberately: an attacker holding a stolen token could otherwise invoke "log out of
//! all devices" and lock the legitimate user out of every device they own. Gating the global
//! revoke on a signature by the account's identity key means a stolen token can revoke only
//! itself — it cannot escalate a theft into a denial of service.
//!
//! # Why the message is built here rather than at each end
//!
//! The server verifies this proof and a client produces it, so it is a two-ended format — and a
//! two-ended format defined twice is one edit away from a signature that stops verifying. This
//! is the same reasoning that moved the custody receipt into [`crate::crypto::receipts`], with
//! one difference that makes it cheaper: the signed message is a byte string rather than a CBOR
//! map, so there is no key-ordering trap. The trap that remains is the domain separator, and
//! that is exactly what a shared constant removes.
//!
//! # Domain separation is not decoration
//!
//! The IK signs several things — the device directory core, and this. A bare challenge is a
//! short opaque string, and a signature over one is a signature over anything else of that
//! shape. Prefixing every revoke-all proof with [`REVOKE_ALL_DOMAIN`] means a signature
//! produced for one purpose cannot be replayed as the other, whatever a future ceremony chooses
//! to sign.

use crate::crypto::keys::{HybridSignature, HybridVerifyingKey};

/// The domain separator every revoke-all proof is signed under.
///
/// Versioned, so a future change to what the proof covers is a *different* message rather than
/// a silently compatible one.
pub const REVOKE_ALL_DOMAIN: &[u8] = b"capsule/revoke-all/v1";

/// The exact bytes a revoke-all proof signs, for `challenge`.
///
/// The challenge is a single-use, server-issued, high-entropy token; it carries the freshness
/// and the account binding, and this function carries the domain.
#[must_use]
pub fn revoke_all_signing_bytes(challenge: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(REVOKE_ALL_DOMAIN.len() + challenge.len());
    bytes.extend_from_slice(REVOKE_ALL_DOMAIN);
    bytes.extend_from_slice(challenge.as_bytes());
    bytes
}

/// Whether `signature` is a valid revoke-all proof for `challenge` under `identity_key`.
///
/// `identity_key` is the account's **anchor** — the identity public key its published device
/// directory verifies under (`S-C42`) — not a key the request supplied. A proof checked against
/// a caller-supplied key proves only that the caller can sign, which is not a fact about the
/// account.
#[must_use]
pub fn verify_revoke_all_proof(
    identity_key: &HybridVerifyingKey,
    challenge: &str,
    signature: &HybridSignature,
) -> bool {
    identity_key.verify(&revoke_all_signing_bytes(challenge), signature)
}

#[cfg(test)]
mod tests {
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
}
