//! The library's record of a **hardware-bound device signing key**: the half of the account the
//! library, not `capsule-core`, keeps on disk.
//!
//! A device whose signing key's classical half lives in a secure element (Secure Enclave,
//! StrongBox, TPM) cannot rebuild that key from the account file: the account file holds the
//! software DSK, and the element's P-256 key is non-exportable by contract. Reopening such a
//! library needs three things the account file does not carry, and this record holds them in
//! `{root}/.library/device_signer.cbor`:
//!
//! - the element's **alias** for the key, so reopening asks the element for the same key;
//! - the **ML-DSA-65 seed** of the post-quantum half, sealed under the account master key with a
//!   domain prefix, so the same hybrid key is rebuilt;
//! - the **published hybrid verifying key**, so a reopen can prove the element still holds the
//!   key this device published rather than one minted under the same alias since.
//!
//! The record's presence is the binding. A library without one signs with the account's software
//! DSK, which is every library created before this record existed, and an element offered on
//! reopen is ignored for it. A library with one refuses to open without its element: falling
//! back to the software DSK would change the key the device directory publishes (SSoT:
//! [Cryptography — Keys § Device Keys]).
//!
//! [Cryptography — Keys § Device Keys]: https://docs/design/cryptography/keys/#device-keys

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use capsule_core::cbor;
use capsule_core::crypto::CryptoError;
use capsule_core::crypto::keys::{
    HardwareSigner, HardwareSignerError, HybridVerifyingKey, MasterKey, P256HybridSigningKey,
    Signer,
};
use serde::{Deserialize, Serialize};

use crate::lifecycle::LifecycleError;

type Result<T> = std::result::Result<T, LifecycleError>;

/// The record's file under `{root}/.library/`.
const DEVICE_SIGNER_FILE: &str = "device_signer.cbor";

/// The only record version this build reads or writes.
const DEVICE_SIGNER_VERSION: u32 = 1;

/// Prefix sealed in front of the ML-DSA seed, so no other 32-byte value sealed under the same
/// master key (the account file's sealed DEK seed) opens as this one.
const SEALED_ML_SEED_DOMAIN: &[u8] = b"capsule-device-signer-ml-seed/v1\0";

/// A request to bind this device's **signing key** to a secure element over ECDSA-P256: the
/// element itself plus the alias its key is held under. The signer counterpart of
/// [`HardwareDekBinding`](crate::lifecycle::HardwareDekBinding).
///
/// The alias is used only when the binding is **created**; on reopen the alias recorded in the
/// library wins, so renaming it here can never re-bind an existing library to another key.
#[derive(Clone)]
pub struct HardwareSignerBinding {
    /// The secure element producing the P-256 signatures.
    pub element: Arc<dyn HardwareSigner>,
    /// The element's alias for this device's signing key.
    pub key_alias: String,
}

impl std::fmt::Debug for HardwareSignerBinding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HardwareSignerBinding")
            .field("key_alias", &self.key_alias)
            .finish_non_exhaustive()
    }
}

/// The persisted binding of a hardware-held P-256 device signing key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeviceSignerRecord {
    /// [`DEVICE_SIGNER_VERSION`]; anything else is refused rather than guessed at.
    version: u32,
    /// The element's alias for the P-256 key.
    key_alias: String,
    /// The ML-DSA-65 seed, domain-prefixed and sealed under the account master key.
    #[serde(with = "serde_bytes")]
    sealed_ml_seed: Vec<u8>,
    /// The hybrid verifying key this device published when the binding was made.
    dsk_public: HybridVerifyingKey,
}

impl DeviceSignerRecord {
    /// Where a library at `root` keeps the record.
    pub(crate) fn path(root: &Path) -> PathBuf {
        root.join(".library").join(DEVICE_SIGNER_FILE)
    }

    /// Enroll `binding`'s key and compose it with the ML-DSA half from `ml_seed`, returning the
    /// signer and the record that rebuilds it under `master`.
    ///
    /// Refused with [`CryptoError::Key`] when the element will not enroll; nothing falls back
    /// to software here.
    #[tracing::instrument(skip_all, fields(key_alias = %binding.key_alias))]
    pub(crate) fn enroll(
        binding: HardwareSignerBinding,
        ml_seed: &[u8; 32],
        master: &MasterKey,
    ) -> Result<(P256HybridSigningKey, Self)> {
        let signer =
            P256HybridSigningKey::enroll(binding.element, binding.key_alias.clone(), ml_seed)
                .map_err(|e| {
                    // The element's own reason is the only thing that says whether a retry can
                    // help, so it is logged before the crypto error flattens it.
                    tracing::error!(error = %e, "device signer: hardware P-256 enrollment failed");
                    CryptoError::Key("hardware device signing key enrollment failed")
                })?;
        let mut plaintext = Vec::with_capacity(SEALED_ML_SEED_DOMAIN.len() + ml_seed.len());
        plaintext.extend_from_slice(SEALED_ML_SEED_DOMAIN);
        plaintext.extend_from_slice(ml_seed);
        let record = Self {
            version: DEVICE_SIGNER_VERSION,
            key_alias: binding.key_alias,
            sealed_ml_seed: master.seal(&plaintext),
            dsk_public: signer.verifying_key(),
        };
        tracing::info!("device signer: hardware P-256 signing key enrolled");
        Ok((signer, record))
    }

