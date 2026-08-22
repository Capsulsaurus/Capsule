//! Global session revocation — the master-key-proof ceremony behind "log out of all devices"
//! (slice `S-C23`; SSoT: [Authentication — Explicit Revocation] item 3, and the client half in
//! [Threat Model — Client Invariants]).
//!
//! Single-session revoke is the everyday tool and any active session token authorizes it.
//! The **global** revoke is the nuclear option and is deliberately authenticated
//! *asymmetrically*: by **proof of master-key possession** — a signature with the user's
//! identity key (IK) over a server-issued challenge — and **not** by an active session token.
//! That asymmetry is the whole point: an attacker holding a stolen session token can revoke
//! only *that* session, never escalate to logging the legitimate user out of every device.
//!
//! The ceremony is three steps:
//!
//! 1. [`issue`] mints a single-use, expiring challenge bound to one account.
//! 2. The client signs [`signing_bytes`] with its IK and posts the [`RevokeAllProof`].
//! 3. [`consume_challenge`] burns the challenge, [`verify_proof`] checks the signature, and
//!    only then does the caller invalidate every session — the calling one included.
//!
//! **No confirmation without proof.** Every refusal path here is a hard refusal: a missing or
//! invalid proof revokes nothing at all. There is no partial success, no "revoked all but
//! yours", and nothing for a client to optimistically clear locally on a refusal.
//!
//! ## Anchoring the identity key
//!
//! The doc says the signature is verified "against the identity key published in the device
//! directory". The published [`DeviceDirectory`] does not carry the IK public key as a field —
//! it carries the IK's *signature* over its core (each entry lists a **device** key, not the
//! identity key). So the anchor is established by proof of signing rather than by lookup: the
//! proof presents a candidate IK, and [`verify_proof`] accepts it only if it verifies the
//! account's stored, monotonically-published directory. A key that verifies that directory
//! *is* the identity the account published under, so presenting an attacker's own key gets
//! them nothing. An account that has never published a directory has no anchor and is refused.
//!
//! [Authentication — Explicit Revocation]: https://docs/design/authentication/#explicit-revocation
//! [Threat Model — Client Invariants]: https://docs/design/threat-model/validation/

use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use capsule_core::crypto::keys::DeviceDirectory;
use capsule_core::crypto::keys::hybrid_sig::{HybridSignature, HybridVerifyingKey};
use model::errors::InternalServerError;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};

use crate::session::SessionManager;

/// Domain separator for the revoke-all signing input. Signing is domain-separated so an IK
/// signature minted for any other Capsule structure can never be replayed as a revoke-all
/// proof, and vice versa.
pub const SIGNING_DOMAIN: &str = "capsule-revoke-all/v1";

/// Challenge lifetime: single-use and short, since the client signs it immediately.
pub const CHALLENGE_TTL: Duration = Duration::from_mins(5);

/// Challenge entropy: 32 CSPRNG bytes (256 bits), rendered URL-safe base64 without padding —
/// the same opaque-identifier discipline as enrollment codes and share links (never a UUIDv7
/// or any time-ordered id).
const CHALLENGE_BYTES: usize = 32;

/// Upper bound on a posted proof document. A hybrid IK public key (≈1985 B) plus a hybrid
/// signature (≈3.4 KiB) and a short challenge fits an order of magnitude under this; a larger
/// body is refused before buffering.
pub const MAX_PROOF_BYTES: usize = 64 * 1024;

fn now_secs() -> i64 {
    jiff::Timestamp::now().as_second()
}

fn challenge_key(challenge: &str) -> String {
    format!("revoke_all:challenge:{challenge}")
}

/// The stored challenge: which account it authorizes a revoke for, and when it dies.
///
/// Both guards run together, exactly as for enrollment codes: the storage TTL evicts an
/// abandoned challenge, and the explicit `expires_at` lets [`consume_challenge`]
/// deterministically refuse *and* delete an expired one before the TTL fires — so expiry is
/// testable without sleeps.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChallengeRecord {
    /// The account this challenge authorizes a global revoke for.
    user_id: String,
    /// Explicit expiry (epoch seconds).
    expires_at: i64,
}

/// A freshly issued revoke-all challenge, returned to the requesting device to sign.
#[derive(Debug, Clone)]
pub struct IssuedChallenge {
    /// The opaque challenge value. The client signs [`signing_bytes`] over this exact string
    /// and echoes it back in the proof.
    pub challenge: String,
    /// Expiry as epoch seconds (the route renders it RFC 3339).
    pub expires_at: i64,
}

