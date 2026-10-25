//! Share links — link generation and capability validation (SSoT: [Share Links]).
//!
//! A share link grants **view-only** access to an asset or album to a recipient with no
//! Capsule account: `https://server.tld/s/{opaque-id}#{secret}`. The fragment secret
//! carries the decryption material and never reaches the server; an optional passphrase
//! wraps it a second time via the password-based KDF, unwrapped **client-side** (the
//! server stores and returns only the wrapped material). The serving endpoints live in
//! the server's share module; this module owns link generation, the encapsulation
//! crypto, and the recipient-side [`open_scope`](crate::sharing::open_scope) path.
//!
//! ## Cryptographic shape
//!
//! [Cryptography — Keys § Non-registered accounts] owns the shape: *"we encapsulate the
//! decryption keys around the secret stored in the link ... the password-based KDF adds a
//! second encapsulation layer on top of the link secret."* Concretely, per link:
//!
//! 1. A fresh random **link secret** (`fragment_secret`,
//!    [`LINK_SECRET_LEN`](crate::sharing::LINK_SECRET_LEN) bytes) is drawn from the CSPRNG. It
//!    is the URL fragment `#{secret}` and never reaches the server.
//! 2. The scope's [`ScopeMaterial`](crate::sharing::ScopeMaterial) (a single file key, or an
//!    album's AMK ledger) is serialized to canonical CBOR and sealed under
//!    `HKDF(link_secret, salt=opaque_id)` — the *link-secret encapsulation*. The sealed bytes
//!    are opaque to the server.
//! 3. **If** a passphrase is supplied, that sealed blob is wrapped a **second** time under
//!    an [Argon2id][pw] key ([`crate::crypto::pwkdf`]) — the passphrase never leaves the
//!    client, and the server stores only this wrapped form (served from
//!    `/s/{opaque-id}/wrapped-secret`). See [`WrappedScope`](crate::sharing::WrappedScope).
//!
//! The recipient reverses the layers with [`open_scope`](crate::sharing::open_scope):
//! Argon2id-unwrap (client-side, iff passphrase-protected), then link-secret-unwrap with the
//! fragment secret.
//!
//! ## The record a viewer starts from
//!
//! What the viewer needs besides the keys — each covered asset's content address and key-free
//! crypto parameters — is a [`ShareMetadata`](crate::sharing::ShareMetadata) record, canonical
//! CBOR sealed under `HKDF(link_secret, salt=opaque_id, info=share-metadata/v1)` and uploaded as
//! the blob at the link's `metadata_hash`. [`publish_share`](crate::sharing::publish_share)
//! builds it and [`open_share_metadata`](crate::sharing::open_share_metadata) opens it. A
//! link-only scope travels inside the record; a passphrase scope is served only from
//! `/s/{opaque-id}/wrapped-secret`, so the fragment alone never opens what the passphrase guards.
//!
//! [Share Links]: https://docs/design/share-links/
//! [Cryptography — Keys § Non-registered accounts]: https://docs/design/cryptography/keys/#non-registered-accounts
//! [pw]: https://docs/design/cryptography/primitives/#password-based-kdf

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::cbor;
use crate::crypto::encryption::{open_blob, seal_blob};
use crate::crypto::keys::Amk;
use crate::crypto::primitives::{Argon2Params, info};
use crate::crypto::pwkdf::{self, WrappedSecret};
use crate::crypto::{kdf, rng};

/// Length of the random link secret carried in the URL fragment. 32 bytes = 256 bits,
/// comfortably above the design's ≥128-bit floor for the link secret.
pub const LINK_SECRET_LEN: usize = 32;

/// Length of the opaque URL-path id: a **full 128 bits** of CSPRNG entropy — never a
/// structured/UUIDv7 id whose embedded timestamp would cut real entropy to ~62 bits
/// (SSoT: [Share Links] Security Contract — Opaque-id entropy).
///
/// [Share Links]: https://docs/design/share-links/
pub const OPAQUE_ID_LEN: usize = 16;

