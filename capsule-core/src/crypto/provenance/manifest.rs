//! The signed asset manifest and derivative manifest (SSoT: [Cryptography — Provenance]).
//!
//! A manifest carries **two** hybrid signatures over the same canonical core bytes:
//! `device_sig` (provenance — which device produced it) and `write_sig` (authorization —
//! the album's per-epoch write-tier key). Both must verify at [`verify_asset`]. The core
//! excludes the signatures, so signing bytes are unambiguous and downgrade-resistant
//! (both sigs cover `crypto_suite_id`, `protocol_version`, and `prior_provenance_hash`).
//!
//! [`verify_asset`]: crate::crypto::verify_asset
//! [Cryptography — Provenance]: https://docs/design/cryptography/provenance/

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::action::{Action, DerivativeRole};
use crate::cbor;
use crate::crypto::CryptoError;
use crate::crypto::hash::Hash32;
use crate::crypto::keys::{AmkVersion, HybridSignature, Signer};

/// Current asset-manifest schema string.
pub const ASSET_MANIFEST_VERSION: &str = "asset-manifest/v1";
/// Current derivative-manifest schema string.
pub const DERIVATIVE_MANIFEST_VERSION: &str = "derivative-manifest/v1";

/// How a reader obtains the asset's file key (closed enum; SSoT:
/// [Cryptography — Provenance](https://docs/design/cryptography/provenance/)).
///
/// Wire-presence rule: `Derived` is the default and encodes as an **absent** map key in
/// canonical CBOR (`skip_serializing_if`), so manifests signed before this field existed
/// re-verify byte-identically. Emitting `key_mode: "derived"` explicitly would change the
/// signed bytes and break verification.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KeyMode {
    /// The file key is recomputed from the AMK (`asset-file/v1`); nothing is stored.
    #[default]
    Derived,
    /// The file key was chosen externally (an adopted web-upload drop) and is carried in
    /// `wrapped_file_key`, sealed under the AMK (`asset-keywrap/v1`).
    Wrapped,
}

impl KeyMode {
    /// Serde helper: the default (`derived`) is wire-absent.
    pub fn is_derived(&self) -> bool {
        matches!(self, Self::Derived)
    }
}

/// An externally-chosen file key sealed under the AMK: `wrap_nonce || AES-256-GCM(K) || tag`
/// (the `asset-keywrap/v1` derivation — see
/// [Encryption — Asset Key Derivation](https://docs/design/cryptography/encryption/)).
/// Length is fixed by `crypto_suite_id`; the bytes are ciphertext, opaque to the server.
///
/// Serializes as a CBOR **byte string** (major type 2), like [`Hash32`] — never as an
/// array of integers — so canonical encodings are byte-identical across implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedFileKey(pub Vec<u8>);

impl Serialize for WrappedFileKey {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for WrappedFileKey {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = WrappedFileKey;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an AMK-wrapped file key as a byte string")
            }
            fn visit_bytes<E: serde::de::Error>(self, v: &[u8]) -> Result<WrappedFileKey, E> {
                Ok(WrappedFileKey(v.to_vec()))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<WrappedFileKey, A::Error> {
                // Tolerate decoders that surface a byte string as a sequence.
                let mut out = Vec::new();
                while let Some(b) = seq.next_element()? {
                    out.push(b);
                }
                Ok(WrappedFileKey(out))
            }
        }
        d.deserialize_bytes(V)
    }
}

/// The signed core of an asset manifest — every field the two signatures cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestCore {
    /// Schema version string (`asset-manifest/v1`).
    pub version: String,
    /// The primitive bundle this manifest was produced under.
    pub crypto_suite_id: u16,
    /// Date-based wire protocol version; matches the album pin.
    pub protocol_version: String,
    /// The asset's file id.
    pub file_id: Uuid,
    /// The album the asset belongs to.
    pub album_id: Uuid,
    /// The AMK epoch (and write-tier key) this manifest is authorized under.
    pub amk_version: AmkVersion,
    /// Content-address digest over the ciphertext.
    pub ciphertext_hash: Hash32,
    /// Total plaintext byte length.
    pub plaintext_size: u64,
    /// Plaintext bytes per STREAM chunk.
    pub chunk_size: u32,
    /// STREAM nonce prefix (random per file).
    pub nonce_prefix: [u8; 7],
    /// How the file key is obtained: `derived` (default; wire-absent) or `wrapped` (an
    /// adopted web-upload drop). Closed enum — see [`KeyMode`] for the wire-presence rule.
    #[serde(default, skip_serializing_if = "KeyMode::is_derived")]
    pub key_mode: KeyMode,
    /// The AMK-sealed file key; present iff `key_mode = wrapped`, wire-absent otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_file_key: Option<WrappedFileKey>,
    /// Content address of the asset's encrypted metadata blob. Present on
    /// `create | replace | metadata-update`; wire-absent (key omitted, never null) on
    /// `delete | derivative-* | trash-restore`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_blob_hash: Option<Hash32>,
    /// User who produced the asset.
    pub created_by_user: Uuid,
    /// Device that produced the asset (resolved in the device directory).
    pub created_by_device: Uuid,
    /// Producing client version string.
    pub client_version: String,
    /// Self-asserted capture/write time (RFC3339). Audit-only; never load-bearing.
    pub timestamp: String,
    /// The lifecycle action.
    pub action: Action,
    /// SHA-256 of the previous manifest in this asset's chain; null iff `action = create`.
    pub prior_provenance_hash: Option<Hash32>,
    /// Server-visible retention deadline (RFC3339); set only for `action = delete`.
    pub retention_until: Option<String>,
}

