//! The Device Encryption Key (DEK) — an **X-Wing** hybrid KEM keypair (X25519 + ML-KEM-768).
//!
//! The DEK is the device's key-encapsulation key: a sender encapsulates a shared secret
//! to the device's public key, and the device decapsulates it with its private key. In
//! Capsule it carries key wraps to a device and underlies MLS HPKE
//! (SSoT: [Cryptography — Keys § Device Keys], [Primitives § KEM]).
//!
//! This is the hybrid the design's `crypto_suite_id = 0x0001` declares: **X-Wing**
//! (`draft-connolly-cfrg-xwing-kem`), combining the post-quantum **ML-KEM-768** half with the
//! classical **X25519** half so neither being broken alone compromises the shared secret. Both
//! halves are bound into a single 32-byte secret by the X-Wing combiner
//! `SHA3-256(ss_M ‖ ss_X ‖ ct_X ‖ pk_X ‖ XWingLabel)`.
//!
//! Keys are deterministic from a 32-byte seed (X-Wing's secret key), so the DEK persists and
//! restores verbatim. The seam (`encapsulate`/`decapsulate` over byte strings) is unchanged
//! from the earlier ML-KEM-only stand-in, so callers are unaffected by the upgrade.
//!
//! [Cryptography — Keys § Device Keys]: https://docs/design/cryptography/keys/#device-keys
//! [Primitives § KEM]: https://docs/design/cryptography/primitives/#kem

use ml_kem::kem::Decapsulate;
use ml_kem::{B32, DecapsulationKey, KeyExport, MlKem768, Seed};
use sha3::digest::{ExtendableOutput, Update, XofReader};
use sha3::{Digest, Sha3_256, Shake256};
use x25519_dalek::{X25519_BASEPOINT_BYTES, x25519};

use crate::crypto::{CryptoError, rng};

/// Length of the X-Wing secret-key seed. SHAKE256-expanded to 96 bytes: the ML-KEM-768 seed
/// `d ‖ z` (64) and the X25519 secret scalar (32).
pub const DEK_SEED_LEN: usize = 32;

/// X-Wing public-key length: `pk_M (1184) ‖ pk_X (32)`.
pub const DEK_PUBLIC_LEN: usize = 1184 + 32;
/// X-Wing ciphertext length: `ct_M (1088) ‖ ct_X (32)`.
pub const DEK_CIPHERTEXT_LEN: usize = 1088 + 32;
/// Length of the X-Wing encapsulation seed: ML-KEM coins `m` (32) and the X25519 ephemeral (32).
const ESEED_LEN: usize = 64;
const MLKEM_CT_LEN: usize = 1088;

/// The X-Wing combiner label: the 6-byte ASCII art `\.//^\` (`5c2e2f2f5e5c`).
const XWING_LABEL: [u8; 6] = [0x5c, 0x2e, 0x2f, 0x2f, 0x5e, 0x5c];

/// A device encryption keypair (X-Wing = X25519 + ML-KEM-768). Holds both private halves; the
/// public encapsulation key is derived on demand.
pub struct DekKeypair {
    /// The 32-byte X-Wing secret-key seed (the value sealed in the keystore).
    seed: [u8; DEK_SEED_LEN],
    /// ML-KEM-768 decapsulation key, derived from the seed.
    dk: DecapsulationKey<MlKem768>,
    /// X25519 secret scalar, derived from the seed.
    sk_x: [u8; 32],
}

fn shared_to_32(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    out
}

/// The X-Wing combiner: bind both shared secrets, the X25519 ciphertext, and the recipient's
/// static X25519 public key under SHA3-256 with the domain-separating label.
fn combiner(ss_m: &[u8], ss_x: &[u8; 32], ct_x: &[u8; 32], pk_x: &[u8; 32]) -> [u8; 32] {
    let mut h = Sha3_256::new();
    Digest::update(&mut h, ss_m);
    Digest::update(&mut h, ss_x);
    Digest::update(&mut h, ct_x);
    Digest::update(&mut h, pk_x);
    Digest::update(&mut h, XWING_LABEL);
    shared_to_32(&h.finalize())
}

impl DekKeypair {
    /// Generate a fresh DEK from the OS CSPRNG.
    pub fn generate() -> Self {
        Self::from_seed(&rng::random_array::<DEK_SEED_LEN>())
    }

