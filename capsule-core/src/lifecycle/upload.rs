//! The per-asset **upload bundle** — everything a network layer needs to push one managed
//! asset to a server, and nothing of the [`Workspace`] itself (slice `S-D18`).
//!
//! Clients store plaintext locally; the ciphertext that crosses a boundary is re-derived on
//! demand from the manifest's recorded `nonce_prefix` (the same trick
//! [`export_backup`](Workspace::export_backup) has always used, and which now runs through
//! this one accessor so there is exactly one copy of that crypto).
//!
//! **This is a post-import pass, not an implementation of
//! [`AssetUploader`](crate::import::AssetUploader).** That trait cannot be
//! implemented: `stream_candidate` holds `&mut Workspace` across the `uploader.upload(...)`
//! call, so an implementor can never borrow the workspace back to read the bytes it is
//! supposed to send. Do not try again — push reads a *committed* asset out of an
//! immutably-borrowed workspace, which is why it is a second command
//! (`capsule push`) rather than a flag on import.

use std::fs;

use uuid::Uuid;

use super::{AssetState, LifecycleError, Result, Workspace, media_dir};
use crate::cbor;
use crate::crypto::encryption::stream;
use crate::crypto::hash::{self, Hash32};
use crate::crypto::provenance::DerivativeManifest;
use crate::crypto::provenance::action::{Action, DerivativeRole};
use crate::crypto::provenance::manifest::KeyMode;

/// One derivative blob of an asset bundle: the bytes plus the content address its signed
/// [`DerivativeManifest`] committed to.
#[derive(Debug, Clone)]
pub struct DerivativeBlob {
    /// Which derivative this is (`thumbnail` / `preview` / `embedding`).
    pub role: DerivativeRole,
    /// The derivative's MIME/format string, e.g. `image/avif`.
    pub format: String,
    /// The AMK epoch the derivative manifest was authorized under, when it recorded one.
    pub amk_version: Option<u32>,
    /// The derivative's transferable bytes.
    pub bytes: Vec<u8>,
    /// The content address the signed derivative manifest committed to.
    pub ciphertext_hash: Hash32,
}

/// Everything needed to push one managed asset: its identity, the re-derived original
/// ciphertext, the exact sealed metadata blob, any derivative blobs, and the manifest-envelope
/// projection fields the server validates. Carries no [`Workspace`] internals and no key
/// material — the AMK never leaves the workspace.
///
/// Every field maps one-for-one onto the upload protocol's `POST /upload` body
/// (`capsule_sdk::push` owns that mapping).
#[derive(Debug, Clone)]
pub struct UploadBundle {
    /// The asset id — equal to the manifest's `file_id`.
    pub asset_id: Uuid,
    /// The album the asset belongs to.
    pub album_id: Uuid,
    /// The primitive bundle the manifest was produced under.
    pub crypto_suite_id: u16,
    /// The date-based wire protocol version the manifest is pinned to.
    pub protocol_version: String,
    /// The AMK epoch the asset is sealed under.
    pub amk_version: u32,
    /// The original's ciphertext, re-derived from the manifest's recorded `nonce_prefix`.
    pub ciphertext: Vec<u8>,
    /// The original ciphertext's content address (equal to the manifest's `ciphertext_hash`).
    pub ciphertext_hash: Hash32,
    /// Total plaintext byte length of the original.
    pub plaintext_size: u64,
    /// Plaintext bytes per STREAM chunk.
    pub chunk_size: u32,
    /// The original's MIME type, from the signed sidecar.
    pub content_type: String,
    /// How the file key is obtained (`derived` / `wrapped`).
    pub key_mode: KeyMode,
    /// The **exact** sealed metadata-blob wire bytes the manifest commits to.
    pub metadata_blob: Vec<u8>,
    /// The content address of the sealed metadata blob, when the head action binds one.
    pub metadata_blob_hash: Option<Hash32>,
    /// The asset's derivative blobs, if any were generated and persisted.
    pub derivatives: Vec<DerivativeBlob>,
    /// The authoring user.
    pub created_by_user: Uuid,
    /// The authoring device.
    pub created_by_device: Uuid,
    /// The exact client build that authored the head manifest.
    pub client_version: String,
    /// RFC3339 authoring timestamp of the head manifest.
    pub timestamp: String,
    /// The head manifest's lifecycle action.
    pub action: Action,
    /// The head manifest's prior-provenance link (null iff `action = create`).
    pub prior_provenance_hash: Option<Hash32>,
    /// The head manifest's retention floor (set only for `delete`).
    pub retention_until: Option<String>,
}

impl UploadBundle {
    /// The original ciphertext's size in bytes — what `POST /upload` declares as `size` for
    /// the original blob.
    #[must_use]
    pub fn ciphertext_size(&self) -> u64 {
        self.ciphertext.len() as u64
    }
}

