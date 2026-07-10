//! Web-upload guest drops — **contract skeleton** (slice `S-A6` in the repo-root
//! `SLICES.md`; SSoT: [Web Upload]).
//!
//! A guest with an upload link seals each asset under a fresh random key `K`,
//! encapsulates `K` to the link's Drop Key, and uploads the sealed bytes to the
//! provisioning user's staging inbox. Nothing becomes a library asset until one of that
//! user's trusted clients **adopts** the drop — decapsulating `K`, rewrapping it under the
//! album AMK (`asset-keywrap/v1`, [`KeyMode::Wrapped`]), and signing an ordinary `create`
//! manifest. The guest is never a signer; drops never flow through `verify_asset`.
//!
//! This module owns the client-side halves: link issuance, drop sealing (compiled to WASM
//! for `capsule-web`), and adoption. The server halves (drop store, inbox, atomic
//! inbox→album promotion) live in `capsule-api-media::drops`.
//!
//! [Web Upload]: https://docs/design/web-upload/
//! [`KeyMode::Wrapped`]: crate::crypto::provenance::KeyMode

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::crypto::hash::Hash32;
use crate::crypto::provenance::AssetManifest;

/// A provisioned upload link: the server-held record plus the fragment-delivered public
/// half. The `opaque_id` follows the share-link rule (random ≥128-bit, never structured);
/// `drop_pubkey` travels only in the URL fragment and never reaches the server.
#[derive(Debug, Clone)]
pub struct UploadLink {
    /// The link's random 128-bit opaque id (the URL path component).
    pub opaque_id: [u8; 16],
    /// The Drop Key public half (KEM encapsulation key; URL fragment only).
    pub drop_pubkey: Vec<u8>,
    /// The caps this link was provisioned with.
    pub caps: LinkCaps,
}

/// Identifies a provisioned upload link for revocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadLinkId(pub Uuid);

/// Identifies a pending drop in the provisioning user's inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DropId(pub Uuid);

/// Per-link caps, enforced server-side at the no-key layer on every drop-session
/// creation ([Web Upload — Security Contract](https://docs/design/web-upload/)).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkCaps {
    /// RFC 3339 expiry; `None` = no expiry (revocation still applies).
    pub expires_at: Option<String>,
    /// Cumulative byte cap across all drops on this link.
    pub max_total_bytes: Option<u64>,
    /// Maximum number of files this link may deposit.
    pub max_file_count: Option<u32>,
    /// Maximum single-file size.
    pub max_file_size: Option<u64>,
    /// Whether the link dies after its first successful drop.
    pub single_use: bool,
}

/// The unsigned descriptor a guest uploads beside the sealed ciphertext. Deliberately
/// **not** an `AssetManifest`: no signatures, no `album_id`, no provenance link. Its
/// integrity is established only when a trusted client decapsulates `K` and the STREAM
/// tags verify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DropDescriptor {
    /// Closed enum for the link's pinned `protocol_version` (same set as a manifest's).
    pub content_type: String,
    /// Total plaintext byte length.
    pub plaintext_size: u64,
    /// The STREAM plaintext chunk size (owned by Encryption).
    pub chunk_size: u32,
    /// The STREAM nonce prefix used for this seal.
    pub nonce_prefix: [u8; 7],
    /// Content-address digest of the STREAM ciphertext.
    pub ciphertext_hash: Hash32,
    /// `K` encapsulated to the link's Drop Key; length fixed by `crypto_suite_id`.
    #[serde(with = "serde_bytes")]
    pub kem_ct: Vec<u8>,
    /// Guest-supplied, unverified; advisory only.
    pub suggested_filename: Option<String>,
}

/// A sealed drop ready for upload: the descriptor plus the STREAM ciphertext.
#[derive(Debug, Clone)]
pub struct SealedDrop {
    /// The unsigned descriptor.
    pub descriptor: DropDescriptor,
    /// The STREAM ciphertext bytes.
    pub ciphertext: Vec<u8>,
}

/// A drop awaiting review in the provisioning user's inbox.
#[derive(Debug, Clone)]
pub struct PendingDrop {
    /// The inbox row id.
    pub drop_id: DropId,
    /// The guest's descriptor.
    pub descriptor: DropDescriptor,
    /// The link it arrived through.
    pub via_link: UploadLinkId,
    /// Server-attested arrival time (RFC 3339, `received_at`).
    pub received_at: String,
}

