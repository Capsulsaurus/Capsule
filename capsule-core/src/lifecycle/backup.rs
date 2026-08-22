//! The portable backup artifact round-trip — the only `lifecycle` file that reaches
//! [`crate::backup`].

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use jiff::Timestamp;
use uuid::Uuid;

use super::{AssetState, LifecycleError, Result, Workspace, now_rfc3339};
use crate::backup::{self, BackupArtifact, BackupAsset, BackupInput, RestoreMode};
use crate::crypto::encryption::stream;
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::HybridVerifyingKey;
use crate::crypto::primitives::CRYPTO_SUITE_ID;
use crate::crypto::provenance::{AssetManifest, ProvenanceChain};
use crate::metadata::crdt::Lww;
use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};

/// Display name given to an album that arrives only as escrowed keys in a backup artifact — the
/// artifact carries AMKs, not album metadata, so there is no original name to restore.
const RECOVERED_ALBUM_NAME: &str = "Recovered album";

impl Workspace {
    /// The current provenance head hash for each managed asset (for backup reconciliation).
    pub fn local_heads(&self) -> BTreeMap<Uuid, Hash32> {
        self.assets
            .iter()
            .filter_map(|(id, a)| a.chain.head().map(|h| (*id, h)))
            .collect()
    }

    /// Export every managed asset to a portable backup artifact.
    #[tracing::instrument(skip_all, fields(out = %out.display()))]
    pub fn export_backup(&self, out: &Path, passphrase: &[u8]) -> Result<()> {
        let mut assets = Vec::new();
        let mut amks: BTreeMap<(Uuid, u32), [u8; 32]> = BTreeMap::new();

        for asset in self.assets.values() {
            let album = self.album(&asset.album_id)?;
            let head = &asset
                .chain
                .records()
                .last()
                .expect("provenance chain is never empty")
                .manifest;
            let plaintext =
                fs::read(self.media_path(asset)).map_err(|e| LifecycleError::Io(e.to_string()))?;
            let epoch = head.core.amk_version.0;
            let file_key = self.file_key(album, epoch, &head.core.file_id, &head.core.nonce_prefix);
            let (_, ciphertext) = stream::encrypt_asset_vec_with_prefix(
                &file_key,
                head.core.nonce_prefix,
                &plaintext,
            );
            amks.insert((asset.album_id, epoch), album.amks[&epoch]);
            // The asset's custody-receipt log, if the client has persisted one beside the chain.
            let receipts = fs::read(crate::library::receipts_path(
                &self.root,
                &asset.asset_id,
                Some(asset.capture_utc),
            ))
            .unwrap_or_default();
            assets.push(BackupAsset {
                album_id: asset.album_id,
                asset_id: asset.asset_id,
                // Export the exact sealed blob the manifest committed to (re-sealing would draw
                // a fresh nonce and break the `metadata_blob_hash` content address).
                metadata_blob: asset.metadata_blob.clone(),
                ciphertext,
                provenance: asset.chain.records().to_vec(),
                receipts,
            });
        }

        let input = BackupInput {
            assets,
            amks,
            exporter_device: self.account.device.device_id,
            source_library_version: "1".into(),
            export_timestamp: now_rfc3339(),
        };
        let bytes = backup::export(&input, passphrase, self.device_signer.as_ref())?;
        fs::write(out, &bytes).map_err(|e| LifecycleError::Io(e.to_string()))?;
        tracing::info!(bytes = bytes.len(), "backup: export complete");
        Ok(())
    }

    /// This device's signing public key (the exporter key a peer verifies a backup against).
    pub fn exporter_verifying_key(&self) -> HybridVerifyingKey {
        self.device_signer.verifying_key()
    }