/// The master-key proof a client posts to revoke every session.
///
/// Transported as canonical CBOR (like the signed device directory) so the hybrid key and
/// signature ride in their native encoding rather than being re-spelled for JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevokeAllProof {
    /// The challenge value exactly as issued.
    pub challenge: String,
    /// The candidate user identity key. Accepted only if it verifies the account's published
    /// device directory — see the module docs.
    pub identity_key: HybridVerifyingKey,
    /// The IK's hybrid signature over [`signing_bytes`].
    pub signature: HybridSignature,
}

/// Why a revoke-all was refused. Retained for tracing and tests only: the HTTP surface
/// collapses every variant into one indistinguishable `error.auth.revoke_proof_invalid`, so a
/// caller cannot use the endpoint as an oracle for which part of the proof was wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The challenge is unknown, already consumed, or past its expiry.
    UnknownOrExpiredChallenge,
    /// The account has never published a device directory, so there is no identity to anchor
    /// the proof against. Refused rather than trusted — an unanchored key proves nothing.
    NoPublishedIdentity,
    /// The stored directory bytes did not decode as a signed `DeviceDirectory`.
    DirectoryUndecodable,
    /// The presented key does not verify the account's published directory: it is not the
    /// identity this account publishes under.
    IdentityKeyMismatch,
    /// The signature does not verify over the challenge under the presented key.
    SignatureInvalid,
}

/// The exact bytes a client's IK signs for a revoke-all proof.
///
/// Domain-separated canonical CBOR over `(domain, user_id, challenge)`. `user_id` is folded in
/// so a proof minted for one account cannot be replayed against another even if a challenge
/// value were somehow reused, and the challenge itself is single-use, so a captured proof is
/// spent the moment it is presented.
///
/// The challenge is signed in the **string form the server issued**, not its decoded bytes, so
/// there is no encoding ambiguity between platforms: a client signs precisely what it received.
pub fn signing_bytes(user_id: &str, challenge: &str) -> Vec<u8> {
    /// Canonical CBOR sorts map keys, so field order here is irrelevant to the encoding.
    #[derive(Serialize)]
    struct SigningInput<'a> {
        challenge: &'a str,
        domain: &'a str,
        user_id: &'a str,
    }

    capsule_core::cbor::to_canonical_vec(&SigningInput {
        challenge,
        domain: SIGNING_DOMAIN,
        user_id,
    })
    .expect("revoke-all signing input serializes")
}

/// Issue a fresh single-use revoke-all challenge for `user_id`, live for `ttl`.
///
/// Callers pass [`CHALLENGE_TTL`] in production; tests pass a shorter (even zero) `ttl` to
/// exercise the expiry path deterministically.
#[tracing::instrument(skip(sm), fields(user_id = %user_id, ttl_secs = ttl.as_secs()))]
pub async fn issue(
    sm: &SessionManager,
    user_id: &str,
    ttl: Duration,
) -> Result<IssuedChallenge, InternalServerError> {
    let mut buf = [0u8; CHALLENGE_BYTES];
    SystemRandom::new()
        .fill(&mut buf)
        .expect("OS CSPRNG must be available");
    let challenge = URL_SAFE_NO_PAD.encode(buf);
    let expires_at = now_secs() + i64::try_from(ttl.as_secs()).unwrap_or(i64::MAX);

    sm.save_temp_data(
        &challenge_key(&challenge),
        &ChallengeRecord {
            user_id: user_id.to_string(),
            expires_at,
        },
        ttl,
    )
    .await?;

    tracing::info!(expires_at, "issued revoke-all challenge");
    Ok(IssuedChallenge {
        challenge,
        expires_at,
    })
}

/// Burn `challenge` and return the account it authorized, or `None` when it is unknown,
/// already consumed, or expired.
///
/// The challenge is deleted **before** the outcome is decided, so neither a successful
/// revoke-all nor a failed attempt can replay it. Burning on every attempt is deliberate: it
/// is what stops an attacker grinding signatures against a live challenge, and costs the
/// legitimate user nothing but a second request.
#[tracing::instrument(skip(sm, challenge))]
pub async fn consume_challenge(
    sm: &SessionManager,
    challenge: &str,
) -> Result<Option<String>, InternalServerError> {
    let record: Option<ChallengeRecord> = sm.get_temp_data(&challenge_key(challenge)).await?;
    let Some(record) = record else {
        tracing::debug!("revoke-all challenge unknown or already consumed");
        return Ok(None);
    };
    sm.delete_temp_data(&challenge_key(challenge)).await?;

    if now_secs() >= record.expires_at {
        tracing::debug!("revoke-all challenge expired — deleted on this attempt");
        return Ok(None);
    }
    Ok(Some(record.user_id))
}