/// What a share link points at. The `{opaque-id}` itself carries **no** scope — the
/// server resolves scope from the link record, so the URL leaks nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareScope {
    /// A single asset.
    Asset(Uuid),
    /// A whole album.
    Album(Uuid),
}

/// Identifies an issued share link for revocation. Internal owner-held handle (never in
/// the URL), so a creation-time-leaking UUIDv7 is appropriate here — unlike the
/// URL-exposed [`ShareLink::opaque_id`], which must be non-structured.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ShareLinkId(pub Uuid);

/// The scope's decryption material — the "decryption keys" that get encapsulated into the
/// link. Serialized to canonical CBOR before sealing, so two implementations encapsulate
/// byte-identically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScopeMaterial {
    /// A single asset: its per-file AES-256 key (already derived under the asset's epoch
    /// AMK, so the recipient needs no album key).
    Asset {
        /// The asset / file id the key belongs to.
        file_id: Uuid,
        /// The asset's per-file key.
        file_key: [u8; 32],
    },
    /// A whole album: the AMK for every epoch, so the recipient can derive any managed
    /// asset's file key regardless of the epoch it was written under.
    Album {
        /// The album id.
        album_id: Uuid,
        /// AMK bytes keyed by epoch.
        amks: BTreeMap<u32, [u8; 32]>,
    },
}

impl ScopeMaterial {
    /// Derive the per-file AES-256 key for `file_id`, written under epoch `amk_version` with
    /// the manifest's `nonce_prefix`.
    ///
    /// For an [`Asset`](ScopeMaterial::Asset) scope the key is returned directly (and the
    /// `file_id` must match) — the grant was minted with the asset's own `nonce_prefix`
    /// already folded in, so the argument is unused. For an [`Album`](ScopeMaterial::Album)
    /// scope it is derived from the requested epoch's AMK with `nonce_prefix` folded into the
    /// salt (the fold is what re-rolls the key per write). A file/epoch this material does not
    /// cover is [`SharingError::ScopeUnavailable`].
    ///
    /// The album arm assumes `key_mode = derived`: the triple carries no `wrapped_file_key`,
    /// so it cannot recover an adopted web-upload drop's key. A caller holding the asset's
    /// manifest uses [`file_key_for_manifest`](Self::file_key_for_manifest), which honours
    /// `key_mode`.
    pub fn file_key_for(
        &self,
        file_id: &Uuid,
        amk_version: u32,
        nonce_prefix: &[u8],
    ) -> Result<[u8; 32], SharingError> {
        match self {
            ScopeMaterial::Asset {
                file_id: owned,
                file_key,
            } => (owned == file_id)
                .then_some(*file_key)
                .ok_or(SharingError::ScopeUnavailable),
            ScopeMaterial::Album { amks, .. } => amks
                .get(&amk_version)
                .map(|amk| Amk::from_bytes(*amk).derive_file_key(file_id, nonce_prefix))
                .ok_or(SharingError::ScopeUnavailable),
        }
    }

    /// Recover the per-file key of the original `core` describes, honouring its `key_mode`.
    ///
    /// For an [`Asset`](ScopeMaterial::Asset) scope the carried key is returned (the issuer
    /// already resolved the key mode when it minted the grant) and `core.file_id` must match.
    /// For an [`Album`](ScopeMaterial::Album) scope the AMK of `core.amk_version` recovers it
    /// through [`recover_file_key`](crate::crypto::encryption::recover_file_key): a `derived`
    /// original re-derives, a `wrapped` one (an adopted web-upload drop) unwraps
    /// `wrapped_file_key`. A file, album, or epoch this material does not cover is
    /// [`SharingError::ScopeUnavailable`]; a wrapped key that does not open, or is missing, is
    /// [`SharingError::Crypto`].
    pub fn file_key_for_manifest(
        &self,
        core: &crate::crypto::provenance::ManifestCore,
    ) -> Result<[u8; 32], SharingError> {
        match self {
            ScopeMaterial::Asset { .. } => {
                self.file_key_for(&core.file_id, core.amk_version.0, &core.nonce_prefix)
            }
            ScopeMaterial::Album { album_id, amks } => {
                // Epoch numbers are per album, so another album's manifest at the same epoch
                // would otherwise be handed a key this grant does not cover.
                if core.album_id != *album_id {
                    return Err(SharingError::ScopeUnavailable);
                }
                let amk = amks
                    .get(&core.amk_version.0)
                    .ok_or(SharingError::ScopeUnavailable)?;
                crate::crypto::encryption::recover_file_key(&Amk::from_bytes(*amk), core)
                    .map_err(|_| SharingError::Crypto("asset file key recovery failed"))
            }
        }
    }

