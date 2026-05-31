//! The signed device directory: how peers learn which device public keys to trust for a
//! user, and the anti-rollback `directory_version` (SSoT: [Cryptography — Keys § Device
//! Directory]).
//!
//! Each user publishes a directory listing their devices' hybrid signing public keys,
//! master-signed (here, by the user IK). `verify_asset` reads it to resolve the
//! `created_by_device` of a manifest and to enforce that a device's `added_at` precedes the
//! manifest timestamp. The monotonic `directory_version` lets readers refuse a rolled-back
//! directory (a server hiding a revocation).
//!
//! [Cryptography — Keys § Device Directory]: https://docs/design/cryptography/keys/#device-directory

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::hybrid_sig::{HybridSignature, HybridSigningKey, HybridVerifyingKey};

/// One device's published entry. A revoked device's entry is **retained** (marked with
/// `revoked_at`), never deleted, so manifests it signed before revocation stay verifiable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceEntry {
    /// Stable device id.
    pub device_id: Uuid,
    /// The device signing key's hybrid public half.
    pub dsk_public: HybridVerifyingKey,
    /// RFC3339 time the device was added (must precede any manifest it signs).
    pub added_at: String,
    /// RFC3339 revocation time, if revoked.
    pub revoked_at: Option<String>,
}

/// The unsigned core of a directory — exactly the bytes the master signature covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectoryCore {
    /// Account owner.
    pub user_id: Uuid,
    /// Monotonic; +1 on every change. Readers refuse a version below their high-water mark.
    pub directory_version: u64,
    /// RFC3339 last-update time.
    pub updated_at: String,
    /// The user's devices.
    pub devices: Vec<DeviceEntry>,
}

/// A master/IK-signed device directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDirectory {
    /// The signed core.
    pub core: DirectoryCore,
    /// Hybrid signature by the user IK over the canonical core bytes.
    pub signature: HybridSignature,
}

impl DirectoryCore {
    fn signing_bytes(&self) -> Vec<u8> {
        crate::cbor::to_canonical_vec(self).expect("directory core serializes")
    }

    /// Sign this core with the user IK, producing a [`DeviceDirectory`].
    pub fn sign(self, ik: &HybridSigningKey) -> DeviceDirectory {
        let signature = ik.sign(&self.signing_bytes());
        DeviceDirectory {
            core: self,
            signature,
        }
    }
}

impl DeviceDirectory {
    /// Verify the directory's signature against the user IK public key.
    pub fn verify(&self, ik_public: &HybridVerifyingKey) -> bool {
        ik_public.verify(&self.core.signing_bytes(), &self.signature)
    }

    /// Look up a device entry by id.
    pub fn device(&self, device_id: &Uuid) -> Option<&DeviceEntry> {
        self.core.devices.iter().find(|d| &d.device_id == device_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(version: u64, ik: &HybridSigningKey, device: &HybridSigningKey) -> DeviceDirectory {
        DirectoryCore {
            user_id: Uuid::from_u128(1),
            directory_version: version,
            updated_at: "2026-05-31T00:00:00Z".into(),
            devices: vec![DeviceEntry {
                device_id: Uuid::from_u128(0xD1),
                dsk_public: device.verifying_key(),
                added_at: "2026-05-30T00:00:00Z".into(),
                revoked_at: None,
            }],
        }
        .sign(ik)
    }

    #[test]
    fn sign_verify_and_lookup() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let dev = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let d = dir(1, &ik, &dev);

        assert!(d.verify(&ik.verifying_key()));
        // Wrong IK does not verify.
        assert!(!d.verify(&HybridSigningKey::from_seed_bytes(&[9; 32], &[9; 32]).verifying_key()));
        // Lookup.
        assert_eq!(
            d.device(&Uuid::from_u128(0xD1)).unwrap().dsk_public,
            dev.verifying_key()
        );
        assert!(d.device(&Uuid::from_u128(0xDEAD)).is_none());
    }

    #[test]
    fn tampering_with_a_device_key_breaks_the_signature() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let dev = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let mut d = dir(1, &ik, &dev);
        // Swap in a different device key without re-signing.
        d.core.devices[0].dsk_public =
            HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]).verifying_key();
        assert!(!d.verify(&ik.verifying_key()));
    }

    #[test]
    fn serializes_canonically() {
        let ik = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let dev = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let d = dir(7, &ik, &dev);
        let bytes = crate::cbor::to_canonical_vec(&d).unwrap();
        let back: DeviceDirectory = crate::cbor::from_slice(&bytes).unwrap();
        assert_eq!(back, d);
        assert!(back.verify(&ik.verifying_key()));
    }
}
