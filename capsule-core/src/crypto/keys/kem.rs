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
use ml_kem::{B32, DecapsulationKey, EncapsulationKey, Key, KeyExport, MlKem768, Seed};
use sha3::digest::{ExtendableOutput, Update, XofReader};
use sha3::{Digest, Sha3_256, Shake256};
use x25519_dalek::{X25519_BASEPOINT_BYTES, x25519};

use crate::crypto::{CryptoError, rng};

/// Length of the X-Wing secret-key seed. SHAKE256-expanded to 96 bytes: the ML-KEM-768 seed
/// `d ‖ z` (64) and the X25519 secret scalar (32).
pub(crate) const DEK_SEED_LEN: usize = 32;

/// X-Wing public-key length: `pk_M (1184) ‖ pk_X (32)`.
pub const DEK_PUBLIC_LEN: usize = 1184 + 32;
/// X-Wing ciphertext length: `ct_M (1088) ‖ ct_X (32)`.
pub const DEK_CIPHERTEXT_LEN: usize = 1088 + 32;
/// Length of the X-Wing encapsulation seed: ML-KEM coins `m` (32) and the X25519 ephemeral (32).
pub const ESEED_LEN: usize = 64;
const MLKEM_CT_LEN: usize = 1088;
/// ML-KEM-768 encapsulation-key length (the `pk_M` prefix of an X-Wing public key).
const MLKEM_PK_LEN: usize = 1184;

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

/// Encapsulate a fresh shared secret to a **foreign** X-Wing public key `pk_M ‖ pk_X` —
/// the sender side when you hold only the recipient's public half (e.g. a web-upload guest
/// sealing an asset key to the link's Drop Key, which travels in the URL fragment). Returns
/// the X-Wing ciphertext `ct_M ‖ ct_X` ([`DEK_CIPHERTEXT_LEN`] bytes) and the 32-byte
/// combined shared secret. Draws the encapsulation randomness from the OS CSPRNG.
///
/// A wrong-length `public_bytes`, or one whose ML-KEM half fails decoding, is rejected
/// ([`CryptoError::Malformed`] / [`CryptoError::Key`]).
pub fn encapsulate_to_public(public_bytes: &[u8]) -> Result<(Vec<u8>, [u8; 32]), CryptoError> {
    encapsulate_to_public_derand(public_bytes, &rng::random_array::<ESEED_LEN>())
}

/// Derandomized [`encapsulate_to_public`] (X-Wing `EncapsDerand` to a foreign key): `eseed`
/// is the ML-KEM coin `m` (`[0:32]`) and the X25519 ephemeral scalar (`[32:64]`). Exposed
/// within the crate for known-answer/round-trip testing; the public path draws `eseed`
/// from the OS CSPRNG.
pub(crate) fn encapsulate_to_public_derand(
    public_bytes: &[u8],
    eseed: &[u8; ESEED_LEN],
) -> Result<(Vec<u8>, [u8; 32]), CryptoError> {
    if public_bytes.len() != DEK_PUBLIC_LEN {
        return Err(CryptoError::Malformed("X-Wing public key wrong length"));
    }
    let (pk_m_bytes, pk_x_bytes) = public_bytes.split_at(MLKEM_PK_LEN);

    // Reconstruct the recipient's ML-KEM-768 encapsulation key from its serialized form.
    let ek_encoded = Key::<EncapsulationKey<MlKem768>>::try_from(pk_m_bytes)
        .map_err(|_| CryptoError::Malformed("ML-KEM public key wrong length"))?;
    let ek = EncapsulationKey::<MlKem768>::new(&ek_encoded)
        .map_err(|_| CryptoError::Key("invalid ML-KEM public key"))?;

    let pk_x = shared_to_32(pk_x_bytes);

    // ML-KEM-768 half: derandomized encapsulation to the recipient's encapsulation key.
    let m = B32::try_from(&eseed[..32]).expect("32-byte m");
    let (ct_m, ss_m) = ek.encapsulate_deterministic(&m);

    // X25519 half: ephemeral scalar from eseed[32:64]; ct_X is its public key; the shared
    // secret is against the recipient's static X25519 public key `pk_X`.
    let eph = shared_to_32(&eseed[32..64]);
    let ct_x = x25519(eph, X25519_BASEPOINT_BYTES);
    let ss_x = x25519(eph, pk_x);

    let ss = combiner(ss_m.as_slice(), &ss_x, &ct_x, &pk_x);
    let mut ct = Vec::with_capacity(DEK_CIPHERTEXT_LEN);
    ct.extend_from_slice(ct_m.as_slice());
    ct.extend_from_slice(&ct_x);
    Ok((ct, ss))
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
mod tests;
