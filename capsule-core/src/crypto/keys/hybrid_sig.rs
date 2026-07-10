//! Hybrid classical + ML-DSA-65 signatures — the long-lived identity signature used for
//! the user IK, device keys (DSK), asset manifests, write-tier keys, sidecars, the device
//! directory, and backup manifests (SSoT: [Cryptography — Primitives § Signature Scheme]).
//!
//! **Both halves must verify** for a signature to be accepted. Neither algorithm being
//! broken alone compromises authentication, and because both halves cover the same bytes
//! (including `crypto_suite_id`), the construction is downgrade-resistant even if one
//! algorithm is later broken.
//!
//! The classical half is **algorithm-tagged** over [`ClassicalAlgorithm`]: software keys use
//! **Ed25519** (the end-to-end default), while the hardware-backed device key pairs the
//! ML-DSA-65 half with a **hardware ECDSA-P256** classical half — because shipping secure
//! elements (Secure Enclave, StrongBox, TPM 2.0) expose P-256, not Ed25519 (see
//! [`crypto::keys::p256`](super::p256)). The tag is carried by the key/signature themselves
//! (recovered from the classical-half length on the wire), so the Ed25519 serialization is
//! **byte-for-byte identical** to before the P-256 variant existed.
//!
//! Ed25519 keys are deterministic from 32-byte seeds (Ed25519 secret scalar / ML-DSA `ξ`), so
//! a software signing key serializes as 64 seed bytes and can be wrapped and restored verbatim.
//!
//! [Cryptography — Primitives § Signature Scheme]: https://docs/design/cryptography/primitives/#signature-scheme

use ed25519_dalek::{
    Signature as EdSignature, Signer as _, SigningKey as EdSigningKey, Verifier as _,
    VerifyingKey as EdVerifyingKey,
};
use ml_dsa::{
    B32, EncodedSignature, EncodedVerifyingKey, Keypair as _, MlDsa65, Signature as MlSignature,
    Signer as _, SigningKey as MlSigningKey, Verifier as _, VerifyingKey as MlVerifyingKey,
};
use p256::ecdsa::{Signature as P256Signature, VerifyingKey as P256VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::crypto::{CryptoError, rng};

/// Ed25519 secret/seed length and ML-DSA `ξ` seed length (both 32 bytes).
const SEED_LEN: usize = 32;
/// Ed25519 public key length.
const ED_PK_LEN: usize = 32;
/// Ed25519 signature length.
const ED_SIG_LEN: usize = 64;
/// ML-DSA-65 public key length (FIPS-204 level-3).
const ML_PK_LEN: usize = 1952;
/// Compressed SEC1 NIST P-256 public key length (`0x02|0x03` prefix + 32-byte x).
const P256_PK_LEN: usize = 33;

/// The classical half of a hybrid device-key composition. Ed25519 is the software default;
/// ECDSA-P256 is what shipping secure elements provide (Secure Enclave / StrongBox / TPM 2.0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassicalAlgorithm {
    /// Software (or future hardware) Ed25519 — today's end-to-end software composition.
    Ed25519,
    /// Hardware ECDSA-P256 with DER-encoded signatures over `SHA-256(msg)`.
    EcdsaP256,
}

/// The classical half of a [`HybridVerifyingKey`], tagged by algorithm.
#[derive(Clone)]
enum ClassicalVerifyingKey {
    Ed25519(EdVerifyingKey),
    EcdsaP256(P256VerifyingKey),
}

/// The classical half of a [`HybridSignature`], tagged by algorithm. The Ed25519 half is a
/// fixed 64-byte signature; the P-256 half is a DER-encoded ECDSA signature.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClassicalSignature {
    Ed25519([u8; ED_SIG_LEN]),
    EcdsaP256(Vec<u8>),
}

/// A hybrid signing keypair (private) — the **software** composition, both halves in memory.
/// The hardware-backed composition lives in [`HardwareBackedSigner`](super::HardwareBackedSigner)
/// (Ed25519 hardware half) and [`P256HybridSigningKey`](super::p256::P256HybridSigningKey)
/// (P-256 hardware half); both drive the same [`HybridSignature`]/[`HybridVerifyingKey`] types.
#[derive(Clone)]
pub struct HybridSigningKey {
    ed: EdSigningKey,
    ml: MlSigningKey<MlDsa65>,
}