impl Workspace {
    /// Build the [`UploadBundle`] for one managed asset.
    ///
    /// Re-derives the original ciphertext from the head manifest's recorded `nonce_prefix`
    /// (clients hold plaintext; ciphertext is regenerated deterministically) and gates it on
    /// the manifest's own content address, so a bundle that reaches the network is one the
    /// signed manifest vouches for. The sealed metadata blob is carried verbatim — it cannot
    /// be regenerated, because `seal_metadata_blob` draws a fresh nonce per call.
    #[tracing::instrument(skip(self), fields(asset_id = %asset_id))]
    pub fn upload_bundle(&self, asset_id: &Uuid) -> Result<UploadBundle> {
        let asset = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?;
        let album = self.album(&asset.album_id)?;
        let head = &asset
            .chain
            .records()
            .last()
            .expect("provenance chain is never empty")
            .manifest
            .core;

        let plaintext =
            fs::read(self.media_path(asset)).map_err(|e| LifecycleError::Io(e.to_string()))?;
        let epoch = head.amk_version.0;
        let file_key = self.file_key(album, epoch, &head.file_id, &head.nonce_prefix);
        let (_, ciphertext) =
            stream::encrypt_asset_vec_with_prefix(&file_key, head.nonce_prefix, &plaintext);

        // The re-derivation is deterministic, so a mismatch means the library's bytes and its
        // own signed manifest disagree — never ship that.
        let ciphertext_hash = hash::hash_bytes(&ciphertext);
        if ciphertext_hash != head.ciphertext_hash {
            tracing::error!(
                asset_id = %asset.asset_id,
                expected = %head.ciphertext_hash.to_hex(),
                actual = %ciphertext_hash.to_hex(),
                "upload bundle: re-derived ciphertext does not match the manifest"
            );
            return Err(LifecycleError::CiphertextMismatch(asset.asset_id));
        }

        let derivatives = self.derivative_blobs(asset);
        tracing::debug!(
            album_id = %asset.album_id,
            amk_version = epoch,
            ciphertext_bytes = ciphertext.len(),
            metadata_blob_bytes = asset.metadata_blob.len(),
            derivatives = derivatives.len(),
            action = ?head.action,
            "upload bundle built"
        );

        Ok(UploadBundle {
            asset_id: asset.asset_id,
            album_id: asset.album_id,
            crypto_suite_id: head.crypto_suite_id,
            protocol_version: head.protocol_version.clone(),
            amk_version: epoch,
            ciphertext,
            ciphertext_hash,
            plaintext_size: head.plaintext_size,
            chunk_size: head.chunk_size,
            content_type: asset.sidecar.content_type.clone(),
            key_mode: head.key_mode,
            metadata_blob: asset.metadata_blob.clone(),
            metadata_blob_hash: head.metadata_blob_hash,
            derivatives,
            created_by_user: head.created_by_user,
            created_by_device: head.created_by_device,
            client_version: head.client_version.clone(),
            timestamp: head.timestamp.clone(),
            action: head.action,
            prior_provenance_hash: head.prior_provenance_hash,
            retention_until: head.retention_until.clone(),
        })
    }

    /// The asset's persisted derivative blobs, read back from
    /// `media/{YYYY}/{YYYY-MM}/derivatives/`. A derivative whose bytes are missing or no
    /// longer content-address to its signed manifest is **skipped with a warning** rather
    /// than failing the bundle: the original and its metadata are what a backup must not
    /// lose, and a stale thumbnail is regenerable.
    fn derivative_blobs(&self, asset: &AssetState) -> Vec<DerivativeBlob> {
        let dir = media_dir(&self.root, asset.capture_utc).join("derivatives");
        let stem = asset.asset_id.simple().to_string();
        let bundle_path = dir.join(format!("{stem}.derivatives.cbor"));
        let Ok(bytes) = fs::read(&bundle_path) else {
            return Vec::new();
        };
        let manifests: Vec<DerivativeManifest> = match cbor::from_slice(&bytes) {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(
                    path = %bundle_path.display(),
                    error = %e,
                    "upload bundle: undecodable derivative manifest bundle; skipping derivatives"
                );
                return Vec::new();
            }
        };

        let mut blobs = Vec::with_capacity(manifests.len());
        for manifest in manifests {
            let core = manifest.core;
            let role_name = derivative_role_name(core.role);
            let prefix = format!("{stem}.{role_name}.");
            let Some(bytes) = read_derivative_bytes(&dir, &prefix) else {
                tracing::warn!(
                    asset_id = %asset.asset_id,
                    role = role_name,
                    "upload bundle: derivative manifest has no bytes on disk; skipping"
                );
                continue;
            };
            let observed = hash::hash_bytes(&bytes);
            if observed != core.ciphertext_hash {
                tracing::warn!(
                    asset_id = %asset.asset_id,
                    role = role_name,
                    "upload bundle: derivative bytes do not match their signed manifest; skipping"
                );
                continue;
            }
            blobs.push(DerivativeBlob {
                role: core.role,
                format: core.format,
                amk_version: core.amk_version.map(|v| v.0),
                bytes,
                ciphertext_hash: observed,
            });
        }
        blobs
    }
}

/// The on-disk / wire name of a derivative role (mirrors `persist_derivatives`' file naming).
fn derivative_role_name(role: DerivativeRole) -> &'static str {
    match role {
        DerivativeRole::Thumbnail => "thumbnail",
        DerivativeRole::Preview => "preview",
        DerivativeRole::Embedding => "embedding",
    }
}

/// The first file in `dir` whose name starts with `prefix` — the derivative's bytes, whose
/// extension varies with the encoder's chosen format.
fn read_derivative_bytes(dir: &std::path::Path, prefix: &str) -> Option<Vec<u8>> {
    let entries = fs::read_dir(dir).ok()?;
    let mut names: Vec<_> = entries
        .filter_map(std::result::Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(prefix))
        .collect();
    names.sort();
    fs::read(dir.join(names.first()?)).ok()
}

#[cfg(test)]
mod tests;
