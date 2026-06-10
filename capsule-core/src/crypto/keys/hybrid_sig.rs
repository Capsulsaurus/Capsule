//! Hybrid Ed25519 + ML-DSA-65 signatures — the long-lived identity signature used for
//! the user IK, device keys (DSK), asset manifests, write-tier keys, sidecars, the device
//! directory, and backup manifests (SSoT: [Cryptography — Primitives § Signature Scheme]).
//!
//! **Both halves must verify** for a signature to be accepted. Neither algorithm being
//! broken alone compromises authentication, and because both halves cover the same bytes
//! (including `crypto_suite_id`), the construction is downgrade-resistant even if one
//! algorithm is later broken.
//!
//! Keys are deterministic from 32-byte seeds (Ed25519 secret scalar / ML-DSA `ξ`), so a
//! signing key serializes as 64 seed bytes and can be wrapped and restored verbatim.
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
use serde::{Deserialize, Serialize};

use crate::crypto::CryptoError;
use crate::crypto::rng;

/// Ed25519 secret/seed length and ML-DSA `ξ` seed length (both 32 bytes).
const SEED_LEN: usize = 32;
/// Ed25519 public key length.
const ED_PK_LEN: usize = 32;
/// Ed25519 signature length.
const ED_SIG_LEN: usize = 64;

/// A hybrid signing keypair (private). Holds both algorithm halves.
#[derive(Clone)]
pub struct HybridSigningKey {
    ed: EdSigningKey,
    ml: MlSigningKey<MlDsa65>,
}

/// A hybrid public verifying key. Published in the device directory.
#[derive(Clone)]
pub struct HybridVerifyingKey {
    ed: EdVerifyingKey,
    ml: MlVerifyingKey<MlDsa65>,
}

/// A hybrid signature: an Ed25519 half and an ML-DSA-65 half over the same message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HybridSignature {
    ed: [u8; ED_SIG_LEN],
    ml: Vec<u8>,
}

impl HybridSignature {
    /// Assemble a signature from its two halves. Used by the hardware-backed signer, which
    /// produces the Ed25519 half inside a secure element and the ML-DSA-65 half in software.
    /// The halves are not validated here — verification happens in
    /// [`HybridVerifyingKey::verify`].
    pub(crate) fn from_halves(ed: [u8; ED_SIG_LEN], ml: Vec<u8>) -> Self {
        Self { ed, ml }
    }
}

fn to_ml_seed(bytes: &[u8; SEED_LEN]) -> B32 {
    B32::try_from(&bytes[..]).expect("32-byte ML-DSA seed")
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
            ed: self.ed.verifying_key(),
            ml: self.ml.verifying_key(),
        }
    }

    /// Sign `msg`, producing both halves. ML-DSA uses the deterministic variant.
    pub fn sign(&self, msg: &[u8]) -> HybridSignature {
        let ed = self.ed.sign(msg).to_bytes();
        let ml = self.ml.sign(msg).encode().to_vec();
        HybridSignature { ed, ml }
    }
}

impl HybridVerifyingKey {
    /// Verify `sig` over `msg`. Returns `true` only if **both** halves verify.
    pub fn verify(&self, msg: &[u8], sig: &HybridSignature) -> bool {
        let ed_ok = self
            .ed
            .verify(msg, &EdSignature::from_bytes(&sig.ed))
            .is_ok();
        // Short-circuit only matters for cost; correctness requires both.
        let ml_ok = match EncodedSignature::<MlDsa65>::try_from(sig.ml.as_slice()) {
            Ok(enc) => match MlSignature::<MlDsa65>::decode(&enc) {
                Some(s) => self.ml.verify(msg, &s).is_ok(),
                None => false,
            },
            Err(_) => false,
        };
        ed_ok && ml_ok
    }

    /// Raw bytes: Ed25519 public key (32) followed by ML-DSA-65 public key (1952).
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(ED_PK_LEN + 1952);
        out.extend_from_slice(&self.ed.to_bytes());
        out.extend_from_slice(self.ml.encode().as_slice());
        out
    }

    /// Reconstruct from the `ed (32) ‖ ml` byte layout produced by [`to_bytes`](Self::to_bytes).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CryptoError> {
        if bytes.len() <= ED_PK_LEN {
            return Err(CryptoError::Malformed("hybrid verifying key too short"));
        }
        let (ed_b, ml_b) = bytes.split_at(ED_PK_LEN);
        let ed_arr: [u8; ED_PK_LEN] = ed_b
            .try_into()
            .map_err(|_| CryptoError::Malformed("bad Ed25519 public key length"))?;
        let ed = EdVerifyingKey::from_bytes(&ed_arr)
            .map_err(|_| CryptoError::Key("invalid Ed25519 public key"))?;
        let enc = EncodedVerifyingKey::<MlDsa65>::try_from(ml_b)
            .map_err(|_| CryptoError::Malformed("bad ML-DSA public key length"))?;
        let ml = MlVerifyingKey::<MlDsa65>::decode(&enc);
        Ok(Self { ed, ml })
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
        write!(
            f,
            "HybridVerifyingKey(ed={})",
            hex::encode(self.ed.to_bytes())
        )
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
        SigWire {
            ed: self.ed.to_vec(),
            ml: self.ml.clone(),
        }
        .serialize(s)
    }
}

impl<'de> Deserialize<'de> for HybridSignature {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let w = SigWire::deserialize(d)?;
        let ed: [u8; ED_SIG_LEN] =
            w.ed.as_slice()
                .try_into()
                .map_err(|_| D::Error::custom("Ed25519 signature must be 64 bytes"))?;
        Ok(HybridSignature { ed, ml: w.ml })
    }
}

impl Serialize for HybridVerifyingKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        SigWire {
            ed: self.ed.to_bytes().to_vec(),
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
        sig.ed[0] ^= 0x01; // ML-DSA half still valid
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
            ed: sig_a.ed,
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
