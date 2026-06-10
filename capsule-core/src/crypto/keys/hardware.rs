//! Hardware-bound device signing (SSoT: [Cryptography — Keys § Device Keys]).
//!
//! The design binds the device signing key (DSK) to a per-platform secure element — Secure
//! Enclave (iOS), StrongBox (Android), TPM (desktop) — so the private key never leaves
//! hardware. No shipping secure element holds ML-DSA-65 keys, so only the **classical** Ed25519
//! half lives in hardware; the post-quantum ML-DSA-65 half stays software-sealed under the
//! master key. A hybrid signature is therefore composed from a hardware-produced Ed25519
//! signature and a software-produced ML-DSA-65 signature.
//!
//! [`HardwareSigner`] is the seam native code implements — over the uniffi foreign-trait
//! boundary under the `ffi` feature; a plain Rust trait otherwise. [`HardwareBackedSigner`]
//! adapts it to [`Signer`]. The per-platform round-trip + non-exportability harness is owned by
//! the design (`keys.md` Validation); the in-process contract is exercised here against a mock.
//!
//! [Cryptography — Keys § Device Keys]: https://docs/design/cryptography/keys/#device-keys

use std::sync::Arc;

use ml_dsa::{B32, Keypair as _, MlDsa65, Signer as _, SigningKey as MlSigningKey};

use super::hybrid_sig::{HybridSignature, HybridVerifyingKey};
use super::signer::Signer;
use crate::crypto::CryptoError;

/// The per-platform hardware secure element, implemented by native code (Swift/Kotlin) over the
/// uniffi foreign-trait boundary. Rust calls *into* it to enroll a non-exportable Ed25519 key
/// and to sign with it; the ML-DSA-65 half is handled in Rust.
#[cfg_attr(feature = "ffi", uniffi::export(with_foreign))]
pub trait HardwareSigner: Send + Sync {
    /// Generate (or bind to) the hardware Ed25519 keypair for `key_alias`, returning its
    /// 32-byte public key. Idempotent per alias.
    fn enroll(&self, key_alias: String) -> Result<Vec<u8>, HardwareSignerError>;

    /// The 32-byte Ed25519 public key for an already-enrolled `key_alias`.
    fn classical_public_key(&self, key_alias: String) -> Result<Vec<u8>, HardwareSignerError>;

    /// Produce a 64-byte Ed25519 signature over `msg` with the hardware key for `key_alias`.
    fn sign_classical(
        &self,
        key_alias: String,
        msg: Vec<u8>,
    ) -> Result<Vec<u8>, HardwareSignerError>;

    /// Non-exportability assertion: a conforming element MUST refuse to reveal the private
    /// bytes, returning [`HardwareSignerError::Exportable`] if they can be read. This drives the
    /// per-platform smoke test (`keys.md` Validation).
    fn assert_non_exportable(&self, key_alias: String) -> Result<(), HardwareSignerError>;
}

/// Failure surfaced by a [`HardwareSigner`] backend.
#[derive(Debug, thiserror::Error)]
#[cfg_attr(feature = "ffi", derive(uniffi::Error), uniffi(flat_error))]
pub enum HardwareSignerError {
    /// The user cancelled the biometric / the element refused authentication.
    #[error("hardware authentication cancelled")]
    AuthCancelled,
    /// No secure element is available on this device.
    #[error("hardware secure element unavailable")]
    Unavailable,
    /// No key exists for the requested alias.
    #[error("hardware key not found")]
    NotFound,
    /// The private key was readable — non-exportability is violated (a failure).
    #[error("hardware private key is exportable")]
    Exportable,
    /// Any other backend error.
    #[error("hardware backend error: {0}")]
    Backend(String),
}

/// Adapts a [`HardwareSigner`] to [`Signer`]: the Ed25519 half is produced in hardware, the
/// ML-DSA-65 half in software from the sealed `ξ` seed, and the two are combined into a
/// [`HybridSignature`].
pub struct HardwareBackedSigner {
    hardware: Arc<dyn HardwareSigner>,
    key_alias: String,
    ml: MlSigningKey<MlDsa65>,
    verifying_key: HybridVerifyingKey,
}