/// A signed asset manifest: a [`ManifestCore`] plus its two hybrid signatures.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetManifest {
    /// The signed core.
    pub core: ManifestCore,
    /// Hybrid signature by the uploading device's DSK (provenance).
    pub device_sig: HybridSignature,
    /// Hybrid signature under the epoch write-tier key (authorization).
    pub write_sig: HybridSignature,
}

impl ManifestCore {
    /// The canonical bytes both signatures cover.
    pub fn signing_bytes(&self) -> Vec<u8> {
        cbor::to_canonical_vec(self).expect("manifest core serializes")
    }

    /// Sign this core with the device DSK and the epoch write-tier key. Fallible because the
    /// device signer may be hardware-backed (the write-tier key is always software).
    pub fn sign(
        self,
        device: &dyn Signer,
        write_tier: &dyn Signer,
    ) -> Result<AssetManifest, CryptoError> {
        let bytes = self.signing_bytes();
        let device_sig = device.sign(&bytes)?;
        let write_sig = write_tier.sign(&bytes)?;
        Ok(AssetManifest {
            core: self,
            device_sig,
            write_sig,
        })
    }
}

impl AssetManifest {
    /// The canonical bytes both signatures cover.
    pub fn signing_bytes(&self) -> Vec<u8> {
        self.core.signing_bytes()
    }

    /// Structural well-formedness independent of any key:
    /// - `prior_provenance_hash` is null **iff** the action is `create`;
    /// - `retention_until` is set only for `delete`;
    /// - `wrapped_file_key` is present **iff** `key_mode = wrapped`.
    ///
    /// The first two are mirrored by the server envelope; the wrapped-key rule is the
    /// signature-visible presence rule for an adopted web-upload drop, enforced here at
    /// `verify_asset`. (The `metadata_blob_hash` presence-by-action rule lands with the
    /// metadata↔manifest binding in S-A3, once the `Workspace` populates the field per the
    /// sealing order — it cannot be enforced before the field is populated.)
    pub fn structural_ok(&self) -> bool {
        let core = &self.core;
        let prior_rule = core.prior_provenance_hash.is_none() == core.action.is_create();
        let retention_rule = core.retention_until.is_none() || core.action == Action::Delete;
        let wrapped_rule = core.wrapped_file_key.is_some() == (core.key_mode == KeyMode::Wrapped);
        prior_rule && retention_rule && wrapped_rule
    }
}

/// The signed core of a derivative manifest (thumbnail / preview / embedding).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivativeCore {
    /// Schema version string (`derivative-manifest/v1`).
    pub version: String,
    /// Primitive bundle.
    pub crypto_suite_id: u16,
    /// Date-based wire protocol version; matches the album pin. Wire-absent only on
    /// pre-binding fixtures; REQUIRED on every real write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<String>,
    /// The AMK epoch whose write-tier key produced `write_sig` — the verifier needs it to
    /// select the verification key. Wire-absent only on pre-binding fixtures; REQUIRED on
    /// every real write (a derivative without it cannot be authorization-verified).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amk_version: Option<AmkVersion>,
    /// The asset this derivative is generated from.
    pub source_asset_id: Uuid,
    /// Which kind of derivative.
    pub role: DerivativeRole,
    /// MIME/format string, e.g. `image/avif` or `embedding/mobileclip-b`.
    pub format: String,
    /// Content-address digest over the derivative ciphertext.
    pub ciphertext_hash: Hash32,
    /// Device that generated the derivative.
    pub generated_by_device: Uuid,
    /// Generating client version.
    pub generated_by_client: String,
    /// Model id (embeddings only).
    pub model_id: Option<String>,
    /// Model version (embeddings only).
    pub model_version: Option<String>,
    /// RFC3339 generation time.
    pub generated_at: String,
    /// Chain link per `(source_asset_id, role)`; null for the first of that role.
    pub prior_provenance_hash: Option<Hash32>,
}

