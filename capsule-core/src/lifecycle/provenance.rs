//! The append-only provenance chain: signing a lifecycle manifest, appending it under the
//! sealing order (re-signing + re-sealing the sidecar), and the `verify_asset` self-check.

use std::fs;

use uuid::Uuid;

use super::{AlbumKeys, LifecycleError, Result, Workspace, now_rfc3339};
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
use crate::sidecar::SidecarV1;

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
            // Each write records the exact client build that produced *this* record (S-D15), not
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
            self.authority(&asset.album_id)?,
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
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;
    use crate::crypto::keys::{Amk, HybridSigningKey};

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
        let album = ws.create_album("Trip").unwrap();
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

    /// Re-point `ws` at a different signing device — and optionally a different **account** —
    /// publishing a directory that holds it. What a second phone, or a shared album's member,
    /// looks like to everything below the signer.
    fn become_device(
        ws: &mut Workspace,
        account: Option<(Uuid, HybridSigningKey)>,
        device_id: Uuid,
        dsk: HybridSigningKey,
    ) {
        use crate::crypto::keys::{DeviceEntry, DirectoryCore};

        let entry = DeviceEntry {
            device_id,
            dsk_public: dsk.verifying_key(),
            dek_public: None,
            // Must precede any manifest it signs; the workspace stamps `now`.
            added_at: "2020-01-01T00:00:00Z".into(),
            revoked_at: None,
        };
        ws.directory = match account {
            // A second device of the *same* account: appended to the account's own directory,
            // which is re-signed by the account IK at a higher version.
            None => {
                let mut core = ws.directory.core.clone();
                core.directory_version += 1;
                core.devices.push(entry);
                core.sign(&ws.account.user_ik)
            }
            // A different account entirely: its own directory, under its own IK.
            Some((user_id, ref ik)) => {
                let directory = DirectoryCore {
                    user_id,
                    directory_version: 1,
                    updated_at: now_rfc3339(),
                    devices: vec![entry],
                }
                .sign(ik);
                ws.account.user_id = user_id;
                directory
            }
        };
        ws.account.device.device_id = device_id;
        ws.device_signer = Box::new(dsk);
    }

    fn imported(lib: &TempDir, src: &TempDir) -> (Workspace, Uuid, Uuid) {
        let img = src.path().join("photo.jpg");
        fs::write(&img, b"\xFF\xD8\xFF continuation-authorship bytes").unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        let asset = ws.import_asset(album, &img).unwrap();
        (ws, album, asset)
    }

    /// **A second device of the same account continues a chain, and the result verifies.**
    ///
    /// The case `sign_lifecycle` used to break outright: it inherited `created_by_user` and
    /// `created_by_device` from the chain head while signing with the *current* device, so a
    /// delete from device B claimed device A and failed `verify_asset` step 8 — the device
    /// signature does not verify under the named entry's key. Ordinary two-device use, no
    /// sharing required.
    #[test]
    fn a_continuation_from_a_second_device_names_it_and_verifies() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (mut ws, _album, asset) = imported(&lib, &src);

        let creator = ws.account.device.device_id;
        let second = Uuid::from_u128(0xD2);
        become_device(
            &mut ws,
            None,
            second,
            HybridSigningKey::from_seed_bytes(&[9; 32], &[10; 32]),
        );

        ws.soft_delete(&asset, 30).unwrap();

        let st = ws.asset(&asset).unwrap();
        let head = &st.chain.records().last().unwrap().manifest;
        assert_eq!(head.core.action, Action::Delete);
        assert_eq!(
            head.core.created_by_device, second,
            "the continuation names the device that signed it"
        );
        assert_ne!(
            head.core.created_by_device, creator,
            "and not the one that created the asset"
        );
        assert_eq!(
            ws.verify(&asset).unwrap(),
            VerifyOutcome::Accept,
            "which is the only reason it can verify at all"
        );

        // The creator is not lost — it is where the append-only chain keeps it.
        assert_eq!(
            st.chain.records()[0].manifest.core.created_by_device,
            creator
        );
        assert_eq!(st.chain.records()[0].manifest.core.action, Action::Create);
    }

    /// **A member of a shared album continues the owner's chain under the member's own account**,
    /// and it verifies against the *member's* directory.
    ///
    /// The write-tier signature is what carries album authority (step 10) and it is unaffected:
    /// the member holds the epoch's write-tier key, which is what membership *is*. Naming the
    /// acting member as the record's author therefore does not weaken the owner's album — it is
    /// the only way the record can be verified by anyone.
    #[test]
    fn a_members_continuation_verifies_under_the_members_own_directory() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let (mut ws, _album, asset) = imported(&lib, &src);

        let owner = ws.account.user_id;
        let member = Uuid::from_u128(0xB0B);
        become_device(
            &mut ws,
            Some((
                member,
                HybridSigningKey::from_seed_bytes(&[11; 32], &[12; 32]),
            )),
            Uuid::from_u128(0xD3),
            HybridSigningKey::from_seed_bytes(&[13; 32], &[14; 32]),
        );

        ws.soft_delete(&asset, 30).unwrap();

        let st = ws.asset(&asset).unwrap();
        let head = &st.chain.records().last().unwrap().manifest;
        assert_eq!(
            head.core.created_by_user, member,
            "a member's write is authored by the member"
        );
        assert_ne!(head.core.created_by_user, owner);
        assert_eq!(
            ws.verify(&asset).unwrap(),
            VerifyOutcome::Accept,
            "verified under the member's directory, against the owner's album authority"
        );
        assert_eq!(
            st.chain.records()[0].manifest.core.created_by_user,
            owner,
            "and the album's asset is still the owner's creation"
        );
    }
}
