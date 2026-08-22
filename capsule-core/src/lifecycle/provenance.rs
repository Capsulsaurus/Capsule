//! The append-only provenance chain: signing a lifecycle manifest, appending it under the
//! sealing order (re-signing + re-sealing the sidecar), and the `verify_asset` self-check.

use std::fs;

use uuid::Uuid;

use super::{AlbumKeys, LifecycleError, Result, Workspace, now_rfc3339};
use crate::crypto::CryptoError;
use crate::crypto::encryption::{blob_ciphertext_hash, blob_nonce, seal_metadata_blob, stream};
use crate::crypto::hash::Hash32;
use crate::crypto::keys::Amk;
use crate::crypto::provenance::action::Action;
use crate::crypto::provenance::manifest::ManifestCore;
use crate::crypto::provenance::{AssetManifest, ProvenanceRecord};
use crate::crypto::verify_asset::{
    MetadataBinding, VerifyOutcome, verify_asset, verify_metadata_binding,
};
use crate::metadata::crdt::AddId;
use crate::sidecar::sidecar_v1::SidecarV1;

impl Workspace {
    /// Build a signed lifecycle manifest for `asset`, sharing the create manifest's content
    /// fields. Used for metadata-update / delete / trash-restore. `metadata_blob_hash` is set
    /// explicitly per the presence-by-action rule (`Some` for a metadata-update that seals a
    /// fresh blob, `None` for delete / trash-restore) rather than inherited from `base`.
    fn sign_lifecycle(
        &self,
        album: &AlbumKeys,
        base: &ManifestCore,
        action: Action,
        prior: Option<Hash32>,
        retention_until: Option<String>,
        metadata_blob_hash: Option<Hash32>,
    ) -> std::result::Result<AssetManifest, CryptoError> {
        let core = ManifestCore {
            action,
            prior_provenance_hash: prior,
            retention_until,
            metadata_blob_hash,
            timestamp: now_rfc3339(),
            // Each write records the exact client build that produced *this* record (S-D15), not
            // the creator's — so an edit by a different client identifies itself in the chain.
            client_version: self.client_version.clone(),
            ..base.clone()
        };
        core.sign(self.device_signer.as_ref(), &album.write_tier)
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
        let file_key = self.file_key(
            album,
            head.core.amk_version.0,
            &head.core.file_id,
            &head.core.nonce_prefix,
        );
        let (_, ciphertext) =
            stream::encrypt_asset_vec_with_prefix(&file_key, head.core.nonce_prefix, &plaintext);

        // Walk the whole chain forward; the head is what enters the trusted set.
        let prior = asset
            .chain
            .records()
            .len()
            .checked_sub(2)
            .map(|i| asset.chain.records()[i].record_hash());
        Ok(verify_asset(
            head,
            &ciphertext,
            &self.directory,
            &self.authorities[&asset.album_id],
            prior,
        ))
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
                .append(ProvenanceRecord {
                    asset_id: *asset_id,
                    manifest: manifest.clone(),
                    prior_provenance_hash: prior,
                })
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

        // Re-borrow immutably to write the updated artifacts to disk.
        let asset = self
            .assets
            .get(asset_id)
            .expect("asset_id was validated above");
        let plaintext =
            fs::read(self.media_path(asset)).map_err(|e| LifecycleError::Io(e.to_string()))?;
        self.write_asset_files(asset, &plaintext)?;
        self.index_asset_row(asset)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;
    use crate::crypto::keys::Amk;

    /// S-A3: the `Workspace` populates `metadata_blob_hash` per the sealing order, the sidecar
    /// binds to the manifest through the prior head, and a one-byte sidecar mutation quarantines.
    #[test]
    fn metadata_binding_populated_and_enforced() {
        use crate::crypto::verify_asset::{
            BindingReject, MetadataBinding, verify_metadata_binding,
        };

        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF metadata-binding bytes").unwrap();

        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip");
        let asset = ws.import_asset(album, &img).unwrap();

        // The create manifest commits to a metadata blob; its sidecar references no prior head,
        // and that absence equals the manifest's `prior_provenance_hash` (both `None` on create).
        let st = ws.asset(&asset).unwrap();
        let head = &st.chain.records().last().unwrap().manifest;
        let epoch = head.core.amk_version.0;
        assert!(
            head.core.metadata_blob_hash.is_some(),
            "create must bind a metadata blob"
        );
        assert_eq!(st.sidecar.provenance_chain_hash, None);
        assert_eq!(
            head.core.prior_provenance_hash,
            st.sidecar.provenance_chain_hash
        );

        // The stored blob round-trips to the signed sidecar under the asset's blob key,
        // re-derived from the blob's own (folded) nonce.
        let blob_key = Amk::from_bytes(ws.album(&album).unwrap().amks[&epoch])
            .derive_blob_key(&asset, &blob_nonce(&st.metadata_blob).unwrap());
        assert_eq!(
            verify_metadata_binding(
                head,
                &st.metadata_blob,
                &blob_key,
                &st.sidecar.to_canonical_vec()
            ),
            MetadataBinding::Bound
        );
        // A one-byte mutation of the local sidecar quarantines (surfaced, never persisted).
        let mut tampered = st.sidecar.to_canonical_vec();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x01;
        assert_eq!(
            verify_metadata_binding(head, &st.metadata_blob, &blob_key, &tampered),
            MetadataBinding::Quarantine(BindingReject::SidecarMismatch)
        );

        // A metadata-update re-binds: the sidecar references the PRIOR head (the create record),
        // equal to the update manifest's `prior_provenance_hash`.
        let create_head = ws.asset(&asset).unwrap().chain.records()[0].record_hash();
        ws.tag_add(&asset, "vacation").unwrap();
        let st = ws.asset(&asset).unwrap();
        let update = &st.chain.records().last().unwrap().manifest;
        assert!(update.core.metadata_blob_hash.is_some());
        assert_eq!(st.sidecar.provenance_chain_hash, Some(create_head));
        assert_eq!(
            update.core.prior_provenance_hash,
            st.sidecar.provenance_chain_hash
        );
        // The re-seal drew a fresh nonce folded into a new blob key — re-derive from the
        // updated blob's own nonce (the create-era `blob_key` no longer opens it).
        let update_blob_key = Amk::from_bytes(ws.album(&album).unwrap().amks[&epoch])
            .derive_blob_key(&asset, &blob_nonce(&st.metadata_blob).unwrap());
        assert_ne!(
            update_blob_key, blob_key,
            "the metadata-update re-rolled the blob key"
        );
        assert_eq!(
            verify_metadata_binding(
                update,
                &st.metadata_blob,
                &update_blob_key,
                &st.sidecar.to_canonical_vec()
            ),
            MetadataBinding::Bound
        );

        // A delete mints no metadata blob: the head manifest commits to none, and that is
        // structurally valid under the presence-by-action rule.
        ws.soft_delete(&asset, 30).unwrap();
        let st = ws.asset(&asset).unwrap();
        let del = &st.chain.records().last().unwrap().manifest;
        assert!(
            del.core.metadata_blob_hash.is_none(),
            "delete binds no metadata blob"
        );
        assert!(del.structural_ok());
    }
}