/// A hybrid public verifying key. Published in the device directory.
#[derive(Clone)]
pub struct HybridVerifyingKey {
    classical: ClassicalVerifyingKey,
    ml: MlVerifyingKey<MlDsa65>,
}

/// A hybrid signature: a classical half (Ed25519 or ECDSA-P256) and an ML-DSA-65 half over the
/// same message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HybridSignature {
    classical: ClassicalSignature,
    ml: Vec<u8>,
}

impl HybridSignature {
    /// Assemble an **Ed25519** hybrid signature from its two halves. Used by the hardware-backed
    /// signer, which produces the Ed25519 half inside a secure element and the ML-DSA-65 half in
    /// software. The halves are not validated here — verification happens in
    /// [`HybridVerifyingKey::verify`].
    pub(crate) fn from_halves(ed: [u8; ED_SIG_LEN], ml: Vec<u8>) -> Self {
        Self {
            classical: ClassicalSignature::Ed25519(ed),
            ml,
        }
    }

    /// Assemble an **ECDSA-P256** hybrid signature from a DER-encoded classical half and the
    /// software ML-DSA-65 half. Used by [`P256HybridSigningKey`](super::p256::P256HybridSigningKey),
    /// whose hardware element emits DER ECDSA over `SHA-256(msg)`.
    pub(crate) fn from_p256_halves(der: Vec<u8>, ml: Vec<u8>) -> Self {
        Self {
            classical: ClassicalSignature::EcdsaP256(der),
            ml,
        }
    }

    /// The classical algorithm this signature's classical half is encoded for.
    pub fn classical_algorithm(&self) -> ClassicalAlgorithm {
        match self.classical {
            ClassicalSignature::Ed25519(_) => ClassicalAlgorithm::Ed25519,
            ClassicalSignature::EcdsaP256(_) => ClassicalAlgorithm::EcdsaP256,
        }
    }
}

fn to_ml_seed(bytes: &[u8; SEED_LEN]) -> B32 {
    B32::try_from(&bytes[..]).expect("32-byte ML-DSA seed")
}

/// Verify a DER-encoded ECDSA-P256 signature over `msg`. RustCrypto's `p256` verifier hashes
/// `msg` with SHA-256 internally (matching what a secure element signs) and accepts both
/// low-S and high-S DER encodings, so a signature is never rejected for S-normalization alone.
fn p256_verify(vk: &P256VerifyingKey, msg: &[u8], der: &[u8]) -> bool {
    match P256Signature::from_der(der) {
        Ok(sig) => vk.verify(msg, &sig).is_ok(),
        Err(_) => false,
    }
}

impl HybridSigningKey {
    /// Generate a fresh hybrid keypair from the OS CSPRNG.
    pub fn generate() -> Self {
        let seeds = rng::random_array::<{ 2 * SEED_LEN }>();
        let mut ed_seed = [0u8; SEED_LEN];
        let mut ml = [0u8; SEED_LEN];
        ed_seed.copy_from_slice(&seeds[..SEED_LEN]);
        ml.copy_from_slice(&seeds[SEED_LEN..]);
        Self::from_seed_bytes(&ed_seed, &ml)
    }

    /// Reconstruct a keypair deterministically from its two 32-byte seeds.
    pub fn from_seed_bytes(ed_seed: &[u8; SEED_LEN], ml_seed: &[u8; SEED_LEN]) -> Self {
        Self {
            ed: EdSigningKey::from_bytes(ed_seed),
            ml: MlSigningKey::<MlDsa65>::from_seed(&to_ml_seed(ml_seed)),
        }
    }

    /// Export the two 32-byte seeds (Ed25519 secret ‖ ML-DSA ξ) for sealed storage.
    pub fn to_seed_bytes(&self) -> [u8; 2 * SEED_LEN] {
        let mut out = [0u8; 2 * SEED_LEN];
        out[..SEED_LEN].copy_from_slice(&self.ed.to_bytes());
        out[SEED_LEN..].copy_from_slice(self.ml.to_seed().as_slice());
        out
    }

