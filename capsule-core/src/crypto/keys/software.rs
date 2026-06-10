//! Software (no-secure-element) device signer — the universal fallback [`HardwareSigner`].
//!
//! [`SoftwareSigner`] is the software peer of the per-platform hardware backends (Secure
//! Enclave / StrongBox / TPM): it implements the same [`HardwareSigner`] contract, so the
//! [`HardwareBackedSigner`](super::HardwareBackedSigner) /
//! [`create_with_hardware_signer`](crate::lifecycle::Workspace::create_with_hardware_signer)
//! path runs unchanged on a device with **no** secure element. The classical Ed25519 key lives
//! in ordinary process memory, derived deterministically (HKDF-SHA512, per `key_alias`) from a
//! 32-byte seed the caller is responsible for sealing under the master key — exactly as the
//! ML-DSA-65 `ξ` seed is sealed — so the published device key is stable across restarts.
//!
//! Unlike a real element it offers **no** hardware non-exportability guarantee, and
//! [`assert_non_exportable`](HardwareSigner::assert_non_exportable) says so by returning
//! [`HardwareSignerError::Exportable`]. It is the reference the native software-signer examples
//! (Kotlin / Swift) mirror, and the only backend the CI smoke test exercises (Linux, in-process,
//! bypassing uniffi and any hardware/TPM requirement).
//!
//! For a device that never touches the hardware seam, the simpler path is the fully-software
//! [`HybridSigningKey`](super::HybridSigningKey) via
//! [`Workspace::create`](crate::lifecycle::Workspace::create) (both halves in software);
//! `SoftwareSigner` exists to drive the hardware-signer seam uniformly across all backends.

use ed25519_dalek::{Signer as _, SigningKey};

use super::hardware::{HardwareSigner, HardwareSignerError};
use crate::crypto::{kdf, rng};

/// HKDF-SHA512 `info` label binding the per-alias Ed25519 derivation to this construction.
const SOFTWARE_SIGNER_INFO: &[u8] = b"capsule/software-signer/ed25519/v1";

/// A software [`HardwareSigner`]: the fallback for devices with no secure element.
#[derive(Clone)]
pub struct SoftwareSigner {
    seed: [u8; 32],
}

impl SoftwareSigner {
    /// Build a signer from a 32-byte seed. The caller **must** seal `seed` (e.g. under the
    /// master key, like the ML-DSA-65 `ξ` seed) so the device key survives restarts.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        Self { seed }
    }

    /// Generate a signer from a fresh OS-CSPRNG seed. Persist [`seed`](Self::seed) before
    /// dropping it — it cannot be recovered afterwards.
    pub fn generate() -> Self {
        Self {
            seed: rng::random_array::<32>(),
        }
    }

    /// The seed, for sealed storage. Present *because* this is a software key — a real secure
    /// element has no equivalent (its private bytes never leave hardware).
    pub fn seed(&self) -> [u8; 32] {
        self.seed
    }

    /// The deterministic per-alias Ed25519 key. Distinct aliases get distinct keys.
    fn signing_key(&self, key_alias: &str) -> SigningKey {
        let ed_seed = kdf::derive_key32(&self.seed, key_alias.as_bytes(), SOFTWARE_SIGNER_INFO);
        SigningKey::from_bytes(&ed_seed)
    }
}

impl HardwareSigner for SoftwareSigner {
    fn enroll(&self, key_alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        // Derivation is idempotent per alias, so enrollment is just the public key.
        self.classical_public_key(key_alias)
    }

    fn classical_public_key(&self, key_alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        Ok(self
            .signing_key(&key_alias)
            .verifying_key()
            .to_bytes()
            .to_vec())
    }

    fn sign_classical(
        &self,
        key_alias: String,
        msg: Vec<u8>,
    ) -> Result<Vec<u8>, HardwareSignerError> {
        Ok(self.signing_key(&key_alias).sign(&msg).to_bytes().to_vec())
    }

    fn assert_non_exportable(&self, _key_alias: String) -> Result<(), HardwareSignerError> {
        // Honest by design: a software key lives in ordinary memory and *is* readable, so it can
        // never satisfy the non-exportability contract a real secure element does. The native
        // smoke harnesses assert this `Err` to prove they distinguish software from hardware.
        Err(HardwareSignerError::Exportable)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::crypto::keys::{HardwareBackedSigner, Signer as _};

    #[test]
    fn software_signer_smoke() {
        // The Linux software-signer smoke (keys.md Validation, software fallback): enroll the
        // device key, sign a payload, verify it against the published hybrid key. This bypasses
        // uniffi and all hardware, so it is the backend gated in CI.
        let hw = Arc::new(SoftwareSigner::from_seed([5u8; 32]));
        let signer =
            HardwareBackedSigner::enroll(hw.clone(), "device-dsk".into(), &[6u8; 32]).unwrap();

        let msg = b"asset manifest bytes";
        let sig = signer.sign(msg).unwrap();
        assert!(
            signer.verifying_key().verify(msg, &sig),
            "the software-composed hybrid signature must verify against the published key"
        );
        assert!(!signer.verifying_key().verify(b"tampered", &sig));

        // Determinism: the same sealed seed reproduces the same device key across restarts.
        let again = HardwareBackedSigner::enroll(
            Arc::new(SoftwareSigner::from_seed([5u8; 32])),
            "device-dsk".into(),
            &[6u8; 32],
        )
        .unwrap();
        assert_eq!(signer.verifying_key(), again.verifying_key());

        // It is honest about offering no hardware guarantee — unlike a real element.
        assert!(matches!(
            hw.assert_non_exportable("device-dsk".into()),
            Err(HardwareSignerError::Exportable)
        ));
    }

    #[test]
    fn distinct_aliases_get_distinct_keys() {
        let hw = SoftwareSigner::from_seed([1u8; 32]);
        assert_ne!(
            hw.classical_public_key("dsk-a".into()).unwrap(),
            hw.classical_public_key("dsk-b".into()).unwrap(),
            "per-alias derivation must not collide"
        );
    }
}
