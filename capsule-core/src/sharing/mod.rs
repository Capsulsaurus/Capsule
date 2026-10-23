//! Share links — link generation and capability validation (slice `S-A5` in the repo-root
//! `SLICES.md`; SSoT: [Share Links]).
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
/// endpoint (S-C4) consults: the opaque id it is addressed by, the scope it resolves to,
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
/// The record carries no capture metadata (no EXIF, GPS, camera serial or device id), so the
/// [Privacy on Export] strip has nothing to remove from it today. A field that adds any is a
/// boundary-crossing field and must be added already stripped (#432).
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
mod tests {
    use super::*;
    use crate::crypto::primitives::Argon2Params;

    /// Tiny Argon2id parameters keep the passphrase-wrap tests fast.
    fn fast() -> Argon2Params {
        Argon2Params {
            mem_kib: 64,
            t_cost: 1,
            p_cost: 1,
        }
    }

    fn sample_material() -> ScopeMaterial {
        ScopeMaterial::Asset {
            file_id: Uuid::now_v7(),
            file_key: [0x42; 32],
        }
    }

    /// `S-A5` acceptance / doc "Opaque-id entropy (unit)": generated opaque ids are
    /// 128-bit CSPRNG values, never structured; a generator producing shorter or
    /// guessable/sequential ids fails.
    #[test]
    fn opaque_id_entropy() {
        // A full 128 bits — not a shorter identifier.
        assert_eq!(OPAQUE_ID_LEN, 16, "opaque id must be a full 128 bits");
        assert_eq!(generate_opaque_id().len(), 16);

        // Draw a batch: all distinct (no collisions), and — unlike a UUIDv7 — carrying no
        // monotonic timestamp prefix. A structured/sequential generator would fail both.
        let ids: Vec<[u8; OPAQUE_ID_LEN]> = (0..256).map(|_| generate_opaque_id()).collect();
        for i in 0..ids.len() {
            for j in (i + 1)..ids.len() {
                assert_ne!(ids[i], ids[j], "two CSPRNG opaque ids must never collide");
            }
        }
        // No UUIDv7 version/variant structure: the version nibble (byte 6 high nibble) and
        // variant bits (byte 8) are not pinned to 0x7_ / 0b10 as a v7 generator forces.
        assert!(
            ids.iter().any(|id| (id[6] >> 4) != 0x7),
            "opaque ids must not all carry the UUIDv7 version nibble"
        );
        assert!(
            ids.iter().any(|id| (id[8] & 0xc0) != 0x80),
            "opaque ids must not all carry the UUID variant bits"
        );
        // Not sequential: the leading 48 bits (a v7 timestamp field) are not monotonic.
        let high48 =
            |id: &[u8; OPAQUE_ID_LEN]| id[..6].iter().fold(0u64, |acc, &b| (acc << 8) | b as u64);
        assert!(
            ids.windows(2).any(|w| high48(&w[0]) >= high48(&w[1])),
            "opaque ids must not be time-ordered/sequential like a UUIDv7"
        );
    }

    /// `S-A5` acceptance / doc "Passphrase unwrap locality (unit)": with a passphrase set,
    /// the server-held material is the **wrapped** form and unwraps **client-side** via the
    /// password-based KDF; the passphrase itself never appears in any wire-bound structure.
    #[test]
    fn passphrase_unwrap_is_client_side() {
        const PASSPHRASE: &str = "correct horse battery staple";
        let material = sample_material();
        let fragment = rng::random_array::<LINK_SECRET_LEN>();
        let opaque_id = generate_opaque_id();

        let wrapped =
            encapsulate_scope(&material, &fragment, &opaque_id, Some(PASSPHRASE), fast()).unwrap();

        // The server holds only the wrapped form — an Argon2id passphrase layer.
        let WrappedScope::Passphrase { wrapped: inner } = &wrapped else {
            panic!("a passphrase-protected link must yield the wrapped form");
        };
        // Argon2id really ran (self-describing cost params recorded in-band).
        assert_eq!(inner.mem_kib, fast().mem_kib);
        assert_eq!(inner.t_cost, fast().t_cost);

        // The passphrase never appears in any wire-bound byte of the server-held material.
        let pw = PASSPHRASE.as_bytes();
        let contains = |h: &[u8]| h.windows(pw.len()).any(|w| w == pw);
        assert!(
            !contains(&inner.ciphertext),
            "passphrase must not be in ciphertext"
        );
        assert!(!contains(&inner.salt), "passphrase must not be in the salt");
        assert!(
            !contains(&inner.nonce),
            "passphrase must not be in the nonce"
        );

        // Client-side unwrap with the passphrase + fragment recovers the exact material.
        let opened = open_scope(&wrapped, &opaque_id, &fragment, Some(PASSPHRASE)).unwrap();
        assert_eq!(opened, material);

        // A wrong passphrase is rejected client-side (the Argon2id backstop) ...
        assert_eq!(
            open_scope(&wrapped, &opaque_id, &fragment, Some("wrong")),
            Err(SharingError::WrongPassphrase),
        );
        // ... and opening a passphrase link with no passphrase is refused, not silently served.
        assert_eq!(
            open_scope(&wrapped, &opaque_id, &fragment, None),
            Err(SharingError::PassphraseRequired),
        );
    }