    /// [`file_key_for_manifest`](Self::file_key_for_manifest) over the four manifest fields it
    /// reads, for a recipient that is served those fields rather than the signed manifest: the
    /// browser share viewer, through `capsule-wasm`.
    ///
    /// An [`Asset`](ScopeMaterial::Asset) scope carries its key directly, as in
    /// [`file_key_for`](Self::file_key_for). For an [`Album`](ScopeMaterial::Album) scope the
    /// AMK of `amk_version` recovers it through
    /// [`recover_file_key_parts`](crate::crypto::encryption::recover_file_key_parts), which
    /// follows `key_mode`. There is no album id to check: the caller names no album, and a
    /// grant's material covers only its own. A wrapped key from another album does not open
    /// under this grant's AMK, and fails closed here.
    ///
    /// Errors are those of `file_key_for_manifest`: an epoch the grant does not hold is
    /// [`SharingError::ScopeUnavailable`], and a wrapped key that is missing or does not open is
    /// [`SharingError::Crypto`].
    pub fn file_key_for_key_mode(
        &self,
        file_id: &Uuid,
        amk_version: u32,
        nonce_prefix: &[u8],
        key_mode: crate::crypto::provenance::KeyMode,
        wrapped_file_key: Option<&crate::crypto::provenance::WrappedFileKey>,
    ) -> Result<[u8; 32], SharingError> {
        match self {
            ScopeMaterial::Asset { .. } => self.file_key_for(file_id, amk_version, nonce_prefix),
            ScopeMaterial::Album { amks, .. } => {
                let amk = amks
                    .get(&amk_version)
                    .ok_or(SharingError::ScopeUnavailable)?;
                crate::crypto::encryption::recover_file_key_parts(
                    &Amk::from_bytes(*amk),
                    file_id,
                    nonce_prefix,
                    key_mode,
                    wrapped_file_key,
                )
                .map_err(|_| SharingError::Crypto("asset file key recovery failed"))
            }
        }
    }
}

/// The server-held, **opaque** encapsulation of a link's [`ScopeMaterial`]. The server
/// can neither open it nor observe the passphrase; it only stores and returns these bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WrappedScope {
    /// Encapsulated around the link secret only — opened with the fragment secret alone.
    LinkOnly {
        /// `seal_blob(HKDF(link_secret, opaque_id), CBOR(scope_material))`.
        blob: Vec<u8>,
    },
    /// The link-secret encapsulation, **additionally** wrapped under an Argon2id
    /// passphrase key. Opened client-side (the passphrase never reaches the server); this
    /// is the material served from `/s/{opaque-id}/wrapped-secret`.
    Passphrase {
        /// `pwkdf::wrap(link_only_blob, passphrase)` — self-describing Argon2id params.
        wrapped: WrappedSecret,
    },
}

impl WrappedScope {
    /// Whether an Argon2id passphrase layer protects this material.
    pub fn is_passphrase_protected(&self) -> bool {
        matches!(self, WrappedScope::Passphrase { .. })
    }
}

