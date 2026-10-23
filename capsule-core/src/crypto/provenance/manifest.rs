//! The signed asset manifest and derivative manifest (SSoT: [Cryptography — Provenance]).
//!
//! A manifest carries **two** hybrid signatures over the same canonical core bytes:
//! `device_sig` (provenance — which device produced it) and `write_sig` (authorization —
//! the album's per-epoch write-tier key). Both must verify at [`verify_asset`]. The core
//! excludes the signatures, so signing bytes are unambiguous and downgrade-resistant
//! (both sigs cover `crypto_suite_id`, `protocol_version`, and `prior_provenance_hash`).
//!
//! [`verify_asset`]: crate::crypto::verify_asset::verify_asset
//! [Cryptography — Provenance]: https://docs/design/cryptography/provenance/

use std::collections::BTreeMap;

use ciborium::value::Value;
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
///
/// **Unknown keys survive under the signature** ([Versioning — invariant 2]). A key this build
/// does not know is kept verbatim in [`Self::unknown`], re-emitted by the canonical encoder
/// beside the known fields, and therefore covered by both signatures: a newer writer's manifest
/// that only adds a field verifies here, and stripping that field fails verification. The
/// decoder is hand-written for that reason (see the `Deserialize` impl below): `serde(flatten)`
/// buffers unknown values through serde's private content type, which cannot hold a CBOR tag
/// or an integer wider than 64 bits, so a future field carrying either would make the whole
/// manifest undecodable instead of preserved.
///
/// [Versioning — invariant 2]: https://docs/design/versioning/#system-invariants
#[derive(Debug, Clone, PartialEq, Serialize)]
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
    /// The account whose device signed **this record**.
    ///
    /// Per-record, not per-asset: a `delete` written by a second device — or by a member of a
    /// shared album — names that writer, not the account that created the asset. The asset's
    /// original creator is recoverable from the `create` record at the head of the append-only
    /// provenance chain, which is where it belongs.
    ///
    /// The pairing with [`Self::created_by_device`] is load-bearing rather than descriptive:
    /// [`verify_asset`](crate::crypto::verify_asset::verify_asset) resolves the device *inside
    /// this account's* published directory (step 6) and verifies [`AssetManifest::device_sig`]
    /// under that entry's key (step 8), so a record naming anyone but its own signer cannot
    /// verify. Album write authority is decided separately, by `write_sig` at step 10.
    pub created_by_user: Uuid,
    /// The device that signed **this record**, resolved in [`Self::created_by_user`]'s directory.
    ///
    /// Per-record for the same reason and with the same consequence: it must be the device whose
    /// DSK produced [`AssetManifest::device_sig`], or step 8 of
    /// [`verify_asset`](crate::crypto::verify_asset::verify_asset) rejects the manifest. The
    /// server mirrors the resolvable half key-free as invariant 7 — the device must be in the
    /// *calling* account's published directory, with `added_at` before the manifest's timestamp.
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
    /// The album this asset's album was **forked from**, when it is a fork.
    ///
    /// The normative link between an upgraded album and its predecessor — never the MLS group
    /// name, which is an internal detail. A device that joins the fork reads this and can walk
    /// back to the album the asset came from; without it, a fork looks like an unrelated album
    /// full of assets that appeared from nowhere.
    ///
    /// **Wire-absent when there is none**, never present-`null`. This is inside the signed core,
    /// so a present-`null` would change [`signing_bytes`](Self::signing_bytes) and silently break
    /// re-verification of every manifest written before the field existed — the same trap
    /// `dek_public` avoids, and the reason
    /// `an_absent_lineage_is_wire_absent_and_does_not_move_the_signature` exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgraded_from: Option<crate::crypto::upgrade::UpgradeLineage>,
    /// Keys a newer writer added that this build does not know, preserved verbatim and signed
    /// over. Empty (and so wire-absent) on everything this build writes.
    ///
    /// A key here must never collide with a known field name: the decoder only ever places the
    /// keys it did not recognise here, and a caller building a core by hand must do the same.
    #[serde(flatten)]
    pub unknown: BTreeMap<String, Value>,
}

// `Value` holds floats, so it is only `PartialEq`. Every known field is `Eq`; the one exception
// is a core whose unknown map holds a NaN, which compares unequal to itself. Equality is never
// how a manifest is trusted (signatures over the canonical bytes are), so `Eq` stays.
impl Eq for ManifestCore {}

