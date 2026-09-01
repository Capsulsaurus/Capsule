//! P-256 hybrid-DSK variant — the hardware-backed device signing key (slice `S-A4` in the
//! repo-root `SLICES.md`; design: <https://docs/design/cryptography/keys/#device-keys>).
//!
//! Shipping secure elements (Secure Enclave, StrongBox, TPM 2.0) expose **ECDSA-P256**,
//! not Ed25519, so the hardware-backed device-key composition pairs a *hardware P-256
//! classical half* with the software-sealed ML-DSA-65 half. [`P256HybridSigningKey`] composes
//! the two into the same algorithm-tagged [`HybridSignature`]/[`HybridVerifyingKey`] the
//! Ed25519 path produces, so every downstream signing site — asset manifests, the device
//! directory, `verify_asset` — dispatches on the key's declared [`ClassicalAlgorithm`] without
//! caring which curve produced the classical half.
//!
//! The hardware P-256 half is reached through the [`HardwareSigner`] seam: [`enroll`] reads the
//! element's P-256 public key, and [`sign`] asks the element for a **DER-encoded ECDSA**
//! signature over `SHA-256(msg)` — exactly what Secure Enclave and StrongBox emit. Only the
//! ML-DSA-65 half is in software (no shipping element holds PQ keys). The three native backends
//! (SE/StrongBox/TPM) plug into this same composition via the `HardwareSigner` foreign trait;
//! the software `p256` crate is the reference/mock element they replace.
//!
//! [`ClassicalAlgorithm`]: super::hybrid_sig::ClassicalAlgorithm
//! [`enroll`]: P256HybridSigningKey::enroll
//! [`sign`]: P256HybridSigningKey#impl-Signer-for-P256HybridSigningKey

use std::sync::Arc;

use ml_dsa::{B32, Keypair as _, MlDsa65, Signer as _, SigningKey as MlSigningKey};
use p256::ecdsa::VerifyingKey as P256VerifyingKey;

use super::hardware::{HardwareSigner, HardwareSignerError};
use super::hybrid_sig::{HybridSignature, HybridVerifyingKey};
use super::signer::Signer;
use crate::crypto::CryptoError;

/// Parse a hardware element's P-256 public key into a verifying key. Shipping elements emit the
/// point in one of three shapes: compressed SEC1 (33 bytes), uncompressed SEC1 (65 bytes,
/// `0x04‖x‖y`, e.g. Secure Enclave), or the bare `x‖y` coordinate pair (64 bytes, e.g. the TPM
/// reference in [`super::tpm`]). All three normalize to the same key.
fn parse_p256_public(point: &[u8]) -> Result<P256VerifyingKey, HardwareSignerError> {
    let vk = match point.len() {
        33 | 65 => P256VerifyingKey::from_sec1_bytes(point),
        64 => {
            let mut sec1 = Vec::with_capacity(65);
            sec1.push(0x04); // SEC1 uncompressed-point tag
            sec1.extend_from_slice(point);
            P256VerifyingKey::from_sec1_bytes(&sec1)
        }
        _ => {
            return Err(HardwareSignerError::Backend(
                "unexpected P-256 public key length".into(),
            ));
        }
    };
    vk.map_err(|_| HardwareSignerError::Backend("invalid P-256 public key".into()))
}

/// A hybrid device signing key whose classical half is a **hardware-held P-256 key** and whose
/// ML-DSA-65 half is software-sealed. The P-256 analogue of
/// [`HardwareBackedSigner`](super::hardware::HardwareBackedSigner).
pub struct P256HybridSigningKey {
    hardware: Arc<dyn HardwareSigner>,
    key_alias: String,
    ml: MlSigningKey<MlDsa65>,
    verifying_key: HybridVerifyingKey,
}