    /// Reconstruct from a 64-byte concatenation of the two seeds.
    pub fn from_seed64(bytes: &[u8; 2 * SEED_LEN]) -> Self {
        let mut ed_seed = [0u8; SEED_LEN];
        let mut ml = [0u8; SEED_LEN];
        ed_seed.copy_from_slice(&bytes[..SEED_LEN]);
        ml.copy_from_slice(&bytes[SEED_LEN..]);
        Self::from_seed_bytes(&ed_seed, &ml)
    }

    /// The public verifying key.
    pub fn verifying_key(&self) -> HybridVerifyingKey {
        HybridVerifyingKey {
            classical: ClassicalVerifyingKey::Ed25519(self.ed.verifying_key()),
            ml: self.ml.verifying_key(),
        }
    }

    /// Sign `msg`, producing both halves. ML-DSA uses the deterministic variant.
    pub fn sign(&self, msg: &[u8]) -> HybridSignature {
        let ed = self.ed.sign(msg).to_bytes();
        let ml = self.ml.sign(msg).encode().to_vec();
        HybridSignature {
            classical: ClassicalSignature::Ed25519(ed),
            ml,
        }
    }
}

impl HybridVerifyingKey {
    /// Assemble an **ECDSA-P256** hybrid verifying key from a hardware P-256 public key and the
    /// software ML-DSA-65 public half. Used by
    /// [`P256HybridSigningKey`](super::p256::P256HybridSigningKey) at enrollment.
    pub(crate) fn from_p256_parts(vk: P256VerifyingKey, ml: MlVerifyingKey<MlDsa65>) -> Self {
        Self {
            classical: ClassicalVerifyingKey::EcdsaP256(vk),
            ml,
        }
    }

    /// The classical algorithm this key's classical half is encoded for.
    pub fn classical_algorithm(&self) -> ClassicalAlgorithm {
        match self.classical {
            ClassicalVerifyingKey::Ed25519(_) => ClassicalAlgorithm::Ed25519,
            ClassicalVerifyingKey::EcdsaP256(_) => ClassicalAlgorithm::EcdsaP256,
        }
    }

    /// Verify `sig` over `msg`. Returns `true` only if **both** halves verify. The classical
    /// half dispatches on this key's declared algorithm: a signature whose classical half is a
    /// *different* algorithm than the key (an Ed25519 sig against a P-256 key, or vice versa)
    /// never verifies.
    pub fn verify(&self, msg: &[u8], sig: &HybridSignature) -> bool {
        let classical_ok = match (&self.classical, &sig.classical) {
            (ClassicalVerifyingKey::Ed25519(vk), ClassicalSignature::Ed25519(s)) => {
                vk.verify(msg, &EdSignature::from_bytes(s)).is_ok()
            }
            (ClassicalVerifyingKey::EcdsaP256(vk), ClassicalSignature::EcdsaP256(der)) => {
                p256_verify(vk, msg, der)
            }
            // Algorithm mismatch between key and signature — never accept.
            _ => false,
        };
        // Short-circuit only matters for cost; correctness requires both.
        let ml_ok = match EncodedSignature::<MlDsa65>::try_from(sig.ml.as_slice()) {
            Ok(enc) => match MlSignature::<MlDsa65>::decode(&enc) {
                Some(s) => self.ml.verify(msg, &s).is_ok(),
                None => false,
            },
            Err(_) => false,
        };
        classical_ok && ml_ok
    }

    /// The classical half's raw bytes: Ed25519 public key (32) or compressed SEC1 P-256 public
    /// key (33).
    fn classical_bytes(&self) -> Vec<u8> {
        match &self.classical {
            ClassicalVerifyingKey::Ed25519(vk) => vk.to_bytes().to_vec(),
            ClassicalVerifyingKey::EcdsaP256(vk) => vk.to_encoded_point(true).as_bytes().to_vec(),
        }
    }

    /// Raw bytes: the classical public key (Ed25519 = 32, P-256 = 33) followed by the ML-DSA-65
    /// public key (1952). The classical-half length is what distinguishes the two algorithms on
    /// decode, so an Ed25519 key round-trips to the exact same bytes it always has.
    pub fn to_bytes(&self) -> Vec<u8> {
        let classical = self.classical_bytes();
        let mut out = Vec::with_capacity(classical.len() + ML_PK_LEN);
        out.extend_from_slice(&classical);
        out.extend_from_slice(self.ml.encode().as_slice());
        out
    }