impl<'de> Deserialize<'de> for ManifestCore {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let mut m = cbor::OpenMap::deserialize("asset manifest core", d)?;
        // Struct-literal fields evaluate in order, so `version` is decoded first.
        Ok(Self {
            version: m.take("version")?,
            crypto_suite_id: m.take("crypto_suite_id")?,
            protocol_version: m.take("protocol_version")?,
            file_id: m.take("file_id")?,
            album_id: m.take("album_id")?,
            amk_version: m.take("amk_version")?,
            ciphertext_hash: m.take("ciphertext_hash")?,
            plaintext_size: m.take("plaintext_size")?,
            chunk_size: m.take("chunk_size")?,
            nonce_prefix: m.take("nonce_prefix")?,
            key_mode: m.take_opt("key_mode")?.unwrap_or_default(),
            wrapped_file_key: m.take_opt("wrapped_file_key")?,
            metadata_blob_hash: m.take_opt("metadata_blob_hash")?,
            created_by_user: m.take("created_by_user")?,
            created_by_device: m.take("created_by_device")?,
            client_version: m.take("client_version")?,
            timestamp: m.take("timestamp")?,
            action: m.take("action")?,
            prior_provenance_hash: m.take_opt("prior_provenance_hash")?,
            retention_until: m.take_opt("retention_until")?,
            upgraded_from: m.take_opt("upgraded_from")?,
            unknown: m.into_unknown(),
        })
    }
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
    /// - `wrapped_file_key` is present **iff** `key_mode = wrapped`;
    /// - `metadata_blob_hash` is present **iff** the action binds a metadata blob
    ///   ([`Action::binds_metadata_blob`] — `create | replace | metadata-update`).
    ///
    /// The first two are mirrored by the server envelope; the wrapped-key rule is the
    /// signature-visible presence rule for an adopted web-upload drop, enforced here at
    /// `verify_asset`. The `metadata_blob_hash` presence-by-action rule is enforced
    /// because the `Workspace` populates the field per the [sealing order], so `verify_asset`
    /// can require its presence exactly on the
    /// metadata-bearing actions and its absence on the other four. The server mirrors the
    /// *value* half key-free at [invariant 25]
    /// ([`check_metadata_blob_envelope`](crate::validation::check_metadata_blob_envelope)).
    ///
    /// [sealing order]: https://docs/design/metadata/#provenance-binding-and-sealing-order
    /// [invariant 25]: https://docs/design/threat-model/validation/#server-side-validation-invariants
    pub fn structural_ok(&self) -> bool {
        let core = &self.core;
        let prior_rule = core.prior_provenance_hash.is_none() == core.action.is_create();
        let retention_rule = core.retention_until.is_none() || core.action == Action::Delete;
        let wrapped_rule = core.wrapped_file_key.is_some() == (core.key_mode == KeyMode::Wrapped);
        let blob_hash_rule = core.metadata_blob_hash.is_some() == core.action.binds_metadata_blob();
        prior_rule && retention_rule && wrapped_rule && blob_hash_rule
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
    ///
    /// **A `String`, and deliberately so, even though the still formats are a closed set.**
    /// Two reasons, and both are about keeping a *policy* rejection from becoming a *parse*
    /// failure:
    ///
    /// - the same field carries the embedding-role grammar `embedding/{model_id}`
    ///   (`crate::ml`), which no still-format enum can model, so a single typed field would
    ///   have to be an enum over both grammars;
    /// - a `#[serde(try_from = "String")]` newtype would make an *older* manifest naming a
    ///   future codec fail at deserialisation — before its signature is examined at all —
    ///   turning "this receiver does not recognise that format" into "this manifest is
    ///   unreadable".
    ///
    /// The closed set is enforced at the two boundaries instead: production, because
    /// `media::generate_still_derivatives` only ever writes
    /// [`DerivativeFormat::mime`](crate::derivative_format::DerivativeFormat::mime); and
    /// verification, via
    /// [`verify_still_format`](crate::derivative_format::verify_still_format), which rejects a
    /// still-role manifest whose value is outside the set and leaves the embedding-role grammar
    /// alone.
    ///
    /// Both live in [`crate::derivative_format`], which is **unconditional** — deliberately not
    /// behind the `media` feature. `capsule-server` and `capsule-wasm` build
    /// `default-features = false`, and they are exactly the crates that receive a manifest they
    /// did not author, so a check they could not link would be a closed set only its producer
    /// could evaluate. SSoT: [Thumbnails](https://docs/design/thumbnails/).
    pub format: String,
    /// Content-address digest over the derivative ciphertext.
    pub ciphertext_hash: Hash32,
    /// The STREAM nonce prefix the derivative ciphertext was produced under; folded into the
    /// file-key salt, so it also selects the key.
    ///
    /// **Required, not `Option`.** Derivative bytes are encrypted client-side exactly like the
    /// original ([Encryption](https://docs/design/cryptography/encryption/)), so a receiver that
    /// cannot recover this cannot decrypt the blob at all — an absent prefix would be an
    /// unopenable derivative rather than a tolerable gap. It is safe to make it required
    /// because no real `derivative-manifest/v1` has ever been written to any store: derivatives
    /// were unconditionally `DeferredNoCodec` until the decoder landed, and `crate::ml`
    /// constructs no derivative manifest. There is nothing to stay compatible with, so the
    /// schema string does not move.
    pub nonce_prefix: [u8; 7],
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

impl DerivativeManifest {
    /// The canonical bytes both signatures cover.
    pub fn signing_bytes(&self) -> Vec<u8> {
        self.core.signing_bytes()
    }

    /// The chain link the next manifest of this `(source_asset_id, role)` names as its
    /// `prior_provenance_hash`: SHA-256 over this manifest's canonical CBOR, signatures
    /// included — the same content-hash link the asset provenance chain uses, and the value
    /// the producer (`media::derivative`) chains with.
    pub fn link_hash(&self) -> Hash32 {
        crate::crypto::hash::hash_bytes(
            &cbor::to_canonical_vec(self).expect("derivative manifest serializes"),
        )
    }
}

#[cfg(test)]
mod tests;
