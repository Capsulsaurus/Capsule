//! The **hardware-bound P-256 variant** of the Device Encryption Key (DEK); design:
//! <https://docs/design/cryptography/keys/#device-keys>.
//!
//! The software DEK is **X-Wing** (X25519 + ML-KEM-768; see [`super::kem`]). Shipping secure
//! elements expose **ECDH-P256**, not X25519, so — mirroring the P-256 hybrid **signing** key
//! ([`super::p256`]) — the hardware-backed DEK replaces the X25519 classical half with a
//! *hardware-held P-256 key* while the post-quantum **ML-KEM-768** half stays software-sealed
//! (no shipping element holds PQ keys). The doc pins the composition (`keys.md` §Device Keys):
//!
//! > **DEK** (Device Encryption Key): hybrid **X25519 + ML-KEM-768**. … The **classical half** …
//! > is **generated inside and never leaves hardware** … the **PQ half** (… ML-KEM-768) is
//! > software-sealed … Shipping elements also expose **P-256**, not Ed25519/X25519, so the
//! > hardware-backed composition is the planned **P-256 hybrid variant**.
//!
//! ## Composition
//!
//! The hybrid mirrors the X-Wing combiner structure, substituting the P-256 ECDH half for the
//! X25519 half (the classical half is the *only* thing that changes, exactly as the DSK's P-256
//! variant swaps ECDSA-P256 for Ed25519):
//!
//! - **public key** `ek_M (1184) ‖ pk_P (65, uncompressed SEC1)`
//! - **ciphertext** `ct_M (1088) ‖ ct_P (65, the sender's ephemeral P-256 public key)`
//! - **shared secret** `SHA3-256(ss_M ‖ ss_P ‖ ct_P ‖ pk_P ‖ label)`, where `ss_M` is the ML-KEM
//!   shared secret and `ss_P` is the raw 32-byte P-256 ECDH secret (the point's x-coordinate).
//!
//! A distinct domain-separation `label` ([`P256_HYBRID_LABEL`]) is bound in place of X-Wing's, so
//! a P-256-variant ciphertext and an X-Wing ciphertext can never derive the same secret. The
//! algorithm is **tagged by length**: the P-256 public/ciphertext are 65-byte-classical-half
//! longer than X-Wing's 32-byte-classical-half forms, so the two never alias and the existing
//! X-Wing bytes ([`super::kem`]) are unchanged — the X25519 path is byte-for-byte identical.
//!
//! The classical half is reached through the [`HardwareKeyAgreement`] seam: [`P256HybridDek::enroll`]
//! reads the element's static P-256 public key, and [`P256HybridDek::decapsulate`] asks the element
//! to ECDH the sender's ephemeral public key — so the private P-256 scalar never leaves hardware.
//! **Encapsulation is pure software** (the sender holds only the recipient's public bytes), matching
//! [`super::kem::encapsulate_to_public`].

use std::sync::Arc;

use ml_kem::kem::Decapsulate;
use ml_kem::{B32, DecapsulationKey, EncapsulationKey, Key, KeyExport, MlKem768, Seed};
use p256::elliptic_curve::sec1::ToEncodedPoint;
use sha3::digest::{ExtendableOutput, Update, XofReader};
use sha3::{Digest, Sha3_256, Shake256};

use super::hardware::{HardwareKeyAgreement, HardwareSignerError};
use crate::crypto::{CryptoError, rng};

/// ML-KEM-768 encapsulation-key length (the `ek_M` prefix).
const MLKEM_PK_LEN: usize = 1184;
/// ML-KEM-768 ciphertext length (the `ct_M` prefix).
const MLKEM_CT_LEN: usize = 1088;
/// Uncompressed SEC1 (x9.63) P-256 point length (`0x04‖x‖y`) — the wire form of `pk_P` / `ct_P`.
const P256_POINT_LEN: usize = 65;
/// Raw P-256 ECDH shared-secret length (the 32-byte big-endian x-coordinate).
const P256_SS_LEN: usize = 32;

/// P-256-hybrid DEK public-key length: `ek_M (1184) ‖ pk_P (65)`.
pub const DEK_P256_PUBLIC_LEN: usize = MLKEM_PK_LEN + P256_POINT_LEN;
/// P-256-hybrid DEK ciphertext length: `ct_M (1088) ‖ ct_P (65)`.
pub const DEK_P256_CIPHERTEXT_LEN: usize = MLKEM_CT_LEN + P256_POINT_LEN;