/// A published revocation record. The serving endpoint refuses a link once its record
/// exists (within its fail-closed cache window — SSoT: [Share Links] Security Contract).
///
/// [Share Links]: https://docs/design/share-links/
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationRecord {
    /// The revoked link.
    pub link_id: ShareLinkId,
    /// RFC 3339 revocation timestamp.
    pub revoked_at: String,
}

/// The issuer-held record for one live share link — the authoritative state the serving
/// endpoint consults: the opaque id it is addressed by, the scope it resolves to,
/// the server-held wrapped material, expiry, and any revocation record.
#[derive(Debug, Clone)]
pub struct ShareLinkRecord {
    /// Revocation handle.
    pub link_id: ShareLinkId,
    /// The random 128-bit opaque id (URL path component).
    pub opaque_id: [u8; OPAQUE_ID_LEN],
    /// What the link grants access to.
    pub scope: ShareScope,
    /// The opaque, server-held encapsulated scope material.
    pub wrapped_scope: WrappedScope,
    /// RFC 3339 expiry, if any.
    pub expires_at: Option<String>,
    /// RFC 3339 issuance timestamp.
    pub created_at: String,
    /// Present once the link has been revoked.
    pub revocation: Option<RevocationRecord>,
}

impl ShareLinkRecord {
    /// Whether the link is still live at `now`: not revoked and not past its expiry.
    /// The serving endpoint is fail-closed; this is the authoritative predicate it caches.
    pub fn is_live_at(&self, now: jiff::Timestamp) -> bool {
        if self.revocation.is_some() {
            return false;
        }
        match &self.expires_at {
            None => true,
            Some(ts) => ts.parse::<jiff::Timestamp>().is_ok_and(|exp| now < exp),
        }
    }
}

/// An issued share link: the server-held record fields plus the fragment secret the
/// issuing client places in the URL. The fragment secret **never** leaves the client.
#[derive(Debug, Clone)]
pub struct ShareLink {
    /// Revocation handle.
    pub link_id: ShareLinkId,
    /// The random 128-bit opaque id (URL path component; never structured/UUIDv7).
    pub opaque_id: [u8; OPAQUE_ID_LEN],
    /// What the link grants access to.
    pub scope: ShareScope,
    /// The URL fragment `#{secret}` — the random link secret. **Never** sent to the
    /// server; the server holds only [`wrapped_scope`](ShareLink::wrapped_scope).
    pub fragment_secret: [u8; LINK_SECRET_LEN],
    /// The server-held, opaque encapsulation of the scope material.
    pub wrapped_scope: WrappedScope,
    /// RFC 3339 expiry, if any.
    pub expires_at: Option<String>,
}

impl ShareLink {
    /// Lowercase-hex of the opaque URL-path id.
    pub fn opaque_id_hex(&self) -> String {
        hex::encode(self.opaque_id)
    }

    /// Lowercase-hex of the URL fragment secret (the `#{secret}` component).
    pub fn fragment_hex(&self) -> String {
        hex::encode(self.fragment_secret)
    }

    /// Whether the link is passphrase-protected (an Argon2id layer wraps the material).
    pub fn is_passphrase_protected(&self) -> bool {
        self.wrapped_scope.is_passphrase_protected()
    }
}

