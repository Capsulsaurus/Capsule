//! The signed device directory: how peers learn which device public keys to trust for a
//! user, and the anti-rollback `directory_version` (SSoT: [Cryptography — Keys § Device
//! Directory]).
//!
//! Each user publishes a directory listing their devices' public halves — the hybrid **DSK**
//! (signing) and the **DEK** (key encapsulation) — master-signed (here, by the user IK). That
//! one IK signature over the canonical core is the cross-signature the design means by "both are
//! signed by the IK": it is what lets a peer trust a device key it has never seen, for
//! *encryption* ([`DeviceEntry::encapsulate`]) as much as for verification.
//!
//! `verify_asset` reads the directory to resolve the `created_by_device` of a manifest and to
//! enforce that a device's `added_at` precedes the manifest timestamp. The monotonic
//! `directory_version` lets readers refuse a rolled-back directory (a server hiding a
//! revocation).
//!
//! ## Wire presence is signature-visible here
//!
//! `DirectoryCore::signing_bytes` is the canonical CBOR of the *whole* core, so every field of
//! every [`DeviceEntry`] is inside the IK-signed bytes and field **presence** is part of the
//! signature — exactly as it is for an [asset manifest](crate::crypto::provenance::AssetManifest).
//! [`DeviceEntry::dek_public`] is therefore an **absent-key** optional
//! (`skip_serializing_if = "Option::is_none"`): a present-`null` encoding would change
//! `signing_bytes()` for every entry that has no DEK and silently break re-verification of every
//! directory signed before the field existed, including the cross-platform fixtures.
//! `added_at`/`revoked_at` predate that rule and keep their legacy present-`null` encoding — the
//! directory carries both conventions, and reconciling them would itself be a signature-visible
//! change to already-signed entries, not a cleanup.
//!
//! [Cryptography — Keys § Device Directory]: https://docs/design/cryptography/keys/#device-directory

use std::collections::BTreeMap;

use ciborium::value::Value;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::hybrid_sig::{HybridSignature, HybridSigningKey, HybridVerifyingKey};
use super::kem::{DEK_PUBLIC_LEN, encapsulate_to_public};
use super::kem_p256::{DEK_P256_PUBLIC_LEN, encapsulate_to_p256_public};
use crate::cbor::OpenMap;
use crate::crypto::CryptoError;

/// A device's published **DEK** public encapsulation key: the bytes a peer wraps a key to so
/// that only that device can open it (SSoT: [Cryptography — Keys § Device Keys]).
///
/// ## One field, both compositions, discriminated by length
///
/// Capsule ships two DEK compositions and this single field carries either:
///
/// - the software **X-Wing** DEK, `pk_M ‖ pk_X` — [`DEK_PUBLIC_LEN`] (1216) bytes;
/// - the hardware-bound **P-256 hybrid** DEK, `ek_M ‖ pk_P` — [`DEK_P256_PUBLIC_LEN`] (1249).
///
/// They are **length-disjoint by construction** (see [`P256HybridDek`](super::P256HybridDek)), so the
/// composition is recovered from the bytes themselves and needs no tag — the same way a
/// [`HybridVerifyingKey`] recovers its Ed25519-vs-P-256 classical half from the wire length, and
/// the same way [`DeviceDek::public_bytes`](super::keystore::DeviceDek::public_bytes) already
/// hands both compositions to callers as one opaque `Vec<u8>`. An explicit tag would be a second
/// signature-covered source of truth that can *disagree* with the key bytes it labels; the length
/// cannot, and a mismatched length is refused outright by [`DekPublic::encapsulate`].
///
/// Serializes as a CBOR **byte string** (major type 2) — never an array of integers — so
/// canonical encodings are byte-identical across implementations.
///
/// [Cryptography — Keys § Device Keys]: https://docs/design/cryptography/keys/#device-keys
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DekPublic(pub Vec<u8>);

