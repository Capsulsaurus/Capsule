//! A software keystore: an [`Account`] (master key + user identity key + this device's
//! keys) and its encrypted-at-rest form [`AccountFile`].
//!
//! This stands in for the hardware-bound keystores (Secure Enclave / StrongBox / TPM) the
//! design specifies per platform — those are deferred (see `DEFERRED.md`). Here the master
//! key is wrapped under a passphrase via [`pwkdf`], and the device identity private keys
//! are sealed under the master key (the design's "master key wraps device identity private
//! keys"). SSoT: [Cryptography — Keys].
//!
//! [Cryptography — Keys]: https://docs/design/cryptography/keys/

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::hybrid_sig::HybridSigningKey;
use super::kem::DekKeypair;
use super::master::MasterKey;
use crate::crypto::primitives::{Argon2Params, DeviceTier};
use crate::crypto::{CryptoError, pwkdf};

/// This device's key material: a stable id, a hybrid Device Signing Key (DSK), and a
/// Device Encryption Key (DEK).
pub struct DeviceKeys {
    /// Stable per-device identifier (UUIDv7), published in the device directory.
    pub device_id: Uuid,
    /// Hybrid device signing key — signs asset manifests (`device_sig`).
    pub dsk: HybridSigningKey,
    /// Device encryption key (KEM) — receives key wraps.
    pub dek: DekKeypair,
}

/// A fully unlocked account in memory.
pub struct Account {
    /// The account owner's user id (UUIDv7).
    pub user_id: Uuid,
    /// The backed-up root key.
    pub master: MasterKey,
    /// The user identity key (root of signing trust). Signs the device directory.
    pub user_ik: HybridSigningKey,
    /// This device's keys.
    pub device: DeviceKeys,
}

/// The encrypted-at-rest account: the master key wrapped under a passphrase, and the
/// device/identity private keys sealed under the master key. Safe to persist to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountFile {
    /// Account owner id.
    pub user_id: Uuid,
    /// This device's id.
    pub device_id: Uuid,
    /// Master key wrapped under the passphrase (Argon2id + AES-256-GCM).
    pub wrapped_master: pwkdf::WrappedSecret,
    /// User IK seeds (64 bytes) sealed under the master key.
    #[serde(with = "serde_bytes")]
    pub sealed_ik: Vec<u8>,
    /// Device DSK seeds (64 bytes) sealed under the master key.
    #[serde(with = "serde_bytes")]
    pub sealed_dsk: Vec<u8>,
    /// Device DEK seed (64 bytes) sealed under the master key.
    #[serde(with = "serde_bytes")]
    pub sealed_dek: Vec<u8>,
}

impl Account {
    /// Create a brand-new account with a fresh master key, user IK, and first device.
    pub fn create() -> Self {
        Self {
            user_id: Uuid::now_v7(),
            master: MasterKey::generate(),
            user_ik: HybridSigningKey::generate(),
            device: DeviceKeys {
                device_id: Uuid::now_v7(),
                dsk: HybridSigningKey::generate(),
                dek: DekKeypair::generate(),
            },
        }
    }

    /// Encrypt the account for persistence: master under `passphrase` (cost = `tier`),
    /// identity/device private keys under the master key.
    pub fn to_file(&self, passphrase: &[u8], tier: DeviceTier) -> Result<AccountFile, CryptoError> {
        self.to_file_with(passphrase, tier.params())
    }

    /// As [`to_file`](Self::to_file) but with explicit Argon2id parameters (used by tests
    /// to avoid the multi-hundred-MiB production cost).
    pub fn to_file_with(
        &self,
        passphrase: &[u8],
        params: Argon2Params,
    ) -> Result<AccountFile, CryptoError> {
        Ok(AccountFile {
            user_id: self.user_id,
            device_id: self.device.device_id,
            wrapped_master: pwkdf::wrap_with(self.master.as_bytes(), passphrase, params)?,
            sealed_ik: self.master.seal(&self.user_ik.to_seed_bytes()),
            sealed_dsk: self.master.seal(&self.device.dsk.to_seed_bytes()),
            sealed_dek: self.master.seal(&self.device.dek.to_seed_bytes()),
        })
    }
}

fn seed64(bytes: Vec<u8>) -> Result<[u8; 64], CryptoError> {
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| CryptoError::Malformed("sealed key seed wrong length"))
}

impl AccountFile {
    /// Decrypt the account with `passphrase`. Returns [`CryptoError::Auth`] on a wrong
    /// passphrase (master unwrap fails) or tampering.
    pub fn unlock(&self, passphrase: &[u8]) -> Result<Account, CryptoError> {
        let master_bytes: [u8; 32] = pwkdf::unwrap(&self.wrapped_master, passphrase)?
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::Malformed("master key wrong length"))?;
        let master = MasterKey::from_bytes(master_bytes);

        let user_ik = HybridSigningKey::from_seed64(&seed64(master.open(&self.sealed_ik)?)?);
        let dsk = HybridSigningKey::from_seed64(&seed64(master.open(&self.sealed_dsk)?)?);
        let dek = DekKeypair::from_seed(&seed64(master.open(&self.sealed_dek)?)?);

        Ok(Account {
            user_id: self.user_id,
            master,
            user_ik,
            device: DeviceKeys {
                device_id: self.device_id,
                dsk,
                dek,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fast Argon2 params keep keystore tests quick; the production tier table is asserted
    // in `primitives` without paying the 128–512 MiB hashing cost.
    fn fast() -> Argon2Params {
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        }
    }

    #[test]
    fn account_create_save_unlock_round_trip() {
        let acct = Account::create();
        let ik_vk = acct.user_ik.verifying_key();
        let dsk_vk = acct.device.dsk.verifying_key();
        let default_album = acct.master.derive_default_album_id();

        let file = acct.to_file_with(b"passphrase", fast()).unwrap();
        let restored = file.unlock(b"passphrase").unwrap();

        assert_eq!(restored.user_id, acct.user_id);
        assert_eq!(restored.device.device_id, acct.device.device_id);
        // Identity and device verifying keys survive the round trip.
        assert_eq!(restored.user_ik.verifying_key(), ik_vk);
        assert_eq!(restored.device.dsk.verifying_key(), dsk_vk);
        // The master key still derives the same default album id.
        assert_eq!(restored.master.derive_default_album_id(), default_album);
    }

    #[test]
    fn wrong_passphrase_fails_to_unlock() {
        let acct = Account::create();
        let file = acct.to_file_with(b"right", fast()).unwrap();
        assert!(file.unlock(b"wrong").is_err());
    }

    #[test]
    fn account_file_serializes_canonically() {
        let acct = Account::create();
        let file = acct.to_file_with(b"pw", fast()).unwrap();
        let bytes = crate::cbor::to_canonical_vec(&file).unwrap();
        let back: AccountFile = crate::cbor::from_slice(&bytes).unwrap();
        assert_eq!(back.user_id, file.user_id);
        assert_eq!(back.sealed_dek, file.sealed_dek);
    }
}
