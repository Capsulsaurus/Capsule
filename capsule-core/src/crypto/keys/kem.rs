//! The Device Encryption Key (DEK) — an ML-KEM-768 KEM keypair.
//!
//! The DEK is the device's key-encapsulation key: a sender encapsulates a shared secret
//! to the device's public key, and the device decapsulates it with its private key. In
//! Capsule it carries key wraps to a device and underlies MLS HPKE
//! (SSoT: [Cryptography — Keys § Device Keys], [Primitives § KEM]).
//!
//! Keys are deterministic from a 64-byte seed, so the DEK persists and restores verbatim.
//!
//! **Deferred:** the design's DEK is the *hybrid* X-Wing (X25519 + ML-KEM-768); the
//! X25519 half and the X-Wing combiner land with the OpenMLS integration (see
//! `DEFERRED.md`). This module implements the post-quantum ML-KEM-768 half, which is the
//! part exercised offline. The seam (`encapsulate`/`decapsulate` over byte strings) is
//! combiner-agnostic, so swapping in X-Wing later does not change callers.
//!
//! [Cryptography — Keys § Device Keys]: https://docs/design/cryptography/keys/#device-keys
//! [Primitives § KEM]: https://docs/design/cryptography/primitives/#kem

use ml_kem::kem::Decapsulate;
use ml_kem::{B32, DecapsulationKey, KeyExport, MlKem768, Seed};

use crate::crypto::{CryptoError, rng};

/// Length of the ML-KEM-768 keypair seed (d ‖ z).
pub const DEK_SEED_LEN: usize = 64;

/// A device encryption keypair (ML-KEM-768). Holds the private decapsulation key; the
/// public encapsulation key is reachable from it.
pub struct DekKeypair {
    dk: DecapsulationKey<MlKem768>,
}

fn shared_to_32(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    out
}

impl DekKeypair {
    /// Generate a fresh DEK from the OS CSPRNG.
    pub fn generate() -> Self {
        Self::from_seed(&rng::random_array::<DEK_SEED_LEN>())
    }

    /// Reconstruct deterministically from a 64-byte seed.
    pub fn from_seed(seed: &[u8; DEK_SEED_LEN]) -> Self {
        let s = Seed::try_from(&seed[..]).expect("64-byte ML-KEM seed");
        Self {
            dk: DecapsulationKey::<MlKem768>::from_seed(s),
        }
    }

    /// Export the 64-byte seed for sealed storage in the keystore.
    pub fn to_seed_bytes(&self) -> [u8; DEK_SEED_LEN] {
        let seed = self
            .dk
            .to_seed()
            .expect("a seed-derived DEK always retains its seed");
        let mut out = [0u8; DEK_SEED_LEN];
        out.copy_from_slice(seed.as_slice());
        out
    }

    /// The public encapsulation-key bytes (for publishing in the device directory).
    pub fn public_bytes(&self) -> Vec<u8> {
        self.dk.encapsulation_key().to_bytes().to_vec()
    }

    /// Encapsulate a fresh shared secret to this keypair's own public key, returning the
    /// ciphertext and the 32-byte shared secret (the sender's side of a round trip).
    pub fn encapsulate_to_self(&self) -> (Vec<u8>, [u8; 32]) {
        // `encapsulate_deterministic` takes the encapsulation randomness explicitly; we
        // draw it fresh from the OS CSPRNG, so this is a randomized encapsulation.
        let m = B32::try_from(&rng::random_array::<32>()[..]).expect("32-byte m");
        let (ct, ss) = self.dk.encapsulation_key().encapsulate_deterministic(&m);
        (ct.as_slice().to_vec(), shared_to_32(ss.as_slice()))
    }

    /// Decapsulate a ciphertext, recovering the 32-byte shared secret (the receiver side).
    pub fn decapsulate(&self, ciphertext: &[u8]) -> Result<[u8; 32], CryptoError> {
        let ss = self
            .dk
            .decapsulate_slice(ciphertext)
            .map_err(|_| CryptoError::Malformed("ML-KEM ciphertext wrong length"))?;
        Ok(shared_to_32(ss.as_slice()))
    }
}

impl Clone for DekKeypair {
    fn clone(&self) -> Self {
        Self::from_seed(&self.to_seed_bytes())
    }
}

impl std::fmt::Debug for DekKeypair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DekKeypair(****)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // ML-KEM uses implicit rejection: a foreign key decapsulates to a *different*
        // pseudo-random secret rather than erroring — still cryptographically safe.
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
}
