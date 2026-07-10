//! P-256 hybrid-DSK variant — **contract skeleton** (slice `S-A4` in the repo-root
//! `SLICES.md`; design: <https://docs/design/cryptography/keys/#device-keys>).
//!
//! Shipping secure elements (Secure Enclave, StrongBox, TPM 2.0) expose **ECDSA-P256**,
//! not Ed25519, so the hardware-backed device-key composition pairs a *hardware P-256
//! classical half* with the software-sealed ML-DSA-65 half. Only the software backend
//! composes end-to-end today; this module is the seam the three hardware backends plug
//! into once the variant lands.
//!
//! What the implementing slice must deliver (recorded here so the contract is binding):
//!
//! - `P256HybridSigningKey` implementing [`Signer`](super::signer::Signer), composing a
//!   [`HardwareSigner`](super::hardware::HardwareSigner) P-256 half (DER-encoded ECDSA
//!   signatures) with the software ML-DSA-65 half — which requires the hybrid signature
//!   and verifying-key types (and the device-directory entry) to become
//!   **algorithm-tagged** over [`ClassicalAlgorithm`] rather than assuming Ed25519.
//! - Verification-side dispatch in `verify_asset` on the directory entry's declared
//!   classical algorithm, with the existing Ed25519 path byte-for-byte unchanged.
//! - The per-platform smoke (sign + verify + non-exportability) against a real element.

use std::sync::Arc;

use super::hardware::{HardwareSigner, HardwareSignerError};

/// The classical half of a hybrid device-key composition. Ed25519 is the software
/// default; P-256 is what shipping secure elements provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassicalAlgorithm {
    /// Software (or future hardware) Ed25519 — today's only end-to-end composition.
    Ed25519,
    /// Hardware ECDSA-P256 (Secure Enclave / StrongBox / TPM 2.0).
    EcdsaP256,
}

/// A hybrid device signing key whose classical half is a **hardware-held P-256 key**
/// and whose ML-DSA-65 half is software-sealed. Skeleton: construction and signing are
/// unimplemented until slice `S-A4`.
pub struct P256HybridSigningKey {
    #[allow(dead_code)]
    hardware: Arc<dyn HardwareSigner>,
    #[allow(dead_code)]
    key_alias: String,
}

impl P256HybridSigningKey {
    /// Enroll a P-256 hardware key under `key_alias` and compose it with the software
    /// ML-DSA-65 half derived from `ml_seed` — the P-256 analogue of
    /// [`HardwareBackedSigner::enroll`](super::hardware::HardwareBackedSigner::enroll).
    ///
    /// # Panics
    /// Unimplemented skeleton (slice `S-A4`).
    pub fn enroll(
        hardware: Arc<dyn HardwareSigner>,
        key_alias: String,
        ml_seed: &[u8; 32],
    ) -> Result<Self, HardwareSignerError> {
        let (_, _, _) = (hardware, key_alias, ml_seed);
        todo!("S-A4: P-256 hybrid enrollment — see SLICES.md")
    }
}

#[cfg(test)]
mod tests {
    /// Acceptance criteria for slice `S-A4`, encoded as the contract test to un-ignore:
    /// enroll a mock P-256 element, sign a fixed payload, verify both halves through the
    /// algorithm-tagged hybrid verifying key, assert an Ed25519-only verifier rejects it,
    /// and assert non-exportability via the existing `assert_non_exportable` contract.
    #[test]
    #[ignore = "S-A4 contract: P-256 hybrid sign/verify round-trip not yet implemented"]
    fn p256_hybrid_round_trip_and_directory_dispatch() {
        unimplemented!("implemented by slice S-A4");
    }
}