/// Failure surfaced by the drop lifecycle.
#[derive(Debug, Error)]
pub enum DropError {
    /// The link is expired, revoked, or over a cap.
    #[error("upload link refused: {0}")]
    LinkRefused(&'static str),
    /// The KEM decapsulation or STREAM verification failed.
    #[error("drop crypto failure: {0}")]
    Crypto(&'static str),
    /// The drop was not found in the caller's inbox.
    #[error("pending drop not found")]
    NotFound,
}

/// Issues and revokes upload links on a trusted (native) client — the seam
/// `lifecycle::Workspace` will implement. Provisioning mints the Drop Key, wraps its
/// private half under the master key + OGK escrow, and registers the link record.
pub trait UploadLinkIssuer {
    /// Provision an upload link with `caps`; `passphrase` adds the server-verified
    /// Argon2id abuse gate (never transmitted — the record stores a verifier).
    fn create_link(
        &mut self,
        caps: LinkCaps,
        passphrase: Option<&str>,
    ) -> Result<UploadLink, DropError>;

    /// Revoke a link; the serve path refuses it within its fail-closed cache window.
    fn revoke_link(&mut self, link: UploadLinkId) -> Result<(), DropError>;
}

/// Reviews and adopts pending drops on a trusted (native) client — decapsulate `K`,
/// rewrap under the destination album's AMK, author the sidecar, sign the `create`
/// manifest with `key_mode = wrapped`, and submit the atomic inbox→album promotion.
pub trait DropAdopter {
    /// The provisioning user's pending drops.
    fn list_inbox(&self) -> Result<Vec<PendingDrop>, DropError>;

    /// Adopt a drop into `album_id` in place (no byte re-upload). Returns the signed
    /// adopting `create` manifest whose `ciphertext_hash` references the inbox blob.
    fn adopt(&mut self, drop: DropId, album_id: Uuid) -> Result<AssetManifest, DropError>;

    /// Discard a pending drop; its bytes are GC'd and the quota freed.
    fn discard(&mut self, drop: DropId) -> Result<(), DropError>;
}

/// Seal `plaintext` for a guest drop: draw a fresh random `K`, STREAM-encrypt under it,
/// and encapsulate `K` to `drop_pubkey` (the link's KEM public half, from the URL
/// fragment). Runs in the browser (WASM) and on native clients alike.
///
/// # Panics
/// Unimplemented skeleton (slice `S-A6`).
pub fn seal_drop(
    plaintext: &[u8],
    drop_pubkey: &[u8],
    content_type: &str,
) -> Result<SealedDrop, DropError> {
    let (_, _, _) = (plaintext, drop_pubkey, content_type);
    todo!("S-A6: drop sealing — see SLICES.md")
}

#[cfg(test)]
mod tests {
    /// `S-A6` acceptance: seal a plaintext to a Drop Key public half, decapsulate with
    /// the private half, STREAM-decrypt, assert byte equality; assert `kem_ct` length
    /// matches the suite; assert the descriptor round-trips through canonical CBOR.
    #[test]
    #[ignore = "S-A6 contract: drop seal round-trip not yet implemented"]
    fn drop_seal_round_trip() {
        unimplemented!("implemented by slice S-A6");
    }

    /// `S-A6` acceptance: adopt a sealed drop — decapsulate `K`, rewrap under a test AMK
    /// (`asset-keywrap/v1`), build the `create` manifest with `key_mode = wrapped`, and
    /// assert `verify_asset` accepts while a second member can unwrap and decrypt the
    /// unchanged ciphertext.
    #[test]
    #[ignore = "S-A6 contract: adoption rewrap not yet implemented"]
    fn adoption_rewrap_verifies_and_decrypts() {
        unimplemented!("implemented by slice S-A6");
    }

    /// `S-A6` acceptance: generated upload-link opaque ids are ≥128-bit CSPRNG values —
    /// never UUIDv7 or otherwise structured (identical rule to share links).
    #[test]
    #[ignore = "S-A6 contract: link issuance not yet implemented"]
    fn opaque_id_entropy() {
        unimplemented!("implemented by slice S-A6");
    }
}