/// Failure surfaced by share-link issuance, revocation, and recipient-side opening.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SharingError {
    /// The scope does not exist or the issuer lacks access to it.
    #[error("share scope unavailable")]
    ScopeUnavailable,
    /// The link was not found (already revoked, or never issued).
    #[error("share link not found")]
    NotFound,
    /// The link is passphrase-protected but no passphrase was supplied to open it.
    #[error("share link is passphrase-protected but no passphrase was supplied")]
    PassphraseRequired,
    /// The supplied passphrase was wrong (or the wrapped secret was tampered with).
    #[error("wrong passphrase or corrupt wrapped secret")]
    WrongPassphrase,
    /// Key material could not be prepared or opened for the link secret.
    #[error("share crypto failure: {0}")]
    Crypto(&'static str),
    /// Material opened but is not a structure this reader understands (an unknown share
    /// metadata version, or a record that breaks one of its rules).
    #[error("malformed share material: {0}")]
    Malformed(&'static str),
}

/// Draw a fresh opaque URL-path id: a full 128 bits of CSPRNG entropy, **not** a
/// structured or sequential identifier (SSoT: [Share Links] Security Contract).
///
/// [Share Links]: https://docs/design/share-links/
pub fn generate_opaque_id() -> [u8; OPAQUE_ID_LEN] {
    rng::random_array::<OPAQUE_ID_LEN>()
}

/// Derive the link-secret wrapping key: `HKDF(ikm=link_secret, salt=opaque_id,
/// info=share-scope-wrap/v1)`. Salting with the opaque id binds the encapsulation to its
/// one link, so a wrapped blob only opens under the id it was issued for.
fn link_wrap_key(
    fragment_secret: &[u8; LINK_SECRET_LEN],
    opaque_id: &[u8; OPAQUE_ID_LEN],
) -> [u8; 32] {
    kdf::derive_key32(fragment_secret, opaque_id, info::SHARE_SCOPE_WRAP_V1)
}

/// Encapsulate `material` around a fresh link secret, optionally adding the Argon2id
/// passphrase layer. Pure crypto — the caller supplies the CSPRNG-drawn secrets and the
/// resolved material, so it is unit-testable without a `Workspace`.
///
/// The issuer-side primitive [`open_scope`] reverses. Public so the cross-language KAT generator
/// (`cargo xtask share-kat`) can seal fixtures byte-identically to the native issuer, then have
/// the browser (`capsule-wasm`) reopen them.
pub fn encapsulate_scope(
    material: &ScopeMaterial,
    fragment_secret: &[u8; LINK_SECRET_LEN],
    opaque_id: &[u8; OPAQUE_ID_LEN],
    passphrase: Option<&str>,
    argon2: Argon2Params,
) -> Result<WrappedScope, SharingError> {
    let plaintext = cbor::to_canonical_vec(material)
        .map_err(|_| SharingError::Crypto("scope serialization failed"))?;
    let blob = seal_blob(&link_wrap_key(fragment_secret, opaque_id), &plaintext);
    match passphrase {
        None => Ok(WrappedScope::LinkOnly { blob }),
        Some(pw) => {
            let wrapped = pwkdf::wrap_with(&blob, pw.as_bytes(), argon2)
                .map_err(|_| SharingError::Crypto("passphrase wrap failed"))?;
            Ok(WrappedScope::Passphrase { wrapped })
        }
    }
}

/// Open a link's encapsulated [`ScopeMaterial`] **client-side** — the recipient path.
///
/// The recipient holds only the URL: the `opaque_id` (path) and `fragment_secret`
/// (fragment). When the link is passphrase-protected the caller must supply the
/// passphrase; the Argon2id unwrap runs **here, on the client**, and the passphrase is
/// never transmitted (SSoT: [Share Links] — Passphrase unwrap is client-side).
///
/// [Share Links]: https://docs/design/share-links/
pub fn open_scope(
    wrapped: &WrappedScope,
    opaque_id: &[u8; OPAQUE_ID_LEN],
    fragment_secret: &[u8; LINK_SECRET_LEN],
    passphrase: Option<&str>,
) -> Result<ScopeMaterial, SharingError> {
    let blob = match wrapped {
        WrappedScope::LinkOnly { blob } => blob.clone(),
        WrappedScope::Passphrase { wrapped } => {
            let pw = passphrase.ok_or(SharingError::PassphraseRequired)?;
            pwkdf::unwrap(wrapped, pw.as_bytes()).map_err(|_| SharingError::WrongPassphrase)?
        }
    };
    let plaintext = open_blob(&link_wrap_key(fragment_secret, opaque_id), &blob)
        .map_err(|_| SharingError::Crypto("scope decapsulation failed"))?;
    cbor::from_slice(&plaintext).map_err(|_| SharingError::Crypto("scope deserialization failed"))
}

// ───────────────────────── The share metadata record (issue #614) ─────────────────────────

/// Schema string of the share metadata record. A reader refuses any other value rather than
/// guessing at a shape it does not know.
pub const SHARE_METADATA_VERSION: &str = "share-metadata/v1";

/// One covered asset as a share viewer is served it: every **key-free** parameter it needs to
/// fetch the asset's ciphertext and decrypt it under the opened [`ScopeMaterial`].
///
/// The field names and wire rules mirror the signed manifest's
/// ([`ManifestCore`](crate::crypto::provenance::ManifestCore)): `key_mode` is wire-absent when
/// `derived`, and `wrapped_file_key` is present iff `key_mode = wrapped`. Nothing here is a key:
/// the nonce prefix is public by construction and the wrapped key is AMK-sealed ciphertext.
///
/// The asset's capture metadata travels only in [`sidecar`](Self::sidecar), the copy the issuing
/// client already passed through the [Privacy on Export] strip (`sidecar_for_export`, a
/// `native` item) before sealing. Every other field
/// is key-free crypto plumbing; a field that adds capture metadata is a boundary-crossing field
/// and must be added already stripped.
///
/// [Privacy on Export]: https://docs/design/metadata/#privacy-on-export
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedAsset {
    /// The asset's file id — the id its per-file key is derived or unwrapped for.
    pub asset_id: Uuid,
    /// The ciphertext's content address: the `{hash}` of `/s/{opaque-id}/blob/{hash}`.
    pub content_hash: crate::crypto::hash::Hash32,
    /// The plaintext media type, which the viewer wraps the decrypted bytes in.
    pub content_type: String,
    /// The plaintext byte length.
    pub size: u64,
    /// The AMK epoch the asset was written under; `0` for an asset-scoped grant, whose key the
    /// scope material carries directly.
    pub amk_version: u32,
    /// The STREAM nonce prefix the ciphertext was written with.
    pub nonce_prefix: [u8; crate::crypto::encryption::NONCE_PREFIX_LEN],
    /// How the file key is obtained; wire-absent when `derived`.
    #[serde(
        default,
        skip_serializing_if = "crate::crypto::provenance::KeyMode::is_derived"
    )]
    pub key_mode: crate::crypto::provenance::KeyMode,
    /// The AMK-sealed file key; present iff `key_mode = wrapped`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrapped_file_key: Option<crate::crypto::provenance::WrappedFileKey>,
    /// Canonical CBOR of the asset's **export** sidecar: the library sidecar after the
    /// [Privacy on Export] strip, unsigned once anything was stripped. Wire-absent when the
    /// issuer sent none, so a record without it encodes byte-identically to one written before
    /// the field existed, and a reader that predates it ignores it. A `native` reader decodes
    /// it with `SharedAsset::exported_sidecar`; the browser viewer carries it unread.
    ///
    /// [Privacy on Export]: https://docs/design/metadata/#privacy-on-export
    #[serde(default, skip_serializing_if = "Option::is_none", with = "serde_bytes")]
    pub sidecar: Option<Vec<u8>>,
}

