//! The signing seam (SSoT: [Cryptography — Keys § Device Keys]).
//!
//! Every data-plane signature flows through [`Signer`] rather than a concrete
//! [`HybridSigningKey`], so a **hardware-bound** device signing key can be dropped in without
//! touching the call sites that build manifests and backup artifacts. The design binds only
//! the *device* signing key (DSK) to hardware; the user IK and per-epoch write-tier keys are
//! software, and [`HybridSigningKey`] is their [`Signer`] implementation here.
//!
//! Signing is **fallible** because a hardware element can refuse (the user cancels a
//! biometric, the element is unavailable). The software implementation never returns `Err`.
//! The hardware-backed implementation lands with [hardware-bound key storage] (`DEFERRED.md`).
//!
//! [Cryptography — Keys § Device Keys]: https://docs/design/cryptography/keys/#device-keys
//! [hardware-bound key storage]: https://docs/design/cryptography/keys/

use super::hybrid_sig::{HybridSignature, HybridSigningKey, HybridVerifyingKey};
use crate::crypto::CryptoError;

/// Produces hybrid signatures over arbitrary bytes and exposes the matching public key.
///
/// This is the abstraction the device signing key (DSK) is consumed through. Keeping it
/// object-safe (`&dyn Signer`) lets a software key and a future Secure Enclave / StrongBox /
/// TPM backend be used interchangeably at every signing site.
pub trait Signer {
    /// Sign `msg`, producing both signature halves (Ed25519 ‖ ML-DSA-65).
    ///
    /// Returns [`CryptoError`] only for a backend that can fail (hardware); the software
    /// [`HybridSigningKey`] implementation is infallible.
    fn sign(&self, msg: &[u8]) -> Result<HybridSignature, CryptoError>;

    /// The hybrid public key both halves of [`sign`](Self::sign) verify against.
    fn verifying_key(&self) -> HybridVerifyingKey;
}

/// The software signer: an in-memory hybrid key. Its `sign` never fails.
impl Signer for HybridSigningKey {
    fn sign(&self, msg: &[u8]) -> Result<HybridSignature, CryptoError> {
        Ok(HybridSigningKey::sign(self, msg))
    }

    fn verifying_key(&self) -> HybridVerifyingKey {
        HybridSigningKey::verifying_key(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn software_key_signs_through_the_trait_object() {
        let key = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let signer: &dyn Signer = &key;
        let sig = signer.sign(b"manifest bytes").unwrap();
        assert!(signer.verifying_key().verify(b"manifest bytes", &sig));
        // The trait object's public key matches the concrete key's.
        assert_eq!(signer.verifying_key(), key.verifying_key());
    }
}