    /// The unprotected path: the scope material round-trips under the fragment secret
    /// alone, and the encapsulation is bound to its opaque id.
    #[test]
    fn link_only_round_trip_is_bound_to_opaque_id() {
        let material = ScopeMaterial::Album {
            album_id: Uuid::now_v7(),
            amks: BTreeMap::from([(1u32, [1u8; 32]), (2u32, [2u8; 32])]),
        };
        let fragment = rng::random_array::<LINK_SECRET_LEN>();
        let opaque_id = generate_opaque_id();
        let wrapped = encapsulate_scope(&material, &fragment, &opaque_id, None, fast()).unwrap();
        assert!(!wrapped.is_passphrase_protected());

        // Correct fragment + opaque id → recovers the material.
        assert_eq!(
            open_scope(&wrapped, &opaque_id, &fragment, None).unwrap(),
            material,
        );
        // A different opaque id (the HKDF salt) does not open it.
        assert!(open_scope(&wrapped, &generate_opaque_id(), &fragment, None).is_err());
        // A different fragment secret does not open it.
        let other = rng::random_array::<LINK_SECRET_LEN>();
        assert!(open_scope(&wrapped, &opaque_id, &other, None).is_err());
    }

    fn shared_asset(tag: u8) -> SharedAsset {
        SharedAsset {
            asset_id: Uuid::from_u128(u128::from(tag)),
            content_hash: crate::crypto::hash::Hash32([tag; 32]),
            content_type: "image/png".to_owned(),
            size: 5008,
            amk_version: 0,
            nonce_prefix: [tag; 7],
            key_mode: crate::crypto::provenance::KeyMode::Derived,
            wrapped_file_key: None,
        }
    }

    /// #614: a link-only link carries its scope inside the sealed record, which opens with the
    /// URL alone; nothing goes to the server's wrapped-secret slot.
    #[test]
    fn a_link_only_publication_seals_its_scope_into_the_record() {
        let material = sample_material();
        let fragment = rng::random_array::<LINK_SECRET_LEN>();
        let opaque_id = generate_opaque_id();
        let wrapped = encapsulate_scope(&material, &fragment, &opaque_id, None, fast()).unwrap();
        let assets = vec![shared_asset(1), shared_asset(2)];

        let publication = publish_share(
            &wrapped,
            &fragment,
            &opaque_id,
            Some("2030-01-01T00:00:00Z".to_owned()),
            assets.clone(),
        )
        .unwrap();
        assert_eq!(publication.wrapped_secret, None);
        assert_eq!(
            publication.metadata_hash,
            crate::crypto::hash::hash_bytes(&publication.metadata_blob),
            "the metadata hash is the blob's content address"
        );
        assert_eq!(
            publication.serves,
            vec![
                publication.metadata_hash,
                assets[0].content_hash,
                assets[1].content_hash
            ],
        );

        let record =
            open_share_metadata(&publication.metadata_blob, &opaque_id, &fragment).unwrap();
        assert_eq!(record.version, SHARE_METADATA_VERSION);
        assert_eq!(record.assets, assets);
        assert_eq!(record.expires_at.as_deref(), Some("2030-01-01T00:00:00Z"));
        let scope = record
            .link_scope
            .expect("a link-only record carries its scope");
        assert_eq!(
            open_scope(&scope, &opaque_id, &fragment, None).unwrap(),
            material
        );
    }

    /// #614: a passphrase link's scope never enters the fragment-sealed record — it goes to the
    /// server's wrapped-secret slot, as the CBOR the browser decodes.
    #[test]
    fn a_passphrase_publication_keeps_its_scope_out_of_the_record() {
        let material = sample_material();
        let fragment = rng::random_array::<LINK_SECRET_LEN>();
        let opaque_id = generate_opaque_id();
        let wrapped =
            encapsulate_scope(&material, &fragment, &opaque_id, Some("pw"), fast()).unwrap();

        let publication =
            publish_share(&wrapped, &fragment, &opaque_id, None, vec![shared_asset(3)]).unwrap();
        let record =
            open_share_metadata(&publication.metadata_blob, &opaque_id, &fragment).unwrap();
        assert_eq!(
            record.link_scope, None,
            "the fragment alone must not open it"
        );

        let secret = publication
            .wrapped_secret
            .expect("a passphrase link has a wrapped secret");
        let served: WrappedScope = cbor::from_slice(&secret).unwrap();
        assert_eq!(served, wrapped);
        assert_eq!(
            open_scope(&served, &opaque_id, &fragment, Some("pw")).unwrap(),
            material
        );
    }

