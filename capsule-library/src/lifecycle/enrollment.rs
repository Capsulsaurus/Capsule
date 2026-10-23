//! The client half of cross-device add (SSoT: [Device Enrollment § Cross-Device Add]): the
//! bundles this device advertises and the cross-sign that admits the new device. The wire form —
//! the payloads, their codec and the safety code — is `capsule_core::enrollment`, re-exported
//! here.
//!
//! [Device Enrollment § Cross-Device Add]: https://docs/design/device-enrollment/#cross-device-add

use capsule_core::crypto::keys::{DekPublic, DeviceDirectory, DeviceEntry};
use capsule_core::crypto::rng;
pub use capsule_core::enrollment::*;

use super::{Result, Workspace, now_rfc3339};

impl Workspace {
    /// What this device advertises as the **initiator** of a cross-device add, with a nonce
    /// freshly drawn by this call. Call it once per ceremony and keep the result: it is both what
    /// A sends and what A computes its safety code over.
    pub fn enrollment_initiator_keys(&self) -> InitiatorKeys {
        InitiatorKeys {
            user_id: self.account.user_id,
            user_ik_public: self.account.user_ik.verifying_key(),
            device_id: self.account.device.device_id,
            dsk_public: self.device_signer.verifying_key(),
            nonce: rng::random_array(),
        }
    }

    /// What this device advertises as the **enrollee** of a cross-device add: the signing key it
    /// actually signs with (a hardware signer's, when one was supplied), its DEK, and a nonce
    /// freshly drawn by this call. Call it once per ceremony and keep the result: B commits to,
    /// reveals, and computes its safety code over this one bundle.
    pub fn enrollment_enrollee_keys(&self) -> EnrolleeKeys {
        EnrolleeKeys {
            device_id: self.account.device.device_id,
            dsk_public: self.device_signer.verifying_key(),
            dek_public: DekPublic(self.account.device.dek.public_bytes()),
            nonce: rng::random_array(),
        }
    }

    /// **Cross-sign** a new device into this account's directory: append `entry` at the next
    /// `directory_version`, re-sign under the user IK, adopt it as this workspace's directory,
    /// and return it for publication and for relaying to the new device.
    ///
    /// Call this only after the safety codes matched. Refused with
    /// [`LifecycleError::Directory`](super::LifecycleError::Directory), leaving the directory
    /// unchanged, when the entry's id or signing key is already listed or the entry is revoked.
    ///
    /// The re-signed directory is persisted beside the account before it is adopted, so
    /// [`open`](Self::open) restores it and the next directory this device signs builds on it;
    /// a failed write is a [`LifecycleError::Io`](super::LifecycleError::Io) and also leaves the
    /// directory unchanged. The caller still publishes the returned directory.
    #[tracing::instrument(skip_all, fields(device_id = %entry.device_id))]
    pub fn cross_sign_device(&mut self, entry: DeviceEntry) -> Result<DeviceDirectory> {
        let mut core = self.directory.core.clone();
        core.add_device(entry, now_rfc3339()).inspect_err(|e| {
            tracing::warn!(error = %e, "enrollment: cross-sign refused");
        })?;
        self.adopt_directory(core.sign(&self.account.user_ik))?;
        tracing::info!(
            directory_version = self.directory.core.directory_version,
            devices = self.directory.core.devices.len(),
            "enrollment: device cross-signed into the directory"
        );
        Ok(self.directory.clone())
    }
}

#[cfg(test)]
mod tests;
