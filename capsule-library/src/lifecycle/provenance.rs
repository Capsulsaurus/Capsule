//! The append-only provenance chain: signing a lifecycle manifest, appending it under the
//! sealing order (re-signing + re-sealing the sidecar), and the `verify_asset` self-check.

use std::fs;

use capsule_core::crypto::encryption::{
    blob_ciphertext_hash, blob_nonce, encrypt_asset_vec_with_prefix, seal_metadata_blob,
};
use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::keys::Amk;
use capsule_core::crypto::provenance::{Action, AssetManifest, ManifestCore, ProvenanceRecord};
use capsule_core::crypto::verify_asset::{
    MetadataBinding, RejectReason, VerifyOutcome, verify_asset, verify_metadata_binding,
};
use capsule_core::metadata::crdt::AddId;
use capsule_core::sidecar::SidecarV1;
use uuid::Uuid;

use super::{AlbumKeys, LifecycleError, Result, Workspace, now_rfc3339};

impl Workspace {
    /// Build a signed lifecycle manifest for `asset`, sharing the create manifest's content
    /// fields. Used for metadata-update / delete / trash-restore. `metadata_blob_hash` is set
    /// explicitly per the presence-by-action rule (`Some` for a metadata-update that seals a
    /// fresh blob, `None` for delete / trash-restore) rather than inherited from `base`.
    ///
    /// # Who a continuation names, and why it cannot be the creator
    ///
    /// `created_by_user` / `created_by_device` name the **signer of this record**, re-minted per
    /// write like `timestamp` and `client_version` — never inherited from `base`.
    ///
    /// That is not a preference, it is what
    /// [`verify_asset`] requires: it resolves
    /// `created_by_device` *inside `created_by_user`'s* published directory (step 6) and then
    /// verifies `device_sig` under **that entry's** key (step 8). A record naming a device that
    /// did not sign it fails step 8 and is unverifiable by every reader. Inheriting the pair
    /// therefore broke the ordinary two-device case — device B deleting an asset created on
    /// device A produced a manifest claiming A and signed by B — as well as every write by a
    /// shared album's member.
    ///
    /// Album authority is a separate check and is unaffected: step 10 verifies `write_sig`
    /// under the epoch's attested write-tier key, so naming the acting member as this record's
    /// author does not weaken the owner's album.
    ///
    /// The asset's original creator stays recoverable where it always was — the `create` record
    /// at the head of the provenance chain, which is append-only.
    fn sign_lifecycle(
        &self,
        album: &AlbumKeys,
        base: &ManifestCore,
        action: Action,
        prior: Option<Hash32>,
        retention_until: Option<String>,
        metadata_blob_hash: Option<Hash32>,
    ) -> Result<AssetManifest> {
        let core = ManifestCore {
            action,
            prior_provenance_hash: prior,
            retention_until,
            metadata_blob_hash,
            timestamp: now_rfc3339(),
            // This record's signer, not the asset's creator — see the doc comment above. The
            // same pair every create path writes (`import.rs`, `drops.rs`, `drop/mod.rs`), for
            // the same reason: it is the device whose DSK signs the bytes below.
            created_by_user: self.account.user_id,
            created_by_device: self.account.device.device_id,
            // Each write records the exact client build that produced *this* record, not
            // the creator's — so an edit by a different client identifies itself in the chain.
            client_version: self.client_version.clone(),
            ..base.clone()
        };
        Ok(core.sign(self.device_signer.as_ref(), album.write_tier_signer()?)?)
    }

    /// Run `verify_asset` for a managed asset (regenerating its ciphertext deterministically).
    pub fn verify(&self, asset_id: &Uuid) -> Result<VerifyOutcome> {
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
            .manifest;
        let plaintext =
            fs::read(self.media_path(asset)).map_err(|e| LifecycleError::Io(e.to_string()))?;
        // The head's `key_mode` / `wrapped_file_key` are read before they are verified, so a
        // tampered pair must not surface as an `Err`: a wrapped key that does not open (or a
        // `wrapped` head with none) yields no regenerated ciphertext, and `verify_asset` then
        // rejects it like any other content the signed manifest does not commit to. A missing
        // epoch key is local state, not tamper, and stays an error.
        let ciphertext = match self.original_file_key(album, &head.core) {
            Ok(file_key) => {
                Some(encrypt_asset_vec_with_prefix(&file_key, head.core.nonce_prefix, &plaintext).1)
            }
            Err(LifecycleError::Crypto(e)) => {
                tracing::warn!(
                    asset_id = %asset_id,
                    error = %e,
                    "the head's file key does not open; verifying it against no ciphertext"
                );
                None
            }
            Err(e) => return Err(e),
        };

        // Walk the whole chain forward; the head is what enters the trusted set.
        let prior = asset
            .chain
            .records()
            .len()
            .checked_sub(2)
            .map(|i| asset.chain.records()[i].record_hash());
        let outcome = verify_asset(
            head,
            ciphertext.as_deref().unwrap_or_default(),
            &self.directory,
            self.authority(&asset.album_id)?,
            prior,
        );
        // No STREAM ciphertext is empty, so an accept here is unreachable; refuse it anyway
        // rather than accept content that was never regenerated.
        Ok(match (ciphertext, outcome) {
            (None, VerifyOutcome::Accept) => {
                VerifyOutcome::TerminalReject(RejectReason::CiphertextHashMismatch)
            }
            (_, outcome) => outcome,
        })
    }