#[cfg(feature = "native")]
impl SharedAsset {
    /// Decode the [`sidecar`](Self::sidecar) the issuer sealed for this asset, if any.
    ///
    /// # Errors
    ///
    /// [`SharingError::Malformed`] when the bytes are not a sidecar this reader understands, or
    /// are the sidecar of another asset.
    pub fn exported_sidecar(&self) -> Result<Option<crate::sidecar::SidecarV1>, SharingError> {
        let Some(bytes) = self.sidecar.as_deref() else {
            return Ok(None);
        };
        let sidecar = crate::sidecar::SidecarV1::from_canonical_slice(
            bytes,
            crate::sidecar::SIDECAR_SCHEMA_V1,
        )
        .map_err(|_| SharingError::Malformed("a shared asset's sidecar does not decode"))?;
        if sidecar.uuid != self.asset_id {
            return Err(SharingError::Malformed(
                "a shared asset's sidecar names another asset",
            ));
        }
        Ok(Some(sidecar))
    }
}

/// The record a share viewer starts from, sealed into the blob at the link's `metadata_hash`
/// (`GET /s/{opaque-id}` names the hash; `/s/{opaque-id}/blob/{hash}` serves the bytes).
///
/// Sealed under `HKDF(link_secret, salt=opaque_id, info=share-metadata/v1)`, so the server, which
/// never sees the fragment, cannot read it. Built by [`publish_share`] and read by
/// [`open_share_metadata`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareMetadata {
    /// Always [`SHARE_METADATA_VERSION`].
    pub version: String,
    /// RFC 3339 expiry the issuer set, shown to the viewer. The server enforces expiry on its own
    /// record and never discloses it; this copy is for display only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    /// The link-only [`WrappedScope`], present **iff** the link has no passphrase.
    ///
    /// A link with no passphrase has no wrapped secret on the server, so its scope travels here.
    /// A passphrase link's scope never does: placing it here would let the fragment alone open
    /// what the passphrase is meant to guard, so it is served only from
    /// `/s/{opaque-id}/wrapped-secret`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_scope: Option<WrappedScope>,
    /// Every asset the link covers.
    pub assets: Vec<SharedAsset>,
}