impl DekPublic {
    /// The published key bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Encapsulate a fresh shared secret to this published key, returning the KEM ciphertext and
    /// the 32-byte sender-side secret. The composition is selected by length; a key of any other
    /// length is refused rather than guessed at.
    pub fn encapsulate(&self) -> Result<(Vec<u8>, [u8; 32]), CryptoError> {
        match self.0.len() {
            DEK_PUBLIC_LEN => {
                tracing::trace!(len = self.0.len(), "dek: encapsulating to an X-Wing DEK");
                encapsulate_to_public(&self.0)
            }
            DEK_P256_PUBLIC_LEN => {
                tracing::trace!(
                    len = self.0.len(),
                    "dek: encapsulating to a P-256 hybrid DEK"
                );
                encapsulate_to_p256_public(&self.0)
            }
            other => {
                tracing::warn!(
                    len = other,
                    "dek: published key matches no known composition"
                );
                Err(CryptoError::Key(
                    "published DEK length matches no known composition",
                ))
            }
        }
    }
}

impl Serialize for DekPublic {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for DekPublic {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = DekPublic;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a device DEK public key as a byte string")
            }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<DekPublic, E> {
                Ok(DekPublic(v.to_vec()))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<DekPublic, A::Error> {
                // Tolerate decoders that surface a byte string as a sequence.
                let mut out = Vec::new();
                while let Some(b) = seq.next_element()? {
                    out.push(b);
                }
                Ok(DekPublic(out))
            }
        }
        d.deserialize_bytes(V)
    }
}

/// One device's published entry. A revoked device's entry is **retained** (marked with
/// `revoked_at`), never deleted, so manifests it signed before revocation stay verifiable.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeviceEntry {
    /// Stable device id.
    pub device_id: Uuid,
    /// The device signing key's hybrid public half.
    pub dsk_public: HybridVerifyingKey,
    /// The device encryption key's public half — what a peer encapsulates to.
    ///
    /// **Wire-absent when unpublished**, never a present `null`: the entry is inside the
    /// IK-signed core, so presence is signature-visible (see the module docs). A device that has
    /// not published a DEK encodes exactly the bytes it did before this field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dek_public: Option<DekPublic>,
    /// RFC3339 time the device was added (must precede any manifest it signs).
    pub added_at: String,
    /// RFC3339 revocation time, if revoked.
    pub revoked_at: Option<String>,
    /// Keys a newer writer added that this build does not know, preserved verbatim and signed
    /// over as part of the core. Empty (and so wire-absent) on everything this build writes.
    ///
    /// A key here must never collide with a known field name: the decoder only ever places the
    /// keys it did not recognise here, and a caller building an entry by hand must do the same.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

// `Value` holds floats, so it is only `PartialEq`; see `ManifestCore` for why `Eq` stays.
impl Eq for DeviceEntry {}

impl<'de> Deserialize<'de> for DeviceEntry {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut m = OpenMap::deserialize("device entry", d)?;
        Ok(Self {
            device_id: m.take("device_id")?,
            dsk_public: m.take("dsk_public")?,
            dek_public: m.take_opt("dek_public")?,
            added_at: m.take("added_at")?,
            // Legacy present-`null`: an absent key and a `null` both read as `None`, exactly as
            // the derived decoder did, and `None` re-emits as `null`.
            revoked_at: m.take_opt("revoked_at")?,
            unknown: m.into_unknown(),
        })
    }
}

impl DeviceEntry {
    /// Encapsulate a fresh shared secret to **this published entry** — the whole point of
    /// cross-signing the DEK: a peer holding nothing but an IK-verified directory can wrap a key
    /// to a device it has never met.
    ///
    /// Refused (rather than silently mis-wrapped) when the device is **revoked** — the design
    /// forbids delivering new key wraps to a revoked device — or when it has published no DEK.
    pub fn encapsulate(&self) -> Result<(Vec<u8>, [u8; 32]), CryptoError> {
        if self.revoked_at.is_some() {
            tracing::debug!(device_id = %self.device_id, "dek: refusing to wrap to a revoked device");
            return Err(CryptoError::Key("device is revoked"));
        }
        let dek = self
            .dek_public
            .as_ref()
            .ok_or(CryptoError::Key("device publishes no DEK"))?;
        dek.encapsulate()
    }
}