/// Domain-separation label for the P-256 hybrid KEM combiner. Distinct from X-Wing's 6-byte label
/// so a P-256-variant ciphertext and an X-Wing ciphertext can never combine to the same secret.
const P256_HYBRID_LABEL: &[u8] = b"Capsule-P256-hybrid-KEM-v1";

fn to_32(bytes: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    out.copy_from_slice(bytes);
    out
}

/// Parse an uncompressed-SEC1 (or compressed) P-256 public key.
fn parse_p256_public(point: &[u8]) -> Result<p256::PublicKey, CryptoError> {
    p256::PublicKey::from_sec1_bytes(point)
        .map_err(|_| CryptoError::Key("invalid P-256 public key"))
}

/// Uncompressed-SEC1 bytes for a P-256 public key (`0x04‖x‖y`, 65 bytes).
fn uncompressed(pk: &p256::PublicKey) -> Vec<u8> {
    pk.to_encoded_point(false).as_bytes().to_vec()
}

/// The hybrid combiner: bind both shared secrets, the P-256 ciphertext (ephemeral public key), and
/// the recipient's static P-256 public key under SHA3-256 with the domain-separating label. Mirrors
/// the X-Wing combiner ([`super::kem`]) with the P-256 ECDH half substituted for the X25519 half.
fn combiner(ss_m: &[u8], ss_p: &[u8; P256_SS_LEN], ct_p: &[u8], pk_p: &[u8]) -> [u8; 32] {
    let mut h = Sha3_256::new();
    Digest::update(&mut h, ss_m);
    Digest::update(&mut h, ss_p);
    Digest::update(&mut h, ct_p);
    Digest::update(&mut h, pk_p);
    Digest::update(&mut h, P256_HYBRID_LABEL);
    to_32(&h.finalize())
}

/// Expand a 32-byte software seed to the 64-byte ML-KEM-768 `d‖z` seed (SHAKE256), matching how
/// the X-Wing DEK derives its ML-KEM half. Only the ML-KEM half is seed-derived; the classical
/// half is hardware-held and non-exportable, so it is not part of any sealed seed.
fn mlkem_from_seed(ml_seed: &[u8; 32]) -> DecapsulationKey<MlKem768> {
    let mut xof = Shake256::default();
    xof.update(ml_seed);
    let mut reader = xof.finalize_xof();
    let mut dz = [0u8; 64];
    reader.read(&mut dz);
    let s = Seed::try_from(&dz[..]).expect("64-byte ML-KEM seed");
    DecapsulationKey::<MlKem768>::from_seed(s)
}

/// A device encryption keypair whose **classical half is a hardware-held P-256 key** and whose
/// ML-KEM-768 half is software-sealed. The KEM analogue of
/// [`P256HybridSigningKey`](super::p256::P256HybridSigningKey), and the hardware-bound counterpart
/// of the software X-Wing [`DekKeypair`](super::kem::DekKeypair).
pub struct P256HybridDek {
    agreement: Arc<dyn HardwareKeyAgreement>,
    key_alias: String,
    /// The 32-byte software seed the ML-KEM-768 half was expanded from. Retained so the keystore
    /// can seal it under the master key and re-derive this half on unlock; the classical
    /// half needs no seed because it never leaves the element.
    ml_seed: [u8; 32],
    /// ML-KEM-768 decapsulation key (software half).
    dk: DecapsulationKey<MlKem768>,
    /// The element's static P-256 public key (`pk_P`) — the private scalar stays in hardware.
    p256_public: p256::PublicKey,
}

impl P256HybridDek {
    /// Enroll a P-256 key-agreement key under `key_alias` in `hardware` and compose it with the
    /// software ML-KEM-768 half derived from `ml_seed` (the software-sealed 32-byte seed). Reads
    /// the element's static P-256 public key and builds the published hybrid public key from it
    /// plus the ML-KEM-768 encapsulation key.
    pub fn enroll(
        hardware: Arc<dyn HardwareKeyAgreement>,
        key_alias: String,
        ml_seed: &[u8; 32],
    ) -> Result<Self, HardwareSignerError> {
        let point = hardware.enroll(key_alias.clone())?;
        let p256_public = parse_p256_public(&point)
            .map_err(|_| HardwareSignerError::Backend("bad P-256 key".into()))?;
        Ok(Self {
            agreement: hardware,
            key_alias,
            ml_seed: *ml_seed,
            dk: mlkem_from_seed(ml_seed),
            p256_public,
        })
    }