impl ShareMetadata {
    /// Check the structural rules a reader relies on: the schema string, a link scope that is
    /// the link-only form when present, and the manifest's `wrapped_file_key`-iff-`wrapped` rule.
    fn validate(&self) -> Result<(), SharingError> {
        if self.version != SHARE_METADATA_VERSION {
            return Err(SharingError::Malformed("unknown share metadata version"));
        }
        if self
            .link_scope
            .as_ref()
            .is_some_and(WrappedScope::is_passphrase_protected)
        {
            return Err(SharingError::Malformed(
                "share metadata carries a passphrase-wrapped scope",
            ));
        }
        let wrapped = crate::crypto::provenance::KeyMode::Wrapped;
        if self
            .assets
            .iter()
            .any(|asset| asset.wrapped_file_key.is_some() != (asset.key_mode == wrapped))
        {
            return Err(SharingError::Malformed(
                "a shared asset's wrapped_file_key disagrees with its key_mode",
            ));
        }
        Ok(())
    }
}

/// What an issuing client hands the server for one link: the sealed metadata blob to upload, and
/// the fields of `POST /v1/shares`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharePublication {
    /// The sealed [`ShareMetadata`] blob, uploaded at [`metadata_hash`](Self::metadata_hash).
    pub metadata_blob: Vec<u8>,
    /// The metadata blob's content address — the request's `metadata_hash`.
    pub metadata_hash: crate::crypto::hash::Hash32,
    /// Every address the link may serve: the metadata blob first, then each covered asset's
    /// ciphertext, without repeats — the request's `serves`.
    pub serves: Vec<crate::crypto::hash::Hash32>,
    /// Canonical CBOR of the passphrase [`WrappedScope`], present iff the link is
    /// passphrase-protected — the request's `wrapped_secret` (base64 on the wire), served back
    /// byte for byte from `/s/{opaque-id}/wrapped-secret`.
    pub wrapped_secret: Option<Vec<u8>>,
}

/// Derive the metadata-record key: `HKDF(ikm=link_secret, salt=opaque_id,
/// info=share-metadata/v1)`.
fn metadata_key(
    fragment_secret: &[u8; LINK_SECRET_LEN],
    opaque_id: &[u8; OPAQUE_ID_LEN],
) -> [u8; 32] {
    kdf::derive_key32(fragment_secret, opaque_id, info::SHARE_METADATA_V1)
}