/// Verify a revoke-all proof for `user_id` against the account's stored device directory.
///
/// `directory` is the verbatim signed CBOR last published by the account (`None` when it has
/// never published one). Both checks must pass: the presented key must verify that directory
/// (establishing it as the account's published identity — see the module docs), and it must
/// verify the signature over [`signing_bytes`].
#[tracing::instrument(skip(proof, directory), fields(user_id = %user_id))]
pub fn verify_proof(
    user_id: &str,
    proof: &RevokeAllProof,
    directory: Option<&[u8]>,
) -> Result<(), Refusal> {
    let Some(bytes) = directory else {
        tracing::warn!("revoke-all refused: account has published no device directory");
        return Err(Refusal::NoPublishedIdentity);
    };
    let directory: DeviceDirectory = capsule_core::cbor::from_slice(bytes).map_err(|e| {
        tracing::error!("revoke-all refused: stored directory did not decode: {e}");
        Refusal::DirectoryUndecodable
    })?;

    if !directory.verify(&proof.identity_key) {
        tracing::warn!("revoke-all refused: presented key does not sign the published directory");
        return Err(Refusal::IdentityKeyMismatch);
    }
    if !proof
        .identity_key
        .verify(&signing_bytes(user_id, &proof.challenge), &proof.signature)
    {
        tracing::warn!("revoke-all refused: signature does not verify over the challenge");
        return Err(Refusal::SignatureInvalid);
    }

    tracing::info!("revoke-all master-key proof verified");
    Ok(())
}

#[cfg(test)]
mod tests {
    use capsule_core::crypto::keys::hybrid_sig::HybridSigningKey;
    use capsule_core::crypto::keys::{DeviceEntry, DirectoryCore};
    use uuid::Uuid;

    use super::*;
    use crate::session::InMemorySessionStorage;

    fn manager() -> SessionManager {
        SessionManager::new_with_storage(
            Box::new(InMemorySessionStorage::new()),
            Duration::from_secs(3600),
        )
    }