    /// Reconstruct from the `classical ‖ ml` byte layout produced by [`to_bytes`](Self::to_bytes).
    /// The ML-DSA-65 half is the fixed-length 1952-byte suffix; the classical prefix is 32 bytes
    /// (Ed25519) or 33 bytes (compressed SEC1 P-256), which selects the algorithm.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        if bytes.len() <= ML_PK_LEN {
            return Err(CryptoError::Malformed("hybrid verifying key too short"));
        }
        let (classical_b, ml_b) = bytes.split_at(bytes.len() - ML_PK_LEN);
        let classical = match classical_b.len() {
            ED_PK_LEN => {
                let arr: [u8; ED_PK_LEN] = classical_b
                    .try_into()
                    .map_err(|_| CryptoError::Malformed("bad Ed25519 public key length"))?;
                let ed = EdVerifyingKey::from_bytes(&arr)
                    .map_err(|_| CryptoError::Key("invalid Ed25519 public key"))?;
                ClassicalVerifyingKey::Ed25519(ed)
            }
            P256_PK_LEN => {
                let vk = P256VerifyingKey::from_sec1_bytes(classical_b)
                    .map_err(|_| CryptoError::Key("invalid P-256 public key"))?;
                ClassicalVerifyingKey::EcdsaP256(vk)
            }
            _ => return Err(CryptoError::Malformed("bad classical public key length")),
        };
        let enc = EncodedVerifyingKey::<MlDsa65>::try_from(ml_b)
            .map_err(|_| CryptoError::Malformed("bad ML-DSA public key length"))?;
        let ml = MlVerifyingKey::<MlDsa65>::decode(&enc);
        Ok(Self { classical, ml })
    }
}

impl PartialEq for HybridVerifyingKey {
    fn eq(&self, other: &Self) -> bool {
        self.to_bytes() == other.to_bytes()
    }
}
impl Eq for HybridVerifyingKey {}

impl std::fmt::Debug for HybridVerifyingKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.classical {
            ClassicalVerifyingKey::Ed25519(vk) => {
                write!(
                    f,
                    "HybridVerifyingKey(ed25519={})",
                    hex::encode(vk.to_bytes())
                )
            }
            ClassicalVerifyingKey::EcdsaP256(vk) => write!(
                f,
                "HybridVerifyingKey(p256={})",
                hex::encode(vk.to_encoded_point(true).as_bytes())
            ),
        }
    }
}

// ── serde: signatures and verifying keys serialize as CBOR byte strings ─────────

#[derive(Serialize, Deserialize)]
struct SigWire {
    #[serde(with = "serde_bytes")]
    ed: Vec<u8>,
    #[serde(with = "serde_bytes")]
    ml: Vec<u8>,
}

impl Serialize for HybridSignature {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let classical = match &self.classical {
            ClassicalSignature::Ed25519(sig) => sig.to_vec(),
            ClassicalSignature::EcdsaP256(der) => der.clone(),
        };
        SigWire {
            ed: classical,
            ml: self.ml.clone(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for HybridSignature {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let w = SigWire::deserialize(d)?;
        // The classical half is either a 64-byte Ed25519 signature or a variable-length DER
        // ECDSA-P256 signature (never 64 bytes for a real P-256 signature). Verification is
        // key-driven, so a misclassified half can only fail verification, never falsely accept.
        let classical = if w.ed.len() == ED_SIG_LEN {
            let ed: [u8; ED_SIG_LEN] =
                w.ed.as_slice()
                    .try_into()
                    .expect("length checked to equal ED_SIG_LEN");
            ClassicalSignature::Ed25519(ed)
        } else {
            ClassicalSignature::EcdsaP256(w.ed)
        };
        Ok(HybridSignature {
            classical,
            ml: w.ml,
        })
    }
}

impl Serialize for HybridVerifyingKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        SigWire {
            ed: self.classical_bytes(),
            ml: self.ml.encode().to_vec(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for HybridVerifyingKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let w = SigWire::deserialize(d)?;
        let mut bytes = w.ed;
        bytes.extend_from_slice(&w.ml);
        HybridVerifyingKey::from_bytes(&bytes).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
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
}