/// A signed derivative manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivativeManifest {
    /// The signed core.
    pub core: DerivativeCore,
    /// Hybrid device signature.
    pub device_sig: HybridSignature,
    /// Hybrid write-tier signature.
    pub write_sig: HybridSignature,
}

impl DerivativeCore {
    /// The canonical bytes both signatures cover.
    pub fn signing_bytes(&self) -> Vec<u8> {
        cbor::to_canonical_vec(self).expect("derivative core serializes")
    }

    /// Sign with the device DSK and epoch write-tier key. Fallible: the device signer may be
    /// hardware-backed.
    pub fn sign(
        self,
        device: &dyn Signer,
        write_tier: &dyn Signer,
    ) -> Result<DerivativeManifest, CryptoError> {
        let bytes = self.signing_bytes();
        Ok(DerivativeManifest {
            device_sig: device.sign(&bytes)?,
            write_sig: write_tier.sign(&bytes)?,
            core: self,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keys::HybridSigningKey;
    use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};

    fn core(action: Action, prior: Option<Hash32>) -> ManifestCore {
        ManifestCore {
            version: ASSET_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            file_id: Uuid::from_u128(0xF11E),
            album_id: Uuid::from_u128(0xA1),
            amk_version: AmkVersion(1),
            ciphertext_hash: Hash32([0xCC; 32]),
            plaintext_size: 1024,
            chunk_size: 65_520,
            nonce_prefix: [1, 2, 3, 4, 5, 6, 7],
            key_mode: KeyMode::Derived,
            wrapped_file_key: None,
            metadata_blob_hash: None,
            created_by_user: Uuid::from_u128(0x05E2),
            created_by_device: Uuid::from_u128(0xD1),
            client_version: "capsule-cli/0.1.0".into(),
            timestamp: "2026-05-31T12:00:00Z".into(),
            action,
            prior_provenance_hash: prior,
            retention_until: None,
        }
    }

    #[test]
    fn sign_produces_two_verifiable_signatures() {
        let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let write = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let m = core(Action::Create, None).sign(&device, &write).unwrap();

        let bytes = m.signing_bytes();
        assert!(device.verifying_key().verify(&bytes, &m.device_sig));
        assert!(write.verifying_key().verify(&bytes, &m.write_sig));
    }

    #[test]
    fn signing_bytes_are_canonical_and_stable() {
        let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let write = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let m = core(Action::Create, None).sign(&device, &write).unwrap();
        // The core round-trips through canonical CBOR unchanged, and the full manifest too.
        let back: AssetManifest = cbor::from_slice(&cbor::to_canonical_vec(&m).unwrap()).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.signing_bytes(), m.signing_bytes());
    }

    /// A wrapped-file-key length (`structural_ok` is length-agnostic; the real length is
    /// pinned by `crate::crypto::encryption::WRAPPED_FILE_KEY_LEN`).
    const WRAPPED_LEN: usize = crate::crypto::encryption::WRAPPED_FILE_KEY_LEN;

    /// A structurally-valid core for `action` (correct prior placement), so tests can perturb
    /// exactly one field.
    fn valid_core(action: Action) -> ManifestCore {
        let prior = (!action.is_create()).then(|| Hash32([1; 32]));
        core(action, prior)
    }

    #[test]
    fn structural_rules_prior_hash_and_retention() {
        // create + null prior: ok.
        assert!(
            valid_core(Action::Create)
                .sign(&dev(), &wt())
                .unwrap()
                .structural_ok()
        );
        // create + non-null prior: violation.
        let mut c = valid_core(Action::Create);
        c.prior_provenance_hash = Some(Hash32([1; 32]));
        assert!(!c.sign(&dev(), &wt()).unwrap().structural_ok());
        // non-create + null prior: violation.
        let mut c = valid_core(Action::Replace);
        c.prior_provenance_hash = None;
        assert!(!c.sign(&dev(), &wt()).unwrap().structural_ok());
        // non-create + non-null prior: ok.
        assert!(
            valid_core(Action::Replace)
                .sign(&dev(), &wt())
                .unwrap()
                .structural_ok()
        );

        // retention only on delete.
        let mut c = valid_core(Action::MetadataUpdate);
        c.retention_until = Some("2026-07-01T00:00:00Z".into());
        assert!(!c.sign(&dev(), &wt()).unwrap().structural_ok());
        let mut d = valid_core(Action::Delete);
        d.retention_until = Some("2026-07-01T00:00:00Z".into());
        assert!(d.sign(&dev(), &wt()).unwrap().structural_ok());
    }

    #[test]
    fn structural_rule_wrapped_file_key_present_iff_wrapped() {
        // derived (default) + absent: ok.
        assert!(
            valid_core(Action::Create)
                .sign(&dev(), &wt())
                .unwrap()
                .structural_ok()
        );
        // wrapped + present: ok.
        let mut c = valid_core(Action::Create);
        c.key_mode = KeyMode::Wrapped;
        c.wrapped_file_key = Some(WrappedFileKey(vec![0xAB; WRAPPED_LEN]));
        assert!(c.sign(&dev(), &wt()).unwrap().structural_ok());
        // wrapped + absent: violation.
        let mut c = valid_core(Action::Create);
        c.key_mode = KeyMode::Wrapped;
        c.wrapped_file_key = None;
        assert!(!c.sign(&dev(), &wt()).unwrap().structural_ok());
        // derived + present: violation.
        let mut c = valid_core(Action::Create);
        c.key_mode = KeyMode::Derived;
        c.wrapped_file_key = Some(WrappedFileKey(vec![0xAB; WRAPPED_LEN]));
        assert!(!c.sign(&dev(), &wt()).unwrap().structural_ok());
    }

    #[test]
    fn derivative_chain_is_independent() {
        let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let write = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let dm = DerivativeCore {
            version: DERIVATIVE_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: Some(PROTOCOL_VERSION.into()),
            amk_version: Some(AmkVersion(1)),
            source_asset_id: Uuid::from_u128(0xF11E),
            role: DerivativeRole::Thumbnail,
            format: "image/avif".into(),
            ciphertext_hash: Hash32([0xAB; 32]),
            generated_by_device: Uuid::from_u128(0xD1),
            generated_by_client: "capsule-cli/0.1.0".into(),
            model_id: None,
            model_version: None,
            generated_at: "2026-05-31T12:00:00Z".into(),
            prior_provenance_hash: None,
        }
        .sign(&device, &write)
        .unwrap();
        assert!(
            write
                .verifying_key()
                .verify(&dm.core.signing_bytes(), &dm.write_sig)
        );
    }

    fn dev() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
    }
    fn wt() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32])
    }

    /// The wire-presence contract for the fields added within `asset-manifest/v1`
    /// (`key_mode`, `wrapped_file_key`, `metadata_blob_hash`): at their defaults they are
    /// ABSENT map keys, so manifests signed before the fields existed re-verify
    /// byte-identically. See the wire-presence rules in
    /// <https://docs/design/cryptography/provenance/>.
    #[test]
    fn default_new_fields_are_wire_absent() {
        let bytes = core(Action::Create, None).signing_bytes();
        let value: ciborium::Value = cbor::from_slice(&bytes).unwrap();
        let map = value.as_map().expect("manifest core encodes as a CBOR map");
        let keys: Vec<&str> = map.iter().filter_map(|(k, _)| k.as_text()).collect();
        assert!(
            !keys.contains(&"key_mode"),
            "derived key_mode must be wire-absent"
        );
        assert!(!keys.contains(&"wrapped_file_key"));
        assert!(!keys.contains(&"metadata_blob_hash"));
        // The pre-existing options keep their legacy present-null encoding.
        assert!(keys.contains(&"prior_provenance_hash"));
        assert!(keys.contains(&"retention_until"));
    }

    #[test]
    fn wrapped_fields_round_trip_as_byte_strings() {
        let mut c = core(Action::Create, None);
        c.key_mode = KeyMode::Wrapped;
        c.wrapped_file_key = Some(WrappedFileKey(vec![0xAB; 60]));
        c.metadata_blob_hash = Some(Hash32([0x4D; 32]));
        let m = c.sign(&dev(), &wt()).unwrap();
        let back: AssetManifest = cbor::from_slice(&cbor::to_canonical_vec(&m).unwrap()).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.signing_bytes(), m.signing_bytes());

        let value: ciborium::Value = cbor::from_slice(&m.signing_bytes()).unwrap();
        let map = value.as_map().unwrap();
        let wrapped = map
            .iter()
            .find(|(k, _)| k.as_text() == Some("wrapped_file_key"))
            .map(|(_, v)| v)
            .expect("wrapped_file_key present when key_mode = wrapped");
        assert!(
            wrapped.is_bytes(),
            "wrapped_file_key must encode as a CBOR byte string"
        );
    }
}