    /// The element alias this DEK's classical half is enrolled under. Persisted (in the clear —
    /// it is a lookup handle, not a secret) in the [`AccountFile`](super::keystore::AccountFile)
    /// so a reopened workspace can re-bind to the same hardware key.
    pub fn key_alias(&self) -> &str {
        &self.key_alias
    }

    /// The 32-byte software seed of the ML-KEM-768 half — the value the keystore seals under the
    /// master key, mirroring [`DekKeypair::to_seed_bytes`](super::kem::DekKeypair::to_seed_bytes).
    /// The classical half is *not* covered: it is hardware-held and non-exportable by contract,
    /// which is exactly why a hardware-bound device cannot be restored from a backup.
    pub fn to_ml_seed_bytes(&self) -> [u8; 32] {
        self.ml_seed
    }

    /// The published hybrid public-encapsulation-key bytes `ek_M ‖ pk_P` (for the device directory).
    pub fn public_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(DEK_P256_PUBLIC_LEN);
        out.extend_from_slice(self.dk.encapsulation_key().to_bytes().as_slice());
        out.extend_from_slice(&uncompressed(&self.p256_public));
        out
    }

    /// Decapsulate a P-256-hybrid ciphertext `ct_M ‖ ct_P`, recovering the 32-byte shared secret
    /// (receiver side). The ML-KEM half is decapsulated in software; the P-256 half is ECDH'd
    /// **inside the hardware element** against the sender's ephemeral public key `ct_P`. A
    /// wrong-length ciphertext is rejected; a foreign ML-KEM ciphertext recovers a different
    /// pseudo-random secret (ML-KEM implicit rejection), never an error.
    pub fn decapsulate(&self, ciphertext: &[u8]) -> Result<[u8; 32], CryptoError> {
        if ciphertext.len() != DEK_P256_CIPHERTEXT_LEN {
            return Err(CryptoError::Malformed(
                "P-256 hybrid ciphertext wrong length",
            ));
        }
        let (ct_m, ct_p) = ciphertext.split_at(MLKEM_CT_LEN);

        let ss_m = self
            .dk
            .decapsulate_slice(ct_m)
            .map_err(|_| CryptoError::Malformed("ML-KEM ciphertext wrong length"))?;

        // The hardware element performs the ECDH; the raw 32-byte x-coordinate comes back.
        let ss_p_bytes = self
            .agreement
            .key_agreement(self.key_alias.clone(), ct_p.to_vec())
            .map_err(|_| CryptoError::Auth("hardware P-256 ECDH failed"))?;
        if ss_p_bytes.len() != P256_SS_LEN {
            return Err(CryptoError::Malformed(
                "hardware ECDH secret must be 32 bytes",
            ));
        }
        let ss_p = to_32(&ss_p_bytes);
        let pk_p = uncompressed(&self.p256_public);

        Ok(combiner(ss_m.as_slice(), &ss_p, ct_p, &pk_p))
    }
}

impl std::fmt::Debug for P256HybridDek {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("P256HybridDek(****)")
    }
}

/// Encapsulate a fresh shared secret to a **foreign** P-256-hybrid public key `ek_M ‖ pk_P` — the
/// sender side, holding only the recipient's public half. Pure software (no hardware): it mirrors
/// [`super::kem::encapsulate_to_public`] for the P-256 variant. Returns the ciphertext
/// `ct_M ‖ ct_P` ([`DEK_P256_CIPHERTEXT_LEN`] bytes) and the 32-byte combined shared secret.
///
/// A wrong-length `public_bytes`, or one whose ML-KEM/P-256 half fails decoding, is rejected.
pub fn encapsulate_to_p256_public(public_bytes: &[u8]) -> Result<(Vec<u8>, [u8; 32]), CryptoError> {
    encapsulate_to_p256_public_derand(
        public_bytes,
        &rng::random_array::<32>(),
        random_p256_secret(),
    )
}

/// Draw a valid random P-256 secret scalar from the OS CSPRNG. `from_slice` rejects the vanishing
/// fraction of 32-byte draws that fall outside `[1, n-1]`; the loop re-draws (expected ~1 attempt).
fn random_p256_secret() -> p256::SecretKey {
    loop {
        if let Ok(sk) = p256::SecretKey::from_slice(&rng::random_array::<32>()) {
            return sk;
        }
    }
}

