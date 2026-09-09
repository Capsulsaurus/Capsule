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
//! [`DirectoryCore::signing_bytes`] is the canonical CBOR of the *whole* core, so every field of
//! every [`DeviceEntry`] is inside the IK-signed bytes and field **presence** is part of the
//! signature — exactly as it is for an [asset manifest](crate::crypto::provenance::manifest).
//! [`DeviceEntry::dek_public`] is therefore an **absent-key** optional
//! (`skip_serializing_if = "Option::is_none"`): a present-`null` encoding would change
//! `signing_bytes()` for every entry that has no DEK and silently break re-verification of every
//! directory signed before the field existed, including the cross-platform fixtures.
//! `added_at`/`revoked_at` predate that rule and keep their legacy present-`null` encoding — the
//! directory carries both conventions, and reconciling them would itself be a signature-visible
//! change to already-signed entries, not a cleanup.
//!
//! [Cryptography — Keys § Device Directory]: https://docs/design/cryptography/keys/#device-directory

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::hybrid_sig::{HybridSignature, HybridSigningKey, HybridVerifyingKey};
use super::kem::{DEK_PUBLIC_LEN, encapsulate_to_public};
use super::kem_p256::{DEK_P256_PUBLIC_LEN, encapsulate_to_p256_public};
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
    use std::sync::Arc;

    use super::*;
    use crate::crypto::keys::kem::DekKeypair;
    use crate::crypto::keys::kem_p256::{MockP256KeyAgreement, P256HybridDek};

    fn entry(device: &HybridSigningKey, dek_public: Option<DekPublic>) -> DeviceEntry {
        DeviceEntry {
            device_id: Uuid::from_u128(0xD1),
            dsk_public: device.verifying_key(),
            dek_public,
            added_at: "2026-05-30T00:00:00Z".into(),
            revoked_at: None,
        }
    }

    fn dir(version: u64, ik: &HybridSigningKey, device: &HybridSigningKey) -> DeviceDirectory {
        core(version, device).sign(ik)
    }

    fn core(version: u64, device: &HybridSigningKey) -> DirectoryCore {
        DirectoryCore {
            user_id: Uuid::from_u128(1),
            directory_version: version,
            updated_at: "2026-05-31T00:00:00Z".into(),
            devices: vec![entry(device, None)],
        }
    }

    fn ik() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
    }
    fn dev() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32])
    }

    /// The CBOR map for the sole device entry inside a core's canonical signing bytes.
    fn entry_keys(core: &DirectoryCore) -> Vec<String> {
        let value: ciborium::Value = crate::cbor::from_slice(&core.signing_bytes()).unwrap();
        entry_map(&value)
            .iter()
            .filter_map(|(k, _)| k.as_text().map(str::to_owned))
            .collect()
    }

    fn entry_map(value: &ciborium::Value) -> Vec<(ciborium::Value, ciborium::Value)> {
        let map = value
            .as_map()
            .expect("directory core encodes as a CBOR map");
        let devices = map
            .iter()
            .find(|(k, _)| k.as_text() == Some("devices"))
            .map(|(_, v)| v)
            .expect("devices key");
        devices.as_array().expect("devices is an array")[0]
            .as_map()
            .expect("a device entry encodes as a CBOR map")
            .clone()
    }

    #[test]
    fn sign_verify_and_lookup() {
        let (ik, dev) = (ik(), dev());
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
        let (ik, dev) = (ik(), dev());
        let mut d = dir(1, &ik, &dev);
        // Swap in a different device key without re-signing.
        d.core.devices[0].dsk_public =
            HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]).verifying_key();
        assert!(!d.verify(&ik.verifying_key()));
    }

    #[test]
    fn serializes_canonically() {
        let (ik, dev) = (ik(), dev());
        let d = dir(7, &ik, &dev);
        let bytes = crate::cbor::to_canonical_vec(&d).unwrap();
        let back: DeviceDirectory = crate::cbor::from_slice(&bytes).unwrap();
        assert_eq!(back, d);
        assert!(back.verify(&ik.verifying_key()));
    }

    // ── `dek_public` wire presence (slice `S-A11`) ──────────────────────────────

    /// The wire-presence contract for `dek_public`: with no DEK published it is an **ABSENT**
    /// map key, so directories signed before the field existed re-verify byte-identically.
    /// Mirrors `manifest.rs`'s `default_new_fields_are_wire_absent`.
    #[test]
    fn default_dek_public_is_wire_absent() {
        let keys = entry_keys(&core(1, &dev()));
        assert!(
            !keys.iter().any(|k| k == "dek_public"),
            "an unpublished dek_public must be a wire-absent key, not a present null: {keys:?}"
        );
        // The pre-existing options keep their legacy present-null encoding — reconciling them
        // would itself be a signature-visible change to entries already signed.
        assert!(keys.iter().any(|k| k == "added_at"));
        assert!(keys.iter().any(|k| k == "revoked_at"));
    }

    /// **The regression this slice must not cause.** `LegacyDirectoryCore` is the *pre-`S-A11`*
    /// encoder verbatim — the struct that produced every directory signed before `dek_public`
    /// existed. Bytes it produced and the IK signed must (a) decode under today's type,
    /// (b) re-derive **byte-identical** signing bytes, and (c) still verify.
    #[test]
    fn a_directory_signed_before_dek_public_existed_still_verifies() {
        #[derive(Serialize)]
        struct LegacyDeviceEntry {
            device_id: Uuid,
            dsk_public: HybridVerifyingKey,
            added_at: String,
            revoked_at: Option<String>,
        }
        #[derive(Serialize)]
        struct LegacyDirectoryCore {
            user_id: Uuid,
            directory_version: u64,
            updated_at: String,
            devices: Vec<LegacyDeviceEntry>,
        }

        let (ik, dev) = (ik(), dev());
        let legacy = LegacyDirectoryCore {
            user_id: Uuid::from_u128(1),
            directory_version: 7,
            updated_at: "2026-05-31T00:00:00Z".into(),
            devices: vec![LegacyDeviceEntry {
                device_id: Uuid::from_u128(0xD1),
                dsk_public: dev.verifying_key(),
                added_at: "2026-05-30T00:00:00Z".into(),
                revoked_at: None,
            }],
        };
        let legacy_bytes = crate::cbor::to_canonical_vec(&legacy).unwrap();
        let legacy_signature = ik.sign(&legacy_bytes);

        // Decode the pre-change bytes with today's type…
        let decoded: DirectoryCore = crate::cbor::from_slice(&legacy_bytes).unwrap();
        assert_eq!(
            decoded.devices[0].dek_public, None,
            "an absent key must decode as None, not fail"
        );
        // …and re-encoding must reproduce the signed bytes exactly.
        assert_eq!(
            decoded.signing_bytes(),
            legacy_bytes,
            "adding dek_public changed the signing bytes of a pre-existing entry"
        );
        // Which is what makes the pre-change signature still verify.
        let directory = DeviceDirectory {
            core: decoded,
            signature: legacy_signature,
        };
        assert!(
            directory.verify(&ik.verifying_key()),
            "a directory signed before dek_public existed must still verify"
        );
        // And today's encoder produces those same bytes for the same logical entry.
        assert_eq!(core(7, &dev).signing_bytes(), legacy_bytes);
    }

    /// A published DEK is a CBOR **byte string**, is present on the wire, and is covered by the
    /// IK signature — tampering with it invalidates the directory.
    #[test]
    fn a_published_dek_is_a_signed_byte_string() {
        let (ik, dev) = (ik(), dev());
        let dek = DekKeypair::generate();
        let mut c = core(1, &dev);
        c.devices[0].dek_public = Some(DekPublic(dek.public_bytes()));

        let keys = entry_keys(&c);
        assert!(
            keys.iter().any(|k| k == "dek_public"),
            "a published dek_public must be present on the wire: {keys:?}"
        );
        let value: ciborium::Value = crate::cbor::from_slice(&c.signing_bytes()).unwrap();
        let published = entry_map(&value)
            .into_iter()
            .find(|(k, _)| k.as_text() == Some("dek_public"))
            .map(|(_, v)| v)
            .expect("dek_public present");
        assert!(
            published.is_bytes(),
            "dek_public must encode as a CBOR byte string, not an array of integers"
        );

        let mut d = c.sign(&ik);
        assert!(d.verify(&ik.verifying_key()));

        // Round-trips through canonical CBOR unchanged.
        let back: DeviceDirectory =
            crate::cbor::from_slice(&crate::cbor::to_canonical_vec(&d).unwrap()).unwrap();
        assert_eq!(back, d);
        assert!(back.verify(&ik.verifying_key()));

        // The signature covers it: swapping in another device's DEK breaks verification.
        d.core.devices[0].dek_public = Some(DekPublic(DekKeypair::generate().public_bytes()));
        assert!(
            !d.verify(&ik.verifying_key()),
            "a substituted dek_public must break the IK signature"
        );
    }

    // ── Encapsulating to a published entry (the point of publishing it) ─────────

    /// **The slice's acceptance.** A peer holding only the published entry wraps a secret to the
    /// device, and the device — and only the device — recovers it.
    #[test]
    fn a_peer_encapsulates_to_a_published_software_dek() {
        let (ik, dev) = (ik(), dev());
        let dek = DekKeypair::generate();
        let mut c = core(1, &dev);
        c.devices[0].dek_public = Some(DekPublic(dek.public_bytes()));
        let directory = c.sign(&ik);
        assert!(directory.verify(&ik.verifying_key()));

        let published = directory.device(&Uuid::from_u128(0xD1)).unwrap();
        let (ciphertext, sent) = published.encapsulate().unwrap();
        assert_eq!(
            dek.decapsulate(&ciphertext).unwrap(),
            sent,
            "the device must recover the secret a peer wrapped to its published entry"
        );
        // A different device's DEK does not recover it.
        assert_ne!(
            DekKeypair::generate().decapsulate(&ciphertext).ok(),
            Some(sent)
        );
    }

    /// The hardware-bound composition rides the same field: the entry carries `ek_M ‖ pk_P` and
    /// the length alone tells a peer which KEM to run — no tag, no out-of-band hint.
    #[test]
    fn a_peer_encapsulates_to_a_published_hardware_dek() {
        let element = Arc::new(MockP256KeyAgreement::new([7u8; 32], false));
        let dek = P256HybridDek::enroll(element, "device-dek".into(), &[3u8; 32]).unwrap();
        let published = entry(&dev(), Some(DekPublic(dek.public_bytes())));

        assert_eq!(
            published.dek_public.as_ref().unwrap().as_bytes().len(),
            DEK_P256_PUBLIC_LEN,
            "the P-256 hybrid half is length-disjoint from X-Wing"
        );
        let (ciphertext, sent) = published.encapsulate().unwrap();
        assert_eq!(dek.decapsulate(&ciphertext).unwrap(), sent);
    }

    /// The two compositions never alias: a peer that runs the wrong KEM produces a ciphertext the
    /// device rejects, so length dispatch is what makes a single field sound.
    #[test]
    fn the_two_compositions_are_length_disjoint() {
        let software = DekKeypair::generate();
        let element = Arc::new(MockP256KeyAgreement::new([9u8; 32], false));
        let hardware = P256HybridDek::enroll(element, "device-dek".into(), &[5u8; 32]).unwrap();

        assert_eq!(software.public_bytes().len(), DEK_PUBLIC_LEN);
        assert_eq!(hardware.public_bytes().len(), DEK_P256_PUBLIC_LEN);
        assert_ne!(DEK_PUBLIC_LEN, DEK_P256_PUBLIC_LEN);

        // The ciphertext a peer produces for one is refused by the other.
        let (hardware_ct, _) = entry(&dev(), Some(DekPublic(hardware.public_bytes())))
            .encapsulate()
            .unwrap();
        assert!(software.decapsulate(&hardware_ct).is_err());
    }

    #[test]
    fn encapsulating_to_a_device_with_no_published_dek_is_refused() {
        assert!(matches!(
            entry(&dev(), None).encapsulate(),
            Err(CryptoError::Key("device publishes no DEK"))
        ));
    }

    /// A revoked device must not receive new key wraps, even though its entry is retained so its
    /// past manifests stay verifiable.
    #[test]
    fn encapsulating_to_a_revoked_device_is_refused() {
        let mut e = entry(
            &dev(),
            Some(DekPublic(DekKeypair::generate().public_bytes())),
        );
        e.revoked_at = Some("2026-06-01T00:00:00Z".into());
        assert!(matches!(
            e.encapsulate(),
            Err(CryptoError::Key("device is revoked"))
        ));
    }

    /// A published key of an unknown length is refused rather than guessed at — the failure mode
    /// a tag would otherwise have to be trusted to prevent.
    #[test]
    fn a_dek_of_unknown_length_is_refused() {
        for bytes in [vec![], vec![0u8; 32], vec![0u8; DEK_PUBLIC_LEN - 1]] {
            assert!(matches!(
                entry(&dev(), Some(DekPublic(bytes))).encapsulate(),
                Err(CryptoError::Key(
                    "published DEK length matches no known composition"
                ))
            ));
        }
    }

    /// A well-formed length with garbage bytes fails in the KEM, not with a wrong secret.
    #[test]
    fn a_malformed_dek_of_the_right_length_fails_in_the_kem() {
        assert!(
            entry(&dev(), Some(DekPublic(vec![0xFF; DEK_P256_PUBLIC_LEN])))
                .encapsulate()
                .is_err()
        );
    }
}