impl HardwareBackedSigner {
    /// Enroll the device key: ensure the hardware Ed25519 key exists (returning its public
    /// half) and build the published hybrid verifying key from it plus the software ML-DSA-65
    /// public key derived from `ml_seed` (the `ξ` half of the sealed DSK seed).
    pub fn enroll(
        hardware: Arc<dyn HardwareSigner>,
        key_alias: String,
        ml_seed: &[u8; 32],
    ) -> Result<Self, CryptoError> {
        let ed_public = hardware
            .enroll(key_alias.clone())
            .map_err(|_| CryptoError::Key("hardware key enrollment failed"))?;
        if ed_public.len() != 32 {
            return Err(CryptoError::Malformed(
                "hardware Ed25519 public key must be 32 bytes",
            ));
        }
        let seed =
            B32::try_from(&ml_seed[..]).map_err(|_| CryptoError::Malformed("bad ML-DSA seed"))?;
        let ml = MlSigningKey::<MlDsa65>::from_seed(&seed);

        let mut vk_bytes = ed_public;
        vk_bytes.extend_from_slice(ml.verifying_key().encode().as_slice());
        let verifying_key = HybridVerifyingKey::from_bytes(&vk_bytes)?;

        Ok(Self {
            hardware,
            key_alias,
            ml,
            verifying_key,
        })
    }
}

impl Signer for HardwareBackedSigner {
    fn sign(&self, msg: &[u8]) -> Result<HybridSignature, CryptoError> {
        let ed = self
            .hardware
            .sign_classical(self.key_alias.clone(), msg.to_vec())
            .map_err(|_| CryptoError::Auth("hardware classical signature failed"))?;
        let ed: [u8; 64] = ed
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::Malformed("hardware Ed25519 signature must be 64 bytes"))?;
        let ml = self.ml.sign(msg).encode().to_vec();
        Ok(HybridSignature::from_halves(ed, ml))
    }

    fn verifying_key(&self) -> HybridVerifyingKey {
        self.verifying_key.clone()
    }
}

/// An in-memory stand-in for a secure element. Test-only; a real element keeps the Ed25519 key
/// in hardware. Shared with the lifecycle tests, so it lives outside the `tests` module.
#[cfg(test)]
pub(crate) struct MockHardwareSigner {
    ed: ed25519_dalek::SigningKey,
    exportable: bool,
}

#[cfg(test)]
impl MockHardwareSigner {
    pub(crate) fn new(ed_seed: [u8; 32], exportable: bool) -> Self {
        Self {
            ed: ed25519_dalek::SigningKey::from_bytes(&ed_seed),
            exportable,
        }
    }
}

#[cfg(test)]
impl HardwareSigner for MockHardwareSigner {
    fn enroll(&self, alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        self.classical_public_key(alias)
    }
    fn classical_public_key(&self, _alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        Ok(self.ed.verifying_key().to_bytes().to_vec())
    }
    fn sign_classical(&self, _alias: String, msg: Vec<u8>) -> Result<Vec<u8>, HardwareSignerError> {
        use ed25519_dalek::Signer as _;
        Ok(self.ed.sign(&msg).to_bytes().to_vec())
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
mod tests {
    use super::*;

    #[test]
    fn hardware_backed_signer_round_trips_and_asserts_non_exportable() {
        // keys.md Validation (hardware-bound storage round-trip): generate the DSK in the
        // element, sign a payload, verify against the published pubkey, assert non-exportability.
        let hw = Arc::new(MockHardwareSigner::new([7; 32], false));
        let signer =
            HardwareBackedSigner::enroll(hw.clone(), "device-dsk".into(), &[3; 32]).unwrap();

        let msg = b"asset manifest bytes";
        let sig = signer.sign(msg).unwrap();
        assert!(
            signer.verifying_key().verify(msg, &sig),
            "the hardware-composed hybrid signature must verify against the published key"
        );
        assert!(!signer.verifying_key().verify(b"tampered", &sig));
        assert!(hw.assert_non_exportable("device-dsk".into()).is_ok());

        // A non-conforming (exportable) element is detected.
        let bad = MockHardwareSigner::new([7; 32], true);
        assert!(bad.assert_non_exportable("x".into()).is_err());
    }

    #[test]
    fn the_hardware_ed25519_half_is_load_bearing() {
        // Two devices share the same software ML-DSA seed but different hardware Ed25519 keys: a
        // signature from one must not verify under the other's published key — so the hardware
        // half genuinely gates, not just the software PQ half.
        let a = HardwareBackedSigner::enroll(
            Arc::new(MockHardwareSigner::new([1; 32], false)),
            "a".into(),
            &[9; 32],
        )
        .unwrap();
        let b = HardwareBackedSigner::enroll(
            Arc::new(MockHardwareSigner::new([2; 32], false)),
            "b".into(),
            &[9; 32],
        )
        .unwrap();
        let sig = a.sign(b"m").unwrap();
        assert!(a.verifying_key().verify(b"m", &sig));
        assert!(!b.verifying_key().verify(b"m", &sig));
    }
}