    /// The element's alias the key is recorded under.
    pub(crate) fn key_alias(&self) -> &str {
        &self.key_alias
    }

    /// The record of the library at `root`, or `None` when it has none (a software-signed
    /// library). A record that is present but unreadable, undecodable or of another version is
    /// an error: treating it as absent would quietly move the library onto its software key.
    pub(crate) fn load(root: &Path) -> Result<Option<Self>> {
        let path = Self::path(root);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => {
                tracing::warn!(error = %e, "device signer: record unreadable");
                return Err(LifecycleError::Io(format!("device signer record: {e}")));
            }
        };
        let record: Self = cbor::from_slice(&bytes).map_err(|e| {
            tracing::warn!(error = %e, "device signer: record does not decode");
            LifecycleError::Cbor(format!("device signer record: {e}"))
        })?;
        if record.version != DEVICE_SIGNER_VERSION {
            tracing::warn!(
                version = record.version,
                "device signer: record of an unknown version"
            );
            return Err(LifecycleError::Cbor(format!(
                "device signer record version {} is not {DEVICE_SIGNER_VERSION}",
                record.version
            )));
        }
        Ok(Some(record))
    }

    /// Write the record for the library at `root`, temp-then-rename so a crash leaves either
    /// no record or a whole one.
    pub(crate) fn persist(&self, root: &Path) -> Result<()> {
        let path = Self::path(root);
        let bytes =
            cbor::to_canonical_vec(self).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        let tmp = crate::utils::paths::tmp_path(&path);
        fs::write(&tmp, &bytes).map_err(|e| LifecycleError::Io(e.to_string()))?;
        fs::rename(&tmp, &path).map_err(|e| LifecycleError::Io(e.to_string()))?;
        tracing::debug!(key_alias = %self.key_alias, "device signer: record persisted");
        Ok(())
    }

    /// Remove a record left by a creation that stopped before its account file was written. Such
    /// a record is sealed under a master key that no account holds, so it can never open.
    pub(crate) fn remove_stale(root: &Path) -> Result<()> {
        match fs::remove_file(Self::path(root)) {
            Ok(()) => {
                tracing::warn!("device signer: removed a record left by an interrupted creation");
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(LifecycleError::Io(format!("device signer record: {e}"))),
        }
    }

    /// Rebuild the signer this record describes: open the ML-DSA seed under `master` and ask
    /// `element` for the key under the recorded alias.
    ///
    /// The element is only asked for the key, never to make one: a key the element no longer
    /// holds is refused, not replaced. The rebuilt key must equal the one this device
    /// published, or the open is refused rather than signing under a key no peer trusts.
    #[tracing::instrument(skip_all, fields(key_alias = %self.key_alias))]
    pub(crate) fn reattach(
        &self,
        master: &MasterKey,
        element: Arc<dyn HardwareSigner>,
    ) -> Result<P256HybridSigningKey> {
        let plaintext = master.open(&self.sealed_ml_seed)?;
        let ml_seed: [u8; 32] = plaintext
            .strip_prefix(SEALED_ML_SEED_DOMAIN)
            .and_then(|seed| seed.try_into().ok())
            .ok_or(CryptoError::Malformed(
                "sealed device signer seed lacks its domain prefix or length",
            ))?;
        let signer = P256HybridSigningKey::enroll(
            Arc::new(ExistingKeyOnly(element)),
            self.key_alias.clone(),
            &ml_seed,
        )
        .map_err(|e| {
            tracing::error!(error = %e, "device signer: the element did not return the recorded key");
            CryptoError::Key("hardware device signing key re-binding failed")
        })?;
        if signer.verifying_key() != self.dsk_public {
            tracing::error!(
                "device signer: the element's key under the recorded alias is not the one this \
                 device published"
            );
            return Err(CryptoError::Key(
                "the secure element's key is not this device's published signing key",
            )
            .into());
        }
        tracing::debug!("device signer: hardware P-256 signing key re-attached");
        Ok(signer)
    }
}

/// Presents an element whose `enroll` only looks the key up, so re-attaching a recorded key can
/// never mint a fresh one under the same alias.
struct ExistingKeyOnly(Arc<dyn HardwareSigner>);

impl HardwareSigner for ExistingKeyOnly {
    fn enroll(&self, key_alias: String) -> std::result::Result<Vec<u8>, HardwareSignerError> {
        self.0.classical_public_key(key_alias)
    }

    fn classical_public_key(
        &self,
        key_alias: String,
    ) -> std::result::Result<Vec<u8>, HardwareSignerError> {
        self.0.classical_public_key(key_alias)
    }

    fn sign_classical(
        &self,
        key_alias: String,
        msg: Vec<u8>,
    ) -> std::result::Result<Vec<u8>, HardwareSignerError> {
        self.0.sign_classical(key_alias, msg)
    }

    fn assert_non_exportable(
        &self,
        key_alias: String,
    ) -> std::result::Result<(), HardwareSignerError> {
        self.0.assert_non_exportable(key_alias)
    }
}

#[cfg(test)]
pub(crate) mod tests;