    /// The record is bound to its URL: another fragment, another opaque id, or a flipped byte
    /// does not open it, and all three are the same `Crypto` refusal.
    #[test]
    fn share_metadata_opens_only_under_its_own_url() {
        let fragment = rng::random_array::<LINK_SECRET_LEN>();
        let opaque_id = generate_opaque_id();
        let wrapped =
            encapsulate_scope(&sample_material(), &fragment, &opaque_id, None, fast()).unwrap();
        let blob = publish_share(&wrapped, &fragment, &opaque_id, None, vec![])
            .unwrap()
            .metadata_blob;

        let other_fragment = rng::random_array::<LINK_SECRET_LEN>();
        let mut tampered = blob.clone();
        *tampered.last_mut().unwrap() ^= 1;
        for (blob, id, secret) in [
            (&blob, &generate_opaque_id(), &fragment),
            (&blob, &opaque_id, &other_fragment),
            (&tampered, &opaque_id, &fragment),
        ] {
            assert!(matches!(
                open_share_metadata(blob, id, secret),
                Err(SharingError::Crypto(_))
            ));
        }
    }

    /// A record that opens but breaks a rule is `Malformed`, never silently accepted: an unknown
    /// version, a passphrase scope inside the record, a key mode at odds with its wrapped key.
    #[test]
    fn a_record_that_breaks_a_rule_is_malformed() {
        let fragment = rng::random_array::<LINK_SECRET_LEN>();
        let opaque_id = generate_opaque_id();
        let seal = |record: &ShareMetadata| {
            seal_blob(
                &metadata_key(&fragment, &opaque_id),
                &cbor::to_canonical_vec(record).unwrap(),
            )
        };
        let good = ShareMetadata {
            version: SHARE_METADATA_VERSION.to_owned(),
            expires_at: None,
            link_scope: None,
            assets: vec![shared_asset(4)],
        };
        assert!(open_share_metadata(&seal(&good), &opaque_id, &fragment).is_ok());

        let mut unknown = good.clone();
        unknown.version = "share-metadata/v2".to_owned();
        let mut passphrase = good.clone();
        passphrase.link_scope = Some(
            encapsulate_scope(
                &sample_material(),
                &fragment,
                &opaque_id,
                Some("pw"),
                fast(),
            )
            .unwrap(),
        );
        let mut mode = good.clone();
        mode.assets[0].key_mode = crate::crypto::provenance::KeyMode::Wrapped;
        for record in [unknown, passphrase, mode.clone()] {
            assert!(matches!(
                open_share_metadata(&seal(&record), &opaque_id, &fragment),
                Err(SharingError::Malformed(_))
            ));
        }
        let not_a_record = seal_blob(&metadata_key(&fragment, &opaque_id), b"\x01");
        assert!(matches!(
            open_share_metadata(&not_a_record, &opaque_id, &fragment),
            Err(SharingError::Malformed(_))
        ));

        // The issuer refuses to seal the key-mode case in the first place.
        let wrapped =
            encapsulate_scope(&sample_material(), &fragment, &opaque_id, None, fast()).unwrap();
        assert!(matches!(
            publish_share(&wrapped, &fragment, &opaque_id, None, mode.assets),
            Err(SharingError::Malformed(_))
        ));
    }

    /// The metadata key and the scope-wrap key are separate HKDF labels over the same inputs.
    #[test]
    fn the_metadata_key_is_not_the_scope_wrap_key() {
        let fragment = [7u8; LINK_SECRET_LEN];
        let opaque_id = [9u8; OPAQUE_ID_LEN];
        assert_ne!(
            metadata_key(&fragment, &opaque_id),
            link_wrap_key(&fragment, &opaque_id)
        );
    }

    /// Album material derives per-epoch file keys; asset material returns its one key.
    #[test]
    fn scope_material_derives_file_keys() {
        let album_id = Uuid::now_v7();
        let file_id = Uuid::now_v7();
        let amks = BTreeMap::from([(1u32, [7u8; 32]), (2u32, [9u8; 32])]);
        let album = ScopeMaterial::Album {
            album_id,
            amks: amks.clone(),
        };
        let np = [0xEEu8; 7];
        // Album epoch key matches a direct AMK derivation under the same folded nonce prefix ...
        assert_eq!(
            album.file_key_for(&file_id, 1, &np).unwrap(),
            Amk::from_bytes(amks[&1]).derive_file_key(&file_id, &np),
        );
        // ... a distinct nonce prefix re-rolls it (the fold) ...
        assert_ne!(
            album.file_key_for(&file_id, 1, &np).unwrap(),
            album.file_key_for(&file_id, 1, &[0x11u8; 7]).unwrap(),
        );
        // ... and an epoch the material does not cover is unavailable.
        assert_eq!(
            album.file_key_for(&file_id, 3, &np),
            Err(SharingError::ScopeUnavailable),
        );

        let asset = ScopeMaterial::Asset {
            file_id,
            file_key: [3u8; 32],
        };
        // An asset grant carries the folded key; the nonce-prefix argument is unused.
        assert_eq!(asset.file_key_for(&file_id, 1, &np).unwrap(), [3u8; 32]);
        // A different asset id is not covered by a single-asset grant.
        assert_eq!(
            asset.file_key_for(&Uuid::now_v7(), 1, &np),
            Err(SharingError::ScopeUnavailable),
        );
    }
}
