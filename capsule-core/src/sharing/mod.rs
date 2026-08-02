//! Share links — **contract skeleton** (slice `S-A5` in the repo-root `SLICES.md`;
//! SSoT: [Share Links]).
//!
//! A share link grants **view-only** access to an asset or album to a recipient with no
//! Capsule account: `https://server.tld/s/{opaque-id}#{secret}`. The fragment secret
//! carries the decryption material and never reaches the server; an optional passphrase
//! wraps it a second time via the password-based KDF, unwrapped **client-side** (the
//! server stores and returns only the wrapped material). The serving endpoints are planned in
//! `capsule-api::shares`; this module owns link generation and capability
//! validation on the issuing client.
//!
//! [Share Links]: https://docs/design/share-links/

use thiserror::Error;
use uuid::Uuid;

/// What a share link points at. The `{opaque-id}` itself carries **no** scope — the
/// server resolves scope from the link record, so the URL leaks nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareScope {
    /// A single asset.
    Asset(Uuid),
    /// A whole album.
    Album(Uuid),
}

/// Identifies an issued share link for revocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShareLinkId(pub Uuid);

/// An issued share link: the server-held record plus the fragment secret.
#[derive(Debug, Clone)]
pub struct ShareLink {
    /// Revocation handle.
    pub link_id: ShareLinkId,
    /// The random 128-bit opaque id (URL path component; never structured/UUIDv7).
    pub opaque_id: [u8; 16],
    /// The fragment secret carrying the decryption material (never sent to the server).
    /// When a passphrase is set, this is the **wrapped** form; unwrap is client-side.
    pub secret: Vec<u8>,
    /// RFC 3339 expiry, if any.
    pub expires_at: Option<String>,
}

/// Failure surfaced by share-link issuance/revocation.
#[derive(Debug, Error)]
pub enum SharingError {
    /// The scope does not exist or the issuer lacks access to it.
    #[error("share scope unavailable")]
    ScopeUnavailable,
    /// The link was not found (already revoked, or never issued).
    #[error("share link not found")]
    NotFound,
    /// Key material could not be prepared for the link secret.
    #[error("share crypto failure: {0}")]
    Crypto(&'static str),
}

/// Issues and revokes view-only share links on a trusted client — the seam
/// `lifecycle::Workspace` will implement. Issuance encapsulates the scope's decryption
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
    /// `S-A5` acceptance: generated opaque ids are ≥128-bit CSPRNG values, never
    /// structured; a generator producing shorter or guessable ids fails.
    #[test]
    #[ignore = "S-A5 contract: link issuance not yet implemented"]
    fn opaque_id_entropy() {
        unimplemented!("implemented by slice S-A5");
    }

    /// `S-A5` acceptance: with a passphrase set, the issued `secret` is the wrapped form
    /// and unwraps client-side via the password-based KDF; the passphrase itself never
    /// appears in any wire-bound structure.
    #[test]
    #[ignore = "S-A5 contract: passphrase wrap not yet implemented"]
    fn passphrase_unwrap_is_client_side() {
        unimplemented!("implemented by slice S-A5");
    }
}