    fn ik() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
    }

    fn other_ik() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[9; 32], &[8; 32])
    }

    /// A real `capsule-core`-signed directory for the account, as stored verbatim.
    fn directory_bytes(signer: &HybridSigningKey) -> Vec<u8> {
        let device = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let directory = DirectoryCore {
            user_id: Uuid::from_u128(1),
            directory_version: 1,
            updated_at: "2026-08-22T00:00:00Z".into(),
            devices: vec![DeviceEntry {
                device_id: Uuid::from_u128(0xD1),
                dsk_public: device.verifying_key(),
                added_at: "2026-08-21T00:00:00Z".into(),
                revoked_at: None,
            }],
        }
        .sign(signer);
        capsule_core::cbor::to_canonical_vec(&directory).expect("directory serializes")
    }

    fn proof_for(signer: &HybridSigningKey, user_id: &str, challenge: &str) -> RevokeAllProof {
        RevokeAllProof {
            challenge: challenge.to_string(),
            identity_key: signer.verifying_key(),
            signature: signer.sign(&signing_bytes(user_id, challenge)),
        }
    }

    #[test]
    fn signing_bytes_are_domain_separated_and_account_bound() {
        let a = signing_bytes("user-a", "chal-1");
        assert_ne!(
            a,
            signing_bytes("user-b", "chal-1"),
            "the same challenge under another account signs different bytes"
        );
        assert_ne!(
            a,
            signing_bytes("user-a", "chal-2"),
            "a different challenge signs different bytes"
        );
        assert!(
            a.windows(SIGNING_DOMAIN.len())
                .any(|w| w == SIGNING_DOMAIN.as_bytes()),
            "the domain separator is inside the signed bytes"
        );
        assert_eq!(a, signing_bytes("user-a", "chal-1"), "encoding is stable");
    }

    #[tokio::test]
    async fn a_challenge_is_single_use() {
        let sm = manager();
        let issued = issue(&sm, "user-a", CHALLENGE_TTL).await.expect("issued");

        assert_eq!(
            consume_challenge(&sm, &issued.challenge).await.expect("ok"),
            Some("user-a".to_string())
        );
        assert_eq!(
            consume_challenge(&sm, &issued.challenge).await.expect("ok"),
            None,
            "a consumed challenge cannot be replayed"
        );
    }

    #[tokio::test]
    async fn unknown_and_expired_challenges_are_indistinguishable() {
        let sm = manager();
        assert_eq!(
            consume_challenge(&sm, "never-issued").await.expect("ok"),
            None
        );

        // Zero TTL: already past its explicit expiry the instant it is issued.
        let issued = issue(&sm, "user-a", Duration::from_secs(0))
            .await
            .expect("issued");
        assert_eq!(
            consume_challenge(&sm, &issued.challenge).await.expect("ok"),
            None,
            "an expired challenge authorizes nothing"
        );
        assert_eq!(
            consume_challenge(&sm, &issued.challenge).await.expect("ok"),
            None,
            "and it was deleted on that attempt"
        );
    }

    #[tokio::test]
    async fn challenges_are_high_entropy_and_nonstructured() {
        let sm = manager();
        let a = issue(&sm, "user-a", CHALLENGE_TTL).await.expect("issued");
        let b = issue(&sm, "user-a", CHALLENGE_TTL).await.expect("issued");
        assert_ne!(a.challenge, b.challenge);
        assert!(a.challenge.len() >= 43, "256-bit base64 is ~43 chars");
    }

    #[test]
    fn a_valid_proof_verifies() {
        let ik = ik();
        let proof = proof_for(&ik, "user-a", "chal-1");
        assert_eq!(
            verify_proof("user-a", &proof, Some(&directory_bytes(&ik))),
            Ok(())
        );
    }

    #[test]
    fn a_proof_from_a_key_that_does_not_sign_the_directory_is_refused() {
        // The attacker holds a perfectly valid keypair — just not the account's identity.
        let attacker = other_ik();
        let proof = proof_for(&attacker, "user-a", "chal-1");
        assert_eq!(
            verify_proof("user-a", &proof, Some(&directory_bytes(&ik()))),
            Err(Refusal::IdentityKeyMismatch)
        );
    }

    #[test]
    fn a_signature_over_another_challenge_is_refused() {
        let ik = ik();
        let mut proof = proof_for(&ik, "user-a", "chal-1");
        // Same account and key, but the signature covers a different challenge.
        proof.challenge = "chal-2".to_string();
        assert_eq!(
            verify_proof("user-a", &proof, Some(&directory_bytes(&ik))),
            Err(Refusal::SignatureInvalid)
        );
    }

    #[test]
    fn a_proof_minted_for_another_account_is_refused() {
        let ik = ik();
        let proof = proof_for(&ik, "user-b", "chal-1");
        assert_eq!(
            verify_proof("user-a", &proof, Some(&directory_bytes(&ik))),
            Err(Refusal::SignatureInvalid),
            "user_id is folded into the signed bytes"
        );
    }

    #[test]
    fn a_tampered_signature_is_refused() {
        let ik = ik();
        let mut proof = proof_for(&ik, "user-a", "chal-1");

        // Flip a byte deep inside the signature's post-quantum half and re-decode it: the
        // document still parses, so this exercises verification rather than decoding.
        let mut sig =
            capsule_core::cbor::to_canonical_vec(&proof.signature).expect("signature serializes");
        let mid = sig.len() / 2;
        sig[mid] ^= 0xFF;
        proof.signature = capsule_core::cbor::from_slice(&sig).expect("still decodes");

        assert_eq!(
            verify_proof("user-a", &proof, Some(&directory_bytes(&ik))),
            Err(Refusal::SignatureInvalid)
        );
    }

    #[test]
    fn a_signature_over_unrelated_bytes_is_refused() {
        let ik = ik();
        let mut proof = proof_for(&ik, "user-a", "chal-1");
        // A genuine IK signature — just not over this ceremony's signing input. Domain
        // separation is what makes such a signature unusable here.
        proof.signature = ik.sign(b"some other capsule structure entirely");
        assert_eq!(
            verify_proof("user-a", &proof, Some(&directory_bytes(&ik))),
            Err(Refusal::SignatureInvalid)
        );
    }

    #[test]
    fn an_account_with_no_published_directory_has_no_anchor() {
        let ik = ik();
        let proof = proof_for(&ik, "user-a", "chal-1");
        assert_eq!(
            verify_proof("user-a", &proof, None),
            Err(Refusal::NoPublishedIdentity),
            "an unanchored key proves nothing, so the revoke is refused"
        );
    }

    #[test]
    fn an_undecodable_stored_directory_refuses_rather_than_trusts() {
        let ik = ik();
        let proof = proof_for(&ik, "user-a", "chal-1");
        assert_eq!(
            verify_proof("user-a", &proof, Some(b"not cbor at all")),
            Err(Refusal::DirectoryUndecodable)
        );
    }

    #[test]
    fn the_proof_round_trips_through_canonical_cbor() {
        let ik = ik();
        let proof = proof_for(&ik, "user-a", "chal-1");
        let wire = capsule_core::cbor::to_canonical_vec(&proof).expect("serializes");
        let back: RevokeAllProof = capsule_core::cbor::from_slice(&wire).expect("decodes");
        assert_eq!(back.challenge, proof.challenge);
        assert_eq!(
            verify_proof("user-a", &back, Some(&directory_bytes(&ik))),
            Ok(())
        );
    }
}