/// Derandomized [`encapsulate_to_p256_public`]: `mlkem_coin` is the ML-KEM encapsulation coin `m`
/// and `ephemeral` is the sender's P-256 ephemeral secret. Exposed within the crate for
/// deterministic round-trip testing; the public path draws both from the OS CSPRNG.
pub(crate) fn encapsulate_to_p256_public_derand(
    public_bytes: &[u8],
    mlkem_coin: &[u8; 32],
    ephemeral: p256::SecretKey,
) -> Result<(Vec<u8>, [u8; 32]), CryptoError> {
    if public_bytes.len() != DEK_P256_PUBLIC_LEN {
        return Err(CryptoError::Malformed(
            "P-256 hybrid public key wrong length",
        ));
    }
    let (ek_bytes, pk_p_bytes) = public_bytes.split_at(MLKEM_PK_LEN);

    // ML-KEM-768 half: reconstruct the recipient's encapsulation key and derandomize-encapsulate.
    let ek_encoded = Key::<EncapsulationKey<MlKem768>>::try_from(ek_bytes)
        .map_err(|_| CryptoError::Malformed("ML-KEM public key wrong length"))?;
    let ek = EncapsulationKey::<MlKem768>::new(&ek_encoded)
        .map_err(|_| CryptoError::Key("invalid ML-KEM public key"))?;
    let m = B32::try_from(&mlkem_coin[..]).expect("32-byte m");
    let (ct_m, ss_m) = ek.encapsulate_deterministic(&m);

    // P-256 half: `ct_P` is the ephemeral public key; `ss_P` is ECDH against the recipient's
    // static `pk_P`.
    let recipient = parse_p256_public(pk_p_bytes)?;
    let ct_p = uncompressed(&ephemeral.public_key());
    let shared = p256::ecdh::diffie_hellman(ephemeral.to_nonzero_scalar(), recipient.as_affine());
    let ss_p = to_32(shared.raw_secret_bytes().as_slice());
    let pk_p = uncompressed(&recipient);

    let ss = combiner(ss_m.as_slice(), &ss_p, &ct_p, &pk_p);
    let mut ct = Vec::with_capacity(DEK_P256_CIPHERTEXT_LEN);
    ct.extend_from_slice(ct_m.as_slice());
    ct.extend_from_slice(&ct_p);
    Ok((ct, ss))
}

/// An in-memory stand-in for a P-256 key-agreement secure element (Secure Enclave / StrongBox /
/// TPM). The software `p256` crate is the reference the real element replaces: it holds the static
/// P-256 key, exposes its public key as uncompressed SEC1 (`0x04‖x‖y`, the form Secure Enclave's
/// `P256.KeyAgreement` emits), and ECDHs against a peer's ephemeral public key. Test-only; a real
/// element keeps the scalar in hardware. Shared with the `verify_asset`/FFI tests, so it lives
/// outside the `tests` module (mirroring `MockP256Element`).
#[cfg(any(test, feature = "test-support"))]
pub struct MockP256KeyAgreement {
    sk: p256::SecretKey,
    exportable: bool,
}

#[cfg(any(test, feature = "test-support"))]
impl MockP256KeyAgreement {
    /// A mock element over `scalar`/`seed`; `exportable` makes it fail the non-exportability assertion.
    pub fn new(scalar: [u8; 32], exportable: bool) -> Self {
        Self {
            sk: p256::SecretKey::from_slice(&scalar).expect("valid P-256 scalar"),
            exportable,
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
impl HardwareKeyAgreement for MockP256KeyAgreement {
    fn enroll(&self, alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        self.public_key(alias)
    }
    fn public_key(&self, _alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        Ok(uncompressed(&self.sk.public_key()))
    }
    fn key_agreement(
        &self,
        _alias: String,
        peer_public: Vec<u8>,
    ) -> Result<Vec<u8>, HardwareSignerError> {
        let peer = p256::PublicKey::from_sec1_bytes(&peer_public)
            .map_err(|_| HardwareSignerError::Backend("bad peer P-256 key".into()))?;
        let shared = p256::ecdh::diffie_hellman(self.sk.to_nonzero_scalar(), peer.as_affine());
        Ok(shared.raw_secret_bytes().as_slice().to_vec())
    }
    fn assert_non_exportable(&self, _alias: String) -> Result<(), HardwareSignerError> {
        if self.exportable {
            Err(HardwareSignerError::Exportable)
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