/// Prepare everything the server needs to serve a link whose scope is already encapsulated
/// (`wrapped`, from [`encapsulate_scope`] under the same `fragment_secret` and `opaque_id`).
///
/// Decides where the scope travels: a link-only scope goes inside the sealed record, a passphrase
/// scope goes to [`SharePublication::wrapped_secret`] and nowhere else. The record is sealed under
/// the fragment-derived metadata key, so it opens with the URL alone.
///
/// # Errors
///
/// [`SharingError::Malformed`] when `assets` breaks the `wrapped_file_key`-iff-`wrapped` rule,
/// and [`SharingError::Crypto`] when a record cannot be serialized.
pub fn publish_share(
    wrapped: &WrappedScope,
    fragment_secret: &[u8; LINK_SECRET_LEN],
    opaque_id: &[u8; OPAQUE_ID_LEN],
    expires_at: Option<String>,
    assets: Vec<SharedAsset>,
) -> Result<SharePublication, SharingError> {
    let (link_scope, wrapped_secret) = match wrapped {
        WrappedScope::LinkOnly { .. } => (Some(wrapped.clone()), None),
        WrappedScope::Passphrase { .. } => {
            let bytes = cbor::to_canonical_vec(wrapped)
                .map_err(|_| SharingError::Crypto("wrapped scope serialization failed"))?;
            (None, Some(bytes))
        }
    };
    let record = ShareMetadata {
        version: SHARE_METADATA_VERSION.to_owned(),
        expires_at,
        link_scope,
        assets,
    };
    record.validate()?;
    let plaintext = cbor::to_canonical_vec(&record)
        .map_err(|_| SharingError::Crypto("share metadata serialization failed"))?;
    let metadata_blob = seal_blob(&metadata_key(fragment_secret, opaque_id), &plaintext);
    let metadata_hash = crate::crypto::hash::hash_bytes(&metadata_blob);

    let mut serves = vec![metadata_hash];
    for asset in &record.assets {
        if !serves.contains(&asset.content_hash) {
            serves.push(asset.content_hash);
        }
    }
    tracing::debug!(
        assets = record.assets.len(),
        passphrase = wrapped_secret.is_some(),
        blob_len = metadata_blob.len(),
        "share metadata sealed"
    );
    Ok(SharePublication {
        metadata_blob,
        metadata_hash,
        serves,
        wrapped_secret,
    })
}

/// Open the sealed metadata record a viewer fetched from the link's `metadata_hash` — the
/// recipient half of [`publish_share`], run client-side with the URL's opaque id and fragment.
///
/// # Errors
///
/// [`SharingError::Crypto`] when the blob does not open under this URL's fragment (a wrong
/// fragment, a wrong opaque id, or a tampered blob, which are indistinguishable), and
/// [`SharingError::Malformed`] when it opens but is not a record this reader understands.
pub fn open_share_metadata(
    blob: &[u8],
    opaque_id: &[u8; OPAQUE_ID_LEN],
    fragment_secret: &[u8; LINK_SECRET_LEN],
) -> Result<ShareMetadata, SharingError> {
    let plaintext = open_blob(&metadata_key(fragment_secret, opaque_id), blob)
        .map_err(|_| SharingError::Crypto("share metadata decapsulation failed"))?;
    let record: ShareMetadata = cbor::from_slice(&plaintext)
        .map_err(|_| SharingError::Malformed("share metadata is not a record"))?;
    record.validate()?;
    Ok(record)
}

/// Issues and revokes view-only share links on a trusted client — the seam
/// `lifecycle::Workspace` implements. Issuance encapsulates the scope's decryption
/// material around a fresh link secret; the optional passphrase adds the second,
/// client-side-unwrapped encapsulation layer.
pub trait ShareLinkIssuer {
    /// Issue a view-only link for `scope`, optionally expiring and/or
    /// passphrase-wrapped.
    fn create_link(
        &mut self,
        scope: ShareScope,
        expires_at: Option<String>,
        passphrase: Option<&str>,
    ) -> Result<ShareLink, SharingError>;

    /// Revoke a link; the serve path refuses it within its fail-closed cache window.
    fn revoke_link(&mut self, link: ShareLinkId) -> Result<(), SharingError>;
}

#[cfg(test)]
mod tests;