impl P256HybridSigningKey {
    /// Enroll a P-256 hardware key under `key_alias` and compose it with the software ML-DSA-65
    /// half derived from `ml_seed` (the `ξ` half of the sealed DSK seed). Reads the element's
    /// P-256 public key and builds the published hybrid verifying key from it plus the software
    /// ML-DSA-65 public key.
    pub fn enroll(
        hardware: Arc<dyn HardwareSigner>,
        key_alias: String,
        ml_seed: &[u8; 32],
    ) -> Result<Self, HardwareSignerError> {
        let point = hardware.enroll(key_alias.clone())?;
        let p256_vk = parse_p256_public(&point)?;
        let seed = B32::try_from(&ml_seed[..])
            .map_err(|_| HardwareSignerError::Backend("bad ML-DSA seed".into()))?;
        let ml = MlSigningKey::<MlDsa65>::from_seed(&seed);
        let verifying_key = HybridVerifyingKey::from_p256_parts(p256_vk, ml.verifying_key());
        Ok(Self {
            hardware,
            key_alias,
            ml,
            verifying_key,
        })
    }
}

impl Signer for P256HybridSigningKey {
    fn sign(&self, msg: &[u8]) -> Result<HybridSignature, CryptoError> {
        // The element signs SHA-256(msg) and returns a DER-encoded ECDSA signature; the ML-DSA-65
        // half is produced in software from the sealed ξ seed.
        let der = self
            .hardware
            .sign_classical(self.key_alias.clone(), msg.to_vec())
            .map_err(|_| CryptoError::Auth("hardware P-256 signature failed"))?;
        let ml = self.ml.sign(msg).encode().to_vec();
        Ok(HybridSignature::from_p256_halves(der, ml))
    }

    fn verifying_key(&self) -> HybridVerifyingKey {
        self.verifying_key.clone()
    }
}

/// An in-memory stand-in for a P-256 secure element (Secure Enclave / StrongBox / TPM). The
/// software `p256` crate is the reference implementation the real element replaces: it signs
/// SHA-256(msg) and returns DER-encoded ECDSA, and exposes its public key as uncompressed SEC1
/// (`0x04‖x‖y`), the format Secure Enclave emits. Test-only; a real element keeps the P-256
/// private key in hardware. Shared with the `verify_asset` chokepoint tests, so it lives outside
/// the `tests` module (mirroring `MockHardwareSigner`).
#[cfg(test)]
pub(crate) struct MockP256Element {
    sk: p256::ecdsa::SigningKey,
    exportable: bool,
}

#[cfg(test)]
impl MockP256Element {
    pub(crate) fn new(scalar: [u8; 32], exportable: bool) -> Self {
        Self {
            sk: p256::ecdsa::SigningKey::from_slice(&scalar).expect("valid P-256 scalar"),
            exportable,
        }
    }
}