    pub(super) fn append_lifecycle(
        &mut self,
        asset_id: &Uuid,
        action: Action,
        retention_until: Option<String>,
        mutate_sidecar: impl FnOnce(&mut SidecarV1, AddId),
    ) -> Result<()> {
        let album_id = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?
            .album_id;
        // Sealing order (1): the prior head `H` is this asset's current chain head.
        let prior = self.assets[asset_id].chain.head();
        let base = self.assets[asset_id]
            .chain
            .records()
            .last()
            .expect("provenance chain is never empty")
            .manifest
            .core
            .clone();
        let binds = action.binds_metadata_blob();
        let epoch = base.amk_version.0;
        let album_amk = {
            let album = self.album(&album_id)?;
            Amk::from_bytes(album.amks[&epoch])
        };
        // Set by the metadata-bearing branch to the freshly derived blob key (nonce-folded),
        // for the binding self-check below.
        let mut sealed_blob_key: Option<[u8; 32]> = None;

        // Sealing order (2)+(3) for a metadata-bearing action: mutate + re-sign the sidecar with
        // `provenance_chain_hash = H`, then re-seal it under a fresh nonce folded into the blob
        // key (refusing to reuse the superseded nonce) and compute the fresh blob hash.
        // `delete` / `trash-restore` mint no new blob, so the sidecar and its stored blob are
        // left as the last metadata-bearing write produced them (their manifests commit to no
        // blob).
        let metadata_blob_hash = if binds {
            let add_id = self.counter.issue();
            let asset = self
                .assets
                .get_mut(asset_id)
                .expect("asset_id was validated above");
            // The nonce of the blob this update supersedes — refused for the fresh draw.
            let prior_nonce = blob_nonce(&asset.metadata_blob);
            mutate_sidecar(&mut asset.sidecar, add_id);
            asset.sidecar.provenance_chain_hash = prior;
            asset.sidecar.signature = None;
            asset.sidecar.sign(&self.account.user_ik);
            let (blob, blob_key) = seal_metadata_blob(
                &album_amk,
                asset_id,
                &asset.sidecar.to_canonical_vec(),
                prior_nonce,
            )?;
            let hash = blob_ciphertext_hash(&blob);
            asset.metadata_blob = blob;
            sealed_blob_key = Some(blob_key);
            Some(hash)
        } else {
            None
        };

        // Sealing order (4): build + sign the manifest with `prior_provenance_hash = H` and the
        // `metadata_blob_hash` from (3); append it as the new chain head.
        let album = self.album(&album_id)?;
        let manifest = self.sign_lifecycle(
            album,
            &base,
            action,
            prior,
            retention_until,
            metadata_blob_hash,
        )?;
        {
            let asset = self
                .assets
                .get_mut(asset_id)
                .expect("asset_id was validated above");
            asset
                .chain
                .append(ProvenanceRecord::new(*asset_id, manifest.clone(), prior))
                .map_err(|e| LifecycleError::Cbor(format!("chain: {e}")))?;
        }

        // Self-check the metadata↔manifest binding for a metadata-bearing write, enforcement on.
        if binds {
            let asset = &self.assets[asset_id];
            let binding = verify_metadata_binding(
                &manifest,
                &asset.metadata_blob,
                &sealed_blob_key.expect("the metadata-bearing branch set the blob key"),
                &asset.sidecar.to_canonical_vec(),
            );
            if binding != MetadataBinding::Bound {
                return Err(LifecycleError::MetadataUnbound(binding));
            }
        }

        // Re-borrow immutably to write the updated artifacts to disk. Only the signed
        // artifacts: a lifecycle write never changes the original, so it neither reads nor
        // rewrites it — a caption edit on a multi-gigabyte video touches the sidecar, the
        // chain, and the blob, and nothing else.
        let asset = self
            .assets
            .get(asset_id)
            .expect("asset_id was validated above");
        self.write_signed_artifacts(asset)?;
        self.index_asset_row(asset)
    }
}

#[cfg(test)]
mod tests;
