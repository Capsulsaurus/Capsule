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
use crate::crypto::hash::Hash32;
use crate::crypto::keys::{AmkVersion, HybridSignature, HybridSigningKey};

/// Current asset-manifest schema string.
pub const ASSET_MANIFEST_VERSION: &str = "asset-manifest/v1";
/// Current derivative-manifest schema string.
pub const DERIVATIVE_MANIFEST_VERSION: &str = "derivative-manifest/v1";

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

    /// Sign this core with the device DSK and the epoch write-tier key.
    pub fn sign(self, device: &HybridSigningKey, write_tier: &HybridSigningKey) -> AssetManifest {
        let bytes = self.signing_bytes();
        let device_sig = device.sign(&bytes);
        let write_sig = write_tier.sign(&bytes);
        AssetManifest {
            core: self,
            device_sig,
            write_sig,
        }
    }
}

impl AssetManifest {
    /// The canonical bytes both signatures cover.
    pub fn signing_bytes(&self) -> Vec<u8> {
        self.core.signing_bytes()
    }

    /// Structural well-formedness independent of any key:
    /// - `prior_provenance_hash` is null **iff** the action is `create`;
    /// - `retention_until` is set only for `delete`.
    ///
    /// These are enforced both here (client `verify_asset`) and by the server envelope.
    pub fn structural_ok(&self) -> bool {
        let prior_rule = self.core.prior_provenance_hash.is_none() == self.core.action.is_create();
        let retention_rule =
            self.core.retention_until.is_none() || self.core.action == Action::Delete;
        prior_rule && retention_rule
    }
}

/// The signed core of a derivative manifest (thumbnail / preview / embedding).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivativeCore {
    /// Schema version string (`derivative-manifest/v1`).
    pub version: String,
    /// Primitive bundle.
    pub crypto_suite_id: u16,
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

    /// Sign with the device DSK and epoch write-tier key.
    pub fn sign(
        self,
        device: &HybridSigningKey,
        write_tier: &HybridSigningKey,
    ) -> DerivativeManifest {
        let bytes = self.signing_bytes();
        DerivativeManifest {
            device_sig: device.sign(&bytes),
            write_sig: write_tier.sign(&bytes),
            core: self,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let m = core(Action::Create, None).sign(&device, &write);

        let bytes = m.signing_bytes();
        assert!(device.verifying_key().verify(&bytes, &m.device_sig));
        assert!(write.verifying_key().verify(&bytes, &m.write_sig));
    }

    #[test]
    fn signing_bytes_are_canonical_and_stable() {
        let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let write = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let m = core(Action::Create, None).sign(&device, &write);
        // The core round-trips through canonical CBOR unchanged, and the full manifest too.
        let back: AssetManifest = cbor::from_slice(&cbor::to_canonical_vec(&m).unwrap()).unwrap();
        assert_eq!(back, m);
        assert_eq!(back.signing_bytes(), m.signing_bytes());
    }

    #[test]
    fn structural_rules_prior_hash_and_retention() {
        // create + null prior: ok.
        assert!(
            core(Action::Create, None)
                .sign(&dev(), &wt())
                .structural_ok()
        );
        // create + non-null prior: violation.
        assert!(
            !core(Action::Create, Some(Hash32([1; 32])))
                .sign(&dev(), &wt())
                .structural_ok()
        );
        // non-create + null prior: violation.
        assert!(
            !core(Action::Replace, None)
                .sign(&dev(), &wt())
                .structural_ok()
        );
        // non-create + non-null prior: ok.
        assert!(
            core(Action::Replace, Some(Hash32([1; 32])))
                .sign(&dev(), &wt())
                .structural_ok()
        );

        // retention only on delete.
        let mut c = core(Action::MetadataUpdate, Some(Hash32([1; 32])));
        c.retention_until = Some("2026-07-01T00:00:00Z".into());
        assert!(!c.sign(&dev(), &wt()).structural_ok());
        let mut d = core(Action::Delete, Some(Hash32([1; 32])));
        d.retention_until = Some("2026-07-01T00:00:00Z".into());
        assert!(d.sign(&dev(), &wt()).structural_ok());
    }

    #[test]
    fn derivative_chain_is_independent() {
        let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let write = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let dm = DerivativeCore {
            version: DERIVATIVE_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
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
        .sign(&device, &write);
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
}