/// The unsigned core of a directory — exactly the bytes the master signature covers.
///
/// **Unknown keys survive under the signature** ([Versioning — invariant 2]), here and in every
/// [`DeviceEntry`]: a directory from a newer writer that only adds a key decodes, re-emits that
/// key canonically, and so still verifies; stripping it fails verification.
///
/// [Versioning — invariant 2]: https://docs/design/versioning/#system-invariants
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DirectoryCore {
    /// Account owner.
    pub user_id: Uuid,
    /// Monotonic; +1 on every change. Readers refuse a version below their high-water mark.
    pub directory_version: u64,
    /// RFC3339 last-update time.
    pub updated_at: String,
    /// The user's devices.
    pub devices: Vec<DeviceEntry>,
    /// Keys a newer writer added that this build does not know, preserved verbatim and signed
    /// over. Empty (and so wire-absent) on everything this build writes.
    ///
    /// A key here must never collide with a known field name: the decoder only ever places the
    /// keys it did not recognise here, and a caller building a core by hand must do the same.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

// `Value` holds floats, so it is only `PartialEq`; see `ManifestCore` for why `Eq` stays.
impl Eq for DirectoryCore {}

impl<'de> Deserialize<'de> for DirectoryCore {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut m = OpenMap::deserialize("directory core", d)?;
        Ok(Self {
            user_id: m.take("user_id")?,
            directory_version: m.take("directory_version")?,
            updated_at: m.take("updated_at")?,
            devices: m.take("devices")?,
            unknown: m.into_unknown(),
        })
    }
}

/// Why a device could not be cross-signed into a directory.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DirectoryError {
    /// The directory already lists a device under this id — enrolling it again would leave two
    /// entries a peer cannot choose between.
    #[error("device {0} is already in the directory")]
    DeviceAlreadyListed(Uuid),
    /// The new entry's signing key is already published under another device id. One key under
    /// two ids would let a manifest's `created_by_device` name either.
    #[error("the signing key of device {new} is already published for device {existing}")]
    SigningKeyAlreadyListed {
        /// The id the new entry claims.
        new: Uuid,
        /// The device already publishing that key.
        existing: Uuid,
    },
    /// The new entry is marked revoked; a device is enrolled live and revoked later, never
    /// enrolled already revoked.
    #[error("device {0} is marked revoked and cannot be enrolled")]
    EntryRevoked(Uuid),
    /// `directory_version` is at its ceiling, so no later version can be signed.
    #[error("directory_version cannot advance past {0}")]
    VersionExhausted(u64),
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

    /// Add a newly enrolled device's entry — the **cross-sign** step of a cross-device add — at
    /// the next `directory_version`, stamping `updated_at`. Sign the result with
    /// [`sign`](Self::sign) to publish it.
    ///
    /// Refused, leaving `self` unchanged, when the entry's id or signing key is already listed
    /// (including on a revoked entry, which is retained so its past manifests stay verifiable),
    /// when the entry arrives already revoked, or when the version cannot advance.
    pub fn add_device(
        &mut self,
        entry: DeviceEntry,
        updated_at: String,
    ) -> Result<(), DirectoryError> {
        if entry.revoked_at.is_some() {
            return Err(DirectoryError::EntryRevoked(entry.device_id));
        }
        if self.devices.iter().any(|d| d.device_id == entry.device_id) {
            return Err(DirectoryError::DeviceAlreadyListed(entry.device_id));
        }
        if let Some(existing) = self
            .devices
            .iter()
            .find(|d| d.dsk_public == entry.dsk_public)
        {
            return Err(DirectoryError::SigningKeyAlreadyListed {
                new: entry.device_id,
                existing: existing.device_id,
            });
        }
        let next = self
            .directory_version
            .checked_add(1)
            .ok_or(DirectoryError::VersionExhausted(self.directory_version))?;
        tracing::info!(
            device_id = %entry.device_id,
            directory_version = next,
            "directory: cross-signing a device in"
        );
        self.directory_version = next;
        self.updated_at = updated_at;
        self.devices.push(entry);
        Ok(())
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
mod tests;
