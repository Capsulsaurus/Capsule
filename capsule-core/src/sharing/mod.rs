//! Share links — link generation and capability validation (slice `S-A5` in the repo-root
//! `SLICES.md`; SSoT: [Share Links]).
//!
//! A share link grants **view-only** access to an asset or album to a recipient with no
//! Capsule account: `https://server.tld/s/{opaque-id}#{secret}`. The fragment secret
//! carries the decryption material and never reaches the server; an optional passphrase
//! wraps it a second time via the password-based KDF, unwrapped **client-side** (the
//! server stores and returns only the wrapped material). The serving endpoints live in
//! the server's share module; this module owns link generation, the encapsulation
//! crypto, and the recipient-side [`open_scope`] path.
//!
//! ## Cryptographic shape
//!
//! [Cryptography — Keys § Non-registered accounts] owns the shape: *"we encapsulate the
//! decryption keys around the secret stored in the link ... the password-based KDF adds a
//! second encapsulation layer on top of the link secret."* Concretely, per link:
//!
//! 1. A fresh random **link secret** (`fragment_secret`, [`LINK_SECRET_LEN`] bytes) is
//!    drawn from the CSPRNG. It is the URL fragment `#{secret}` and never reaches the
//!    server.
//! 2. The scope's [`ScopeMaterial`] (a single file key, or an album's AMK ledger) is
//!    serialized to canonical CBOR and sealed under `HKDF(link_secret, salt=opaque_id)`
//!    — the *link-secret encapsulation*. The sealed bytes are opaque to the server.
//! 3. **If** a passphrase is supplied, that sealed blob is wrapped a **second** time under
//!    an [Argon2id][pw] key ([`crate::crypto::pwkdf`]) — the passphrase never leaves the
//!    client, and the server stores only this wrapped form (served from
//!    `/s/{opaque-id}/wrapped-secret`). See [`WrappedScope`].
//!
//! The recipient reverses the layers with [`open_scope`]: Argon2id-unwrap (client-side,
//! iff passphrase-protected), then link-secret-unwrap with the fragment secret.
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
/// resolved material, so it is unit-testable without a [`Workspace`](crate::lifecycle::Workspace).
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