#[cfg(test)]
impl HardwareSigner for MockP256Element {
    fn enroll(&self, alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        self.classical_public_key(alias)
    }
    fn classical_public_key(&self, _alias: String) -> Result<Vec<u8>, HardwareSignerError> {
        Ok(self
            .sk
            .verifying_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec())
    }
    fn sign_classical(&self, _alias: String, msg: Vec<u8>) -> Result<Vec<u8>, HardwareSignerError> {
        use p256::ecdsa::signature::Signer as _;
        let sig: p256::ecdsa::Signature = self.sk.sign(&msg);
        Ok(sig.to_der().as_bytes().to_vec())
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
    use std::sync::Arc;

    use super::*;
    use crate::crypto::keys::directory::{DeviceEntry, DirectoryCore};
    use crate::crypto::keys::{ClassicalAlgorithm, HybridSigningKey};

    /// The slice-`S-A4` acceptance criterion: enroll a mock P-256 element, sign a fixed payload,
    /// verify both halves through the algorithm-tagged hybrid verifying key, assert an
    /// Ed25519-only verifier rejects it, assert non-exportability, and prove directory dispatch
    /// on the entry's declared classical algorithm.
    #[test]
    fn p256_hybrid_round_trip_and_directory_dispatch() {
        use uuid::Uuid;

        // A mock P-256 secure element (software `p256`) stands in for the Secure Enclave.
        let element = Arc::new(MockP256Element::new([7u8; 32], false));
        let signer =
            P256HybridSigningKey::enroll(element.clone(), "device-dsk".into(), &[3u8; 32]).unwrap();

        // The published hybrid key is P-256-tagged; both halves verify.
        let vk = signer.verifying_key();
        assert_eq!(vk.classical_algorithm(), ClassicalAlgorithm::EcdsaP256);

        let msg = b"asset manifest bytes";
        let sig = signer.sign(msg).unwrap();
        assert_eq!(sig.classical_algorithm(), ClassicalAlgorithm::EcdsaP256);
        assert!(
            vk.verify(msg, &sig),
            "the hardware-composed P-256 hybrid signature must verify against the published key"
        );
        assert!(!vk.verify(b"tampered", &sig));

        // The hardware P-256 half is load-bearing: a different element (different P-256 key) with
        // the SAME software ML-DSA seed must not verify — so the P-256 half genuinely gates.
        let other = P256HybridSigningKey::enroll(
            Arc::new(MockP256Element::new([9u8; 32], false)),
            "other".into(),
            &[3u8; 32],
        )
        .unwrap();
        assert!(
            !other.verifying_key().verify(msg, &sig),
            "the hardware P-256 half must gate, not just the shared software PQ half"
        );

        // Algorithm dispatch: an Ed25519-only verifier must reject the P-256 signature.
        let ed_vk = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]).verifying_key();
        assert_eq!(ed_vk.classical_algorithm(), ClassicalAlgorithm::Ed25519);
        assert!(
            !ed_vk.verify(msg, &sig),
            "an Ed25519 verifier must reject a P-256 signature"
        );

        // Non-exportability contract (the existing `HardwareSigner` smoke): a conforming element
        // asserts Ok; an exportable one is detected.
        assert!(element.assert_non_exportable("device-dsk".into()).is_ok());
        assert!(
            MockP256Element::new([7; 32], true)
                .assert_non_exportable("x".into())
                .is_err()
        );

        // Serde keeps the P-256 tag recoverable across the wire, and the Ed25519 vectors below
        // (see hybrid_sig.rs) are unchanged because only the classical-half length differs.
        let sig_back: HybridSignature =
            crate::cbor::from_slice(&crate::cbor::to_canonical_vec(&sig).unwrap()).unwrap();
        assert_eq!(
            sig_back.classical_algorithm(),
            ClassicalAlgorithm::EcdsaP256
        );
        assert!(vk.verify(msg, &sig_back));
        let vk_back: HybridVerifyingKey =
            crate::cbor::from_slice(&crate::cbor::to_canonical_vec(&vk).unwrap()).unwrap();
        assert_eq!(vk_back, vk);

        // Directory dispatch: a signed device directory carries the P-256 device entry; lookup +
        // verify dispatches on the entry's declared classical algorithm.
        let ik = HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]);
        let directory = DirectoryCore {
            user_id: Uuid::from_u128(1),
            directory_version: 1,
            updated_at: "2026-05-31T00:00:00Z".into(),
            devices: vec![DeviceEntry {
                device_id: Uuid::from_u128(0xD1),
                dsk_public: vk.clone(),
                dek_public: None,
                added_at: "2026-05-30T00:00:00Z".into(),
                revoked_at: None,
            }],
        }
        .sign(&ik);
        assert!(directory.verify(&ik.verifying_key()));
        let entry = directory.device(&Uuid::from_u128(0xD1)).unwrap();
        assert_eq!(
            entry.dsk_public.classical_algorithm(),
            ClassicalAlgorithm::EcdsaP256
        );
        assert!(
            entry.dsk_public.verify(msg, &sig),
            "the directory's P-256 entry verifies the P-256 signature"
        );
        // A cross-algorithm Ed25519 signature does not pass the P-256 directory entry.
        let ed_sig = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]).sign(msg);
        assert!(!entry.dsk_public.verify(msg, &ed_sig));
    }
}