    /// Reconstruct deterministically from the 32-byte X-Wing seed (`expandDecapsulationKey`).
    pub fn from_seed(seed: &[u8; DEK_SEED_LEN]) -> Self {
        // expanded = SHAKE256(seed, 96): [0:64] = ML-KEM d‖z, [64:96] = X25519 scalar.
        let mut xof = Shake256::default();
        xof.update(seed);
        let mut reader = xof.finalize_xof();
        let mut expanded = [0u8; 96];
        reader.read(&mut expanded);

        let s = Seed::try_from(&expanded[..64]).expect("64-byte ML-KEM seed");
        let dk = DecapsulationKey::<MlKem768>::from_seed(s);
        let sk_x = shared_to_32(&expanded[64..96]);
        Self {
            seed: *seed,
            dk,
            sk_x,
        }
    }

    /// Export the 32-byte X-Wing seed for sealed storage in the keystore.
    pub fn to_seed_bytes(&self) -> [u8; DEK_SEED_LEN] {
        self.seed
    }

    /// This device's static X25519 public key (`pk_X = X25519(sk_X, base)`).
    fn pk_x(&self) -> [u8; 32] {
        x25519(self.sk_x, X25519_BASEPOINT_BYTES)
    }

    /// The X-Wing public encapsulation-key bytes `pk_M ‖ pk_X` (for the device directory).
    pub fn public_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(DEK_PUBLIC_LEN);
        out.extend_from_slice(self.dk.encapsulation_key().to_bytes().as_slice());
        out.extend_from_slice(&self.pk_x());
        out
    }

    /// Encapsulate a fresh shared secret to this keypair's own public key, returning the
    /// X-Wing ciphertext `ct_M ‖ ct_X` and the 32-byte combined shared secret (sender side).
    pub fn encapsulate_to_self(&self) -> (Vec<u8>, [u8; 32]) {
        self.encapsulate_to_self_derand(&rng::random_array::<ESEED_LEN>())
    }

    /// Derandomized encapsulation to self (the X-Wing `EncapsDerand` with explicit `eseed`):
    /// `eseed[0:32]` is the ML-KEM coin `m`, `eseed[32:64]` the X25519 ephemeral scalar.
    /// Exposed for known-answer testing; the public path draws `eseed` from the OS CSPRNG.
    fn encapsulate_to_self_derand(&self, eseed: &[u8; ESEED_LEN]) -> (Vec<u8>, [u8; 32]) {
        let pk_x = self.pk_x();

        // ML-KEM-768 half: derandomized encapsulation to our own encapsulation key.
        let m = B32::try_from(&eseed[..32]).expect("32-byte m");
        let (ct_m, ss_m) = self.dk.encapsulation_key().encapsulate_deterministic(&m);

        // X25519 half: ephemeral scalar from eseed[32:64]; ct_X is its public key.
        let eph = shared_to_32(&eseed[32..64]);
        let ct_x = x25519(eph, X25519_BASEPOINT_BYTES);
        let ss_x = x25519(eph, pk_x);

        let ss = combiner(ss_m.as_slice(), &ss_x, &ct_x, &pk_x);
        let mut ct = Vec::with_capacity(DEK_CIPHERTEXT_LEN);
        ct.extend_from_slice(ct_m.as_slice());
        ct.extend_from_slice(&ct_x);
        (ct, ss)
    }

    /// Decapsulate an X-Wing ciphertext `ct_M ‖ ct_X`, recovering the 32-byte shared secret
    /// (receiver side). A wrong-length ciphertext is rejected; a foreign ciphertext recovers a
    /// different pseudo-random secret (ML-KEM implicit rejection), never an error.
    pub fn decapsulate(&self, ciphertext: &[u8]) -> Result<[u8; 32], CryptoError> {
        if ciphertext.len() != DEK_CIPHERTEXT_LEN {
            return Err(CryptoError::Malformed("X-Wing ciphertext wrong length"));
        }
        let (ct_m, ct_x_bytes) = ciphertext.split_at(MLKEM_CT_LEN);

        let ss_m = self
            .dk
            .decapsulate_slice(ct_m)
            .map_err(|_| CryptoError::Malformed("ML-KEM ciphertext wrong length"))?;

        let ct_x = shared_to_32(ct_x_bytes);
        let pk_x = self.pk_x();
        let ss_x = x25519(self.sk_x, ct_x);

        Ok(combiner(ss_m.as_slice(), &ss_x, &ct_x, &pk_x))
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
}