    /// Open a backup artifact and restore (commit) its assets into this workspace, writing
    /// decrypted plaintext + provenance into the library. `exporter_pub` is the exporting
    /// device's signing key (resolved from the user's device directory). Returns the count
    /// of assets added.
    #[tracing::instrument(skip_all, fields(archive = %archive.display()))]
    pub fn import_backup(
        &mut self,
        archive: &Path,
        passphrase: &[u8],
        exporter_pub: &HybridVerifyingKey,
    ) -> Result<usize> {
        let bytes = fs::read(archive).map_err(|e| LifecycleError::Io(e.to_string()))?;
        let artifact = BackupArtifact::open(&bytes, passphrase, exporter_pub)?;
        let report = artifact.restore(RestoreMode::Commit, &self.local_heads())?;

        // Fold the artifact's escrowed AMKs into the durable keystore **before** writing any
        // asset: this is the step that makes a keyless library whole again, and an asset on disk
        // whose key was never persisted is exactly the failure `S-A10` exists to end. An album
        // already known keeps its write capability and simply gains missing epochs; an unknown
        // album lands read-only, since a backup escrows content keys but no signing capability.
        self.absorb_recovered_amks(&artifact)?;

        let mut added = 0;
        for restored in &report.applied {
            // Rebuild on-disk artifacts for the restored asset.
            let head = &restored
                .provenance
                .last()
                .expect("restored provenance is never empty")
                .manifest;
            // Keep `capture_utc` and the sidecar's `capture_timestamp` in agreement: the restored
            // sidecar is stamped with the head manifest's timestamp (see
            // `decode_restored_sidecar`), and a reopened workspace derives an asset's media
            // directory from that field. Using wall-clock "now" here would file the asset under a
            // month its own sidecar disagrees with.
            let capture_utc = head
                .core
                .timestamp
                .parse::<Timestamp>()
                .map_or_else(|_| Timestamp::now().as_second(), Timestamp::as_second);
            let mut chain = ProvenanceChain::new();
            for rec in &restored.provenance {
                chain
                    .append(rec.clone())
                    .map_err(|e| LifecycleError::Cbor(format!("restore chain: {e}")))?;
            }
            // Decode the sidecar from the (decrypted) metadata blob if present.
            let sidecar = self.decode_restored_sidecar(restored, head)?;
            let ext = "bin".to_string();
            let asset = AssetState {
                asset_id: restored.asset_id,
                album_id: restored.album_id,
                ext,
                capture_utc,
                chain,
                sidecar,
                // The artifact preserves the exact sealed blob the manifest committed to.
                metadata_blob: restored.metadata_blob.clone(),
                stack: None,
            };
            self.write_asset_files(&asset, &restored.plaintext)?;
            self.index_asset_row(&asset)?;
            self.index_original_representation(&asset, restored.plaintext.len())?;
            self.assets.insert(restored.asset_id, asset);
            added += 1;
        }
        tracing::info!(added, "backup: import complete");
        Ok(added)
    }

    /// Merge a verified artifact's escrowed `(album_id, epoch, amk)` rows into the durable album
    /// keystore, persist it, and re-apply it to the live workspace so the recovered keys are
    /// usable in this session as well as the next one.
    #[tracing::instrument(skip_all)]
    fn absorb_recovered_amks(&mut self, artifact: &BackupArtifact) -> Result<()> {
        let rows = artifact.amk_rows();
        if rows.is_empty() {
            return Ok(());
        }
        let mut by_album: BTreeMap<Uuid, Vec<(Uuid, u32, [u8; 32])>> = BTreeMap::new();
        for row in rows {
            by_album.entry(row.0).or_default().push(row);
        }

        let mut store = self.album_store_snapshot()?;
        let mut recovered_epochs = 0;
        for (album_id, rows) in by_album {
            let known = store.get(&album_id).is_some();
            let name = store
                .get(&album_id)
                .map_or_else(|| RECOVERED_ALBUM_NAME.to_string(), |a| a.name.clone());
            let added = store.merge_amks(album_id, &name, rows);
            recovered_epochs += added;
            tracing::info!(
                album_id = %album_id,
                known_album = known,
                epochs_added = added,
                "backup: escrowed album keys absorbed into the keystore"
            );
        }
        store.save(&self.root, &self.account.master)?;
        self.apply_album_store(&store);
        tracing::info!(
            recovered_epochs,
            "backup: album keystore updated from the artifact"
        );
        Ok(())
    }

    fn decode_restored_sidecar(
        &self,
        restored: &backup::artifact::RestoredAsset,
        head: &AssetManifest,
    ) -> Result<SidecarV1> {
        // Minimal sidecar reconstructed from the head manifest (the full encrypted metadata
        // blob is preserved verbatim in the artifact; decoding it requires the AMK, which we
        // hold). Here we synthesise a plaintext-equivalent sidecar for the local library.
        let mut sidecar = SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: CRYPTO_SUITE_ID,
            uuid: restored.asset_id,
            hash: hash::hash_bytes(&restored.plaintext),
            capture_timestamp: head.core.timestamp.clone(),
            import_timestamp: now_rfc3339(),
            content_type: "application/octet-stream".into(),
            dimensions: None,
            lqip: None,
            tags_user: Default::default(),
            tags_ai: Default::default(),
            caption: Default::default(),
            rating: Default::default(),
            stack_membership: Lww::new(),
            cull: Lww::new(),
            hidden: Lww::new(),
            camera_id: None,
            device_id: head.core.created_by_device,
            session_id: Uuid::now_v7(),
            gps: None,
            // Mirror the head manifest's prior (the sealing invariant for its action); the true
            // sidecar plaintext lives in the preserved `metadata_blob`, decodable by an album
            // member that holds the AMK.
            provenance_chain_hash: head.core.prior_provenance_hash,
            unknown: BTreeMap::new(),
            signature: None,
        };
        sidecar.sign(&self.account.user_ik);
        Ok(sidecar)
    }
}
