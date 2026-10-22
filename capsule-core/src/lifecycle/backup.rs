//! The portable backup artifact round-trip — the only `lifecycle` file that reaches
//! [`crate::backup`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use jiff::Timestamp;
use uuid::Uuid;

use super::album::{read_doc_file, stack_doc_path};
use super::{AssetState, LifecycleError, Result, Workspace, now_rfc3339};
use crate::backup::{
    self, BackupArtifact, BackupAsset, BackupDoc, BackupDocKind, BackupInput, RestoreMode,
    VerifyOutcome as RecoveryVerdict,
};
use crate::crypto::authority::Authority;
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::albumstore::{AlbumStore, AmkRow, PersistedAuthority};
use crate::crypto::keys::{Amk, HybridVerifyingKey};
#[cfg(any(test, feature = "test-support"))]
use crate::crypto::primitives::Argon2Params;
use crate::crypto::primitives::{CRYPTO_SUITE_ID, DeviceTier};
use crate::crypto::provenance::{AssetManifest, ProvenanceChain};
use crate::crypto::pwkdf::WrappedSecret;
use crate::metadata::crdt::Lww;
use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};
use crate::sidecar::{ALBUM_DOC_SCHEMA_V1, AlbumDocV1, STACK_DOC_SCHEMA_V1, StackDocV1};

/// Display name given to an album that arrives only as escrowed keys in a backup artifact. An
/// artifact that carries the album's label record replaces it with the album's own name once
/// that record is merged; only an artifact exported before the records existed leaves it.
const RECOVERED_ALBUM_NAME: &str = "Recovered album";

impl Workspace {
    /// Wrap this account's master key under `recovery_secret` into the escrow blob a client
    /// stores server-side (`PUT /backup/escrow`, slice `S-C12`; driven from the SDK by
    /// `capsule_sdk::recovery`).
    ///
    /// The master key **never leaves this workspace**: the raw bytes are handed straight to
    /// [`backup::escrow_master_key`] and what comes back out is the passphrase-wrapped blob.
    /// That is the whole reason this verb lives here rather than a `master_key_bytes()`
    /// accessor — an FFI surface that could read the master key would be a far larger hazard
    /// than one that can only mint an escrow of it.
    ///
    /// `recovery_secret` is the ≥128-bit secret shown to the user exactly once
    /// (`capsule_sdk::recovery::MintedSecret`); `tier` selects the Argon2id cost, which is
    /// recorded in the blob so any device can unwrap it.
    #[tracing::instrument(skip_all, fields(?tier))]
    pub fn escrow_master_key(
        &self,
        recovery_secret: &[u8],
        tier: DeviceTier,
    ) -> Result<WrappedSecret> {
        let blob =
            backup::escrow_master_key(self.account.master.as_bytes(), recovery_secret, tier)?;
        tracing::info!("minted a master-key escrow blob under a fresh recovery secret");
        Ok(blob)
    }

    /// This account's user identity key sealed under its master key — the blob a new device
    /// passes to [`create_from_master`](Self::create_from_master) beside the recovered master
    /// key, so it opens as the same account under the same identity the server has pinned.
    ///
    /// Like [`escrow_master_key`](Self::escrow_master_key), the private key never leaves the
    /// workspace in the clear: only the holder of the master key can open what this returns.
    pub fn sealed_identity(&self) -> Vec<u8> {
        tracing::debug!("sealed the user identity key under the master key");
        self.account.sealed_identity()
    }

    /// Local, network-free check that `recovery_secret` still opens `blob` **to this device's
    /// master key** — the derived-tag compare that backs the recovery verification cadence
    /// (`S-D12`).
    ///
    /// Delegates wholesale to [`backup::verify_recovery_secret`]; nothing is compared here, and
    /// no key bytes are surfaced either way. The stale-cache rule (refresh the blob once before
    /// recording a genuine failure) belongs to the networked caller, not to this predicate.
    pub fn verify_escrow(&self, blob: &WrappedSecret, recovery_secret: &[u8]) -> RecoveryVerdict {
        backup::verify_recovery_secret(blob, recovery_secret, self.account.master.as_bytes())
    }

    /// The current provenance head hash for each managed asset (for backup reconciliation).
    pub fn local_heads(&self) -> BTreeMap<Uuid, Hash32> {
        self.assets
            .iter()
            .filter_map(|(id, a)| a.chain.head().map(|h| (*id, h)))
            .collect()
    }

    /// Export every managed asset to a portable backup artifact.
    ///
    /// The AMK ledger is wrapped at the production Argon2id cost
    /// ([`backup::WRAP_PARAMS`]) and there is no way to ask for another: this method takes no
    /// cost argument and [`backup::export`] takes none either, so no caller reachable from a
    /// production build can write a brute-forceable artifact.
    #[tracing::instrument(skip_all, fields(out = %out.display()))]
    pub fn export_backup(&self, out: &Path, passphrase: &[u8]) -> Result<()> {
        let input = self.backup_input()?;
        let bytes = backup::export(&input, passphrase, self.device_signer.as_ref())?;
        self.write_artifact(out, &bytes)
    }

    /// As [`export_backup`](Self::export_backup) but with an explicit Argon2id cost for the
    /// artifact's wrap key.
    ///
    /// The cost is recorded in the artifact, so [`import_backup`](Self::import_backup)
    /// reproduces the wrap key from what it reads rather than from a constant: exporting cheaply
    /// makes *both* legs of a backup round trip cheap, and no import-side entry point is needed.
    ///
    /// **Not in a production build.** Compiled only under `cfg(test)` or the non-default
    /// `test-support` feature, because a weak cost here is a brute-forceable backup and nothing
    /// downstream re-checks it — the same reason
    /// [`escrow_master_key`](Self::escrow_master_key) takes a closed `DeviceTier` rather than
    /// raw parameters.
    #[cfg(any(test, feature = "test-support"))]
    #[tracing::instrument(skip_all, fields(out = %out.display(), ?params))]
    pub fn export_backup_with_params(
        &self,
        out: &Path,
        passphrase: &[u8],
        params: Argon2Params,
    ) -> Result<()> {
        let input = self.backup_input()?;
        let bytes =
            backup::export_with_params(&input, passphrase, params, self.device_signer.as_ref())?;
        self.write_artifact(out, &bytes)
    }

    /// Everything the artifact needs about this library, collected once so both export entry
    /// points read the same state rather than each walking the library themselves.
    fn backup_input(&self) -> Result<BackupInput> {
        let mut assets = Vec::new();
        let mut amks: BTreeMap<(Uuid, u32), [u8; 32]> = BTreeMap::new();

        for asset in self.assets.values() {
            // The per-asset re-encrypt (plaintext → the ciphertext the manifest's recorded
            // nonce prefix pins) is the `upload_bundle` accessor's job — one copy of that
            // crypto, shared with `capsule push` (S-D18). It also self-checks the re-derived
            // ciphertext against the manifest's content address, which this loop never did.
            let bundle = self.upload_bundle(&asset.asset_id)?;
            let album = self.album(&asset.album_id)?;
            amks.insert(
                (asset.album_id, bundle.amk_version),
                album.amks[&bundle.amk_version],
            );
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
                metadata_blob: bundle.metadata_blob,
                // The artifact format holds each ciphertext whole, so the original is
                // materialised here; a push streams it instead.
                ciphertext: bundle
                    .original
                    .to_vec()
                    .map_err(|e| LifecycleError::Io(e.to_string()))?,
                provenance: asset.chain.records().to_vec(),
                receipts,
            });
        }

        // Each exported album's public authority. Only the reference ledger travels: an MLS
        // authority's exported state holds the group's private secrets, which a backup must not
        // escrow, so such an album exports unverifiable until its ledger has a public form.
        let mut authorities = BTreeMap::new();
        for album_id in amks.keys().map(|(album, _)| *album) {
            match self.authorities.get(&album_id) {
                Some(Authority::Reference(a)) => {
                    authorities.insert(album_id, a.to_ledger());
                }
                Some(_) => tracing::warn!(
                    album_id = %album_id,
                    "backup: album authority is not a reference ledger; exported without it"
                ),
                None => tracing::debug!(
                    album_id = %album_id,
                    "backup: album holds no authority; exported without one"
                ),
            }
        }

        let docs = self.backup_docs(&mut amks)?;

        Ok(BackupInput {
            assets,
            amks,
            authorities,
            docs,
            exporter_device: self.account.device.device_id,
            source_library_version: "1".into(),
            export_timestamp: now_rfc3339(),
        })
    }

    /// The label records that ride the artifact: every exported album's record (so a restore
    /// recovers the album's name and labels) and the record of every stack an exported asset
    /// belongs to that has one. Each is sealed under its album's current-epoch AMK, which is
    /// added to `amks` so the ledger escrows it.
    ///
    /// A record that cannot be read, verified or sealed is left out with a warning rather than
    /// failing the export, mirroring [`absorb_recovered_docs`](Self::absorb_recovered_docs): the
    /// assets and keys are what a backup must carry, and a missing record costs a name or a
    /// label, never data. An album left out restores as `Recovered album`.
    fn backup_docs(&self, amks: &mut BTreeMap<(Uuid, u32), [u8; 32]>) -> Result<Vec<BackupDoc>> {
        let mut docs = Vec::new();
        let mut skipped = 0usize;
        let current_amk = |album_id: Uuid, amks: &mut BTreeMap<(Uuid, u32), [u8; 32]>| {
            let album = self.album(&album_id)?;
            let amk = album.amks[&album.current_epoch];
            amks.insert((album_id, album.current_epoch), amk);
            Ok::<_, LifecycleError>((album.current_epoch, Amk::from_bytes(amk)))
        };

        let albums: BTreeSet<Uuid> = amks.keys().map(|(album, _)| *album).collect();
        for album_id in albums {
            let sealed = self.album_doc(&album_id).and_then(|doc| {
                let (epoch, amk) = current_amk(album_id, amks)?;
                Ok((epoch, doc.seal(&amk)?))
            });
            match sealed {
                Ok((epoch, sealed)) => docs.push(BackupDoc {
                    kind: BackupDocKind::Album,
                    doc_id: album_id,
                    album_id,
                    amk_version: epoch,
                    sealed,
                }),
                Err(e) => {
                    skipped += 1;
                    tracing::warn!(
                        album_id = %album_id,
                        error = %e,
                        "backup: album label record left out of the export"
                    );
                }
            }
        }

        let stacks: BTreeSet<Uuid> = self
            .assets
            .values()
            .filter_map(|a| a.sidecar.stack_membership.get().cloned().flatten())
            .map(|m| m.stack_id)
            .collect();
        for stack_id in stacks {
            let sealed = read_doc_file(&stack_doc_path(&self.root, &stack_id)).and_then(|file| {
                if file.is_none() {
                    return Ok(None);
                }
                let doc = self.stack_doc(stack_id)?;
                let (epoch, amk) = current_amk(doc.album_id, amks)?;
                Ok(Some((doc.album_id, epoch, doc.seal(&amk)?)))
            });
            match sealed {
                Ok(None) => {}
                Ok(Some((album_id, epoch, sealed))) => docs.push(BackupDoc {
                    kind: BackupDocKind::Stack,
                    doc_id: stack_id,
                    album_id,
                    amk_version: epoch,
                    sealed,
                }),
                Err(e) => {
                    skipped += 1;
                    tracing::warn!(
                        stack_id = %stack_id,
                        error = %e,
                        "backup: stack label record left out of the export"
                    );
                }
            }
        }
        tracing::debug!(
            docs = docs.len(),
            skipped,
            "backup: label records sealed for export"
        );
        Ok(docs)
    }

    /// Open every label record a verified artifact carries with the AMK its ledger escrows, and
    /// merge it into this library: an album's record brings back its name (and labels), so an
    /// album new to this library is named as it was rather than `Recovered album`.
    ///
    /// The records are trusted on the artifact's own authentication — the manifest HMAC and the
    /// exporter's signature cover each entry — and are re-signed here under this account's
    /// identity key as they are merged. A record that does not open is skipped with a warning:
    /// the assets and keys it travelled with are already restored, and its absence costs a
    /// name, never data.
    #[tracing::instrument(skip_all)]
    fn absorb_recovered_docs(&mut self, artifact: &BackupArtifact) -> Result<()> {
        let mut merged = 0usize;
        for doc in artifact.docs() {
            let Some(amk) = artifact.amk(&doc.album_id, doc.amk_version) else {
                continue;
            };
            let amk = Amk::from_bytes(amk);
            let outcome = match doc.kind {
                BackupDocKind::Album => {
                    AlbumDocV1::open_sealed(&doc.sealed, &amk, &doc.doc_id, ALBUM_DOC_SCHEMA_V1)
                        .map_err(LifecycleError::from)
                        .and_then(|remote| self.merge_album_doc(&remote))
                }
                BackupDocKind::Stack => {
                    StackDocV1::open_sealed(&doc.sealed, &amk, &doc.doc_id, STACK_DOC_SCHEMA_V1)
                        .map_err(LifecycleError::from)
                        .and_then(|remote| self.merge_stack_doc(&remote))
                }
            };
            match outcome {
                Ok(()) => merged += 1,
                Err(e) => tracing::warn!(
                    kind = ?doc.kind,
                    doc_id = %doc.doc_id,
                    error = %e,
                    "backup: label record not restored"
                ),
            }
        }
        tracing::info!(
            merged,
            carried = artifact.docs().len(),
            "backup: label records restored"
        );
        Ok(())
    }

    /// Write an assembled artifact to `out`.
    fn write_artifact(&self, out: &Path, bytes: &[u8]) -> Result<()> {
        fs::write(out, bytes).map_err(|e| LifecycleError::Io(e.to_string()))?;
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
        // already known keeps its write capability and authority and simply gains missing epochs;
        // an unknown album lands read-only with the public authority the artifact carried, since
        // a backup escrows content keys and the admin-signed ledger but no signing capability.
        self.absorb_recovered_amks(&artifact)?;
        // Then the albums' and stacks' label records, which need those keys to open: this is
        // what names a recovered album.
        self.absorb_recovered_docs(&artifact)?;

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

    /// Merge a verified artifact's escrowed `(album_id, epoch, amk)` rows — and each album's
    /// public authority ledger — into the durable album keystore, persist it, and re-apply it to
    /// the live workspace so the recovered keys are usable in this session as well as the next.
    ///
    /// A ledger is adopted only by an album holding no authority ([`AlbumStore::merge_amks`]),
    /// and an adopted ledger must re-verify against the epochs now held before anything is
    /// persisted: an authority that would be dropped at the next open is refused here instead.
    /// An authority the album already held is not re-checked — see [`merge_album`].
    ///
    /// [`AlbumStore::merge_amks`]: crate::crypto::keys::albumstore::AlbumStore::merge_amks
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
            let carried = artifact
                .authority_ledgers()
                .get(&album_id)
                .map(|ledger| PersistedAuthority::Reference(Box::new(ledger.clone())));
            let carried_authority = carried.is_some();
            let (added, authority_adopted) =
                merge_album(&mut store, album_id, &name, rows, carried)?;
            recovered_epochs += added;
            tracing::info!(
                album_id = %album_id,
                known_album = known,
                epochs_added = added,
                carried_authority,
                authority_adopted,
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
        restored: &backup::RestoredAsset,
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
            regions: Default::default(),
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

/// Merge one album's escrowed AMK `rows` and the authority the artifact `carried` into `store`.
/// Returns the epochs added and whether the carried authority was adopted.
///
/// Only an **adopted** authority is re-verified, and a failure there refuses the import (the
/// ledger already re-verified in [`BackupArtifact::open`], so this is defensive). An authority
/// the album already held is left alone, unchecked: an unusable held authority costs the album
/// verification and writes, never read access (as in `Workspace::apply_album_store`), so it must
/// not stop the artifact's content keys from being recovered.
fn merge_album(
    store: &mut AlbumStore,
    album_id: Uuid,
    name: &str,
    rows: Vec<AmkRow>,
    carried: Option<PersistedAuthority>,
) -> Result<(usize, bool)> {
    let had_authority = store.get(&album_id).is_some_and(|a| a.authority.is_some());
    let carried_authority = carried.is_some();
    let added = store.merge_amks(album_id, name, rows, carried);
    let adopted = carried_authority && !had_authority;
    if adopted {
        let persisted = store
            .get(&album_id)
            .expect("merge_amks leaves the album in the store");
        if let Some(authority) = &persisted.authority {
            authority.restore(album_id, &persisted.held_epochs())?;
        }
    }
    Ok((added, adopted))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use tempfile::TempDir;

    use super::*;
    use crate::crypto::authority::{OpenMlsAuthority, ReferenceAuthority};
    use crate::crypto::keys::albumstore::{AlbumStoreError, PersistedAlbum};
    use crate::crypto::keys::{AmkVersion, HybridSigningKey, MasterKey};
    use crate::crypto::primitives::Argon2Params;
    use crate::crypto::verify_asset::{RejectReason, VerifyOutcome};
    use crate::lifecycle::{FAST_PARAMS, fast_workspace};

    /// #468: a backup restored on a new device of the same account carries the album's public
    /// authority, so the restored asset **verifies** — and still cannot be written, and the
    /// authority survives a reopen (it is persisted and re-verified on load, not session state).
    ///
    /// The new device's own directory lists only itself until it adopts the account's published
    /// one (#550), so the test hands it A's directory to stand in for that step; what is under
    /// test is the album authority, which `verify_asset` checks separately.
    #[test]
    fn a_restored_asset_verifies_under_the_authority_the_artifact_carried() {
        let (lib_a, lib_b, src) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let mut a = fast_workspace(lib_a.path());
        let album = a.create_album("Trip").unwrap();
        let photo = src.path().join("photo.jpg");
        fs::write(&photo, b"\xFF\xD8\xFF a photo worth verifying").unwrap();
        let asset = a.import_asset(album, &photo).unwrap();
        let archive = src.path().join("backup.tar");
        a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
            .unwrap();

        let mut b = Workspace::create_from_master(
            lib_b.path(),
            b"device b",
            FAST_PARAMS,
            MasterKey::from_bytes(*a.account.master.as_bytes()),
            a.user_id(),
            &a.sealed_identity(),
        )
        .unwrap();
        b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
            .unwrap();

        // Without the account's directory the next refusal is the device, not the authority.
        assert_eq!(
            b.verify(&asset).unwrap(),
            VerifyOutcome::TerminalReject(RejectReason::UnknownDevice)
        );
        b.directory = a.directory.clone();
        assert_eq!(b.verify(&asset).unwrap(), VerifyOutcome::Accept);
        assert!(matches!(
            b.album(&album).unwrap().write_tier_signer(),
            Err(LifecycleError::AlbumReadOnly(id)) if id == album
        ));

        // Persisted: a reopened B still verifies (with the account directory stood in again).
        drop(b);
        let mut b = Workspace::open(lib_b.path(), b"device b", FAST_PARAMS).unwrap();
        b.directory = a.directory.clone();
        assert_eq!(b.verify(&asset).unwrap(), VerifyOutcome::Accept);
    }

    /// #667: a restored device recovers the album's name, labels and its stacks' labels from
    /// the label records the artifact carries — not `Recovered album` — and keeps them across a
    /// reopen.
    #[test]
    fn a_restored_device_recovers_album_names_and_labels() {
        use crate::domain::StackType;
        use crate::sidecar::sidecar_v1::{StackMembership, StackRole};

        let (lib_a, lib_b, src) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let mut a = fast_workspace(lib_a.path());
        let album = a.create_album("Trip").unwrap();
        a.rename_album(album, "Lisbon 2026").unwrap();
        a.add_album_label(album, "family").unwrap();
        let photo = src.path().join("photo.jpg");
        fs::write(&photo, b"\xFF\xD8\xFF a named album's photo").unwrap();
        let asset = a.import_asset(album, &photo).unwrap();
        let stack = Uuid::now_v7();
        a.set_stack_membership(
            &asset,
            Some(StackMembership {
                stack_id: stack,
                stack_type: StackType::Burst,
                role: StackRole::Primary,
                member_index: Some(0),
            }),
        )
        .unwrap();
        a.add_stack_label(stack, "keeper").unwrap();
        let archive = src.path().join("backup.tar");
        a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
            .unwrap();

        let mut b = Workspace::create_from_master(
            lib_b.path(),
            b"device b",
            FAST_PARAMS,
            MasterKey::from_bytes(*a.account.master.as_bytes()),
            a.user_id(),
            &a.sealed_identity(),
        )
        .unwrap();
        b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
            .unwrap();

        assert_eq!(b.albums(), vec![(album, "Lisbon 2026".to_string())]);
        assert_eq!(
            b.album_labels(&album).unwrap(),
            BTreeSet::from(["family".to_string()])
        );
        assert_eq!(
            b.stack_labels(stack).unwrap(),
            BTreeSet::from(["keeper".to_string()])
        );

        drop(b);
        let b = Workspace::open(lib_b.path(), b"device b", FAST_PARAMS).unwrap();
        assert_eq!(b.albums(), vec![(album, "Lisbon 2026".to_string())]);
        assert_eq!(
            b.album_doc(&album).unwrap().current_name(),
            Some("Lisbon 2026")
        );
    }

    /// A second device of `a`'s account in `dir`, holding none of its albums.
    fn same_account_device(a: &Workspace, dir: &Path) -> Workspace {
        Workspace::create_from_master(
            dir,
            b"device b",
            FAST_PARAMS,
            MasterKey::from_bytes(*a.account.master.as_bytes()),
            a.user_id(),
            &a.sealed_identity(),
        )
        .unwrap()
    }

    /// A persisted label record that does not decode is left out of the export with a warning
    /// rather than failing it: the assets, keys and every other record still travel, and the
    /// album it named restores as `Recovered album`.
    #[test]
    fn an_unreadable_label_record_is_left_out_of_the_export() {
        use crate::domain::StackType;
        use crate::sidecar::sidecar_v1::{StackMembership, StackRole};

        let (lib_a, lib_b, src) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let mut a = fast_workspace(lib_a.path());
        let album = a.create_album("Trip").unwrap();
        let photo = src.path().join("photo.jpg");
        fs::write(&photo, b"\xFF\xD8\xFF a photo whose album record broke").unwrap();
        let asset = a.import_asset(album, &photo).unwrap();
        let stack = Uuid::now_v7();
        a.set_stack_membership(
            &asset,
            Some(StackMembership {
                stack_id: stack,
                stack_type: StackType::Burst,
                role: StackRole::Primary,
                member_index: Some(0),
            }),
        )
        .unwrap();
        a.add_stack_label(stack, "keeper").unwrap();
        fs::write(
            super::super::album::album_doc_path(lib_a.path(), &album),
            b"not a record",
        )
        .unwrap();
        assert!(a.album_doc(&album).is_err());

        let archive = src.path().join("backup.tar");
        a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
            .unwrap();

        let mut b = same_account_device(&a, lib_b.path());
        assert_eq!(
            b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
                .unwrap(),
            1
        );
        assert_eq!(b.albums(), vec![(album, RECOVERED_ALBUM_NAME.to_string())]);
        assert_eq!(
            b.stack_labels(stack).unwrap(),
            BTreeSet::from(["keeper".to_string()])
        );
    }

    /// A record the artifact carries that does not open under its escrowed AMK is skipped on
    /// restore: the assets, keys and every other record are restored around it.
    #[test]
    fn a_label_record_that_will_not_open_is_skipped_on_restore() {
        let (lib_a, lib_b, src) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let mut a = fast_workspace(lib_a.path());
        let album = a.create_album("Trip").unwrap();
        let photo = src.path().join("photo.jpg");
        fs::write(&photo, b"\xFF\xD8\xFF a photo beside a broken record").unwrap();
        a.import_asset(album, &photo).unwrap();

        let mut input = a.backup_input().unwrap();
        let broken_stack = Uuid::now_v7();
        input.docs.push(BackupDoc {
            kind: BackupDocKind::Stack,
            doc_id: broken_stack,
            album_id: album,
            amk_version: 1,
            sealed: vec![0; 64],
        });
        let bytes =
            backup::export_with_params(&input, b"pw", FAST_PARAMS, a.device_signer.as_ref())
                .unwrap();
        let archive = src.path().join("backup.tar");
        fs::write(&archive, bytes).unwrap();

        let mut b = same_account_device(&a, lib_b.path());
        assert_eq!(
            b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
                .unwrap(),
            1
        );
        assert_eq!(b.albums(), vec![(album, "Trip".to_string())]);
        assert!(matches!(
            b.stack_labels(broken_stack),
            Err(LifecycleError::NotFound(_))
        ));
    }

    /// #551: an adopted web-upload drop (`key_mode = wrapped`) survives a backup round trip —
    /// export ships the guest's ciphertext, and restore unwraps its key rather than deriving
    /// one, so a fresh library of the same account reads the guest's plaintext back.
    #[test]
    fn an_adopted_drop_round_trips_through_backup() {
        use crate::drop::{DropAdopter, LinkCaps, UploadLinkIssuer, seal_drop};

        let (lib_a, lib_b, src) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let mut a = fast_workspace(lib_a.path());
        let album = a.create_album("Guests").unwrap();
        let link = UploadLinkIssuer::create_link(&mut a, LinkCaps::default(), None).unwrap();
        let plaintext = b"\xFF\xD8\xFF a guest photo worth backing up";
        let sealed = seal_drop(plaintext, &link.drop_pubkey, "image/jpeg").unwrap();
        let drop_id = a.receive_drop(link.link_id, sealed).unwrap();
        let asset = DropAdopter::adopt(&mut a, drop_id, album)
            .unwrap()
            .asset_id();
        let archive = src.path().join("backup.tar");
        a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
            .unwrap();

        let mut b = Workspace::create_from_master(
            lib_b.path(),
            b"device b",
            FAST_PARAMS,
            MasterKey::from_bytes(*a.account.master.as_bytes()),
            a.user_id(),
            &a.sealed_identity(),
        )
        .unwrap();
        assert_eq!(
            b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
                .unwrap(),
            1
        );
        assert_eq!(b.read_plaintext(&asset).unwrap(), plaintext);
        b.directory = a.directory.clone();
        assert_eq!(b.verify(&asset).unwrap(), VerifyOutcome::Accept);
    }

    /// An album holding a held authority that no longer re-verifies (here a ledger whose admin
    /// key was swapped) still gains the artifact's content keys: the held authority is neither
    /// replaced nor re-checked, so it cannot fail the import. An adopted authority that does not
    /// verify is still refused.
    #[test]
    fn a_held_tampered_authority_does_not_block_key_recovery() {
        let album = Uuid::from_u128(0xA1);
        let admin = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
        let write_tier = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
        let mut tampered = ReferenceAuthority::new(album, admin.verifying_key())
            .with_epoch(&admin, AmkVersion(1), &write_tier.verifying_key(), true)
            .to_ledger();
        tampered.admin_pub = HybridSigningKey::from_seed_bytes(&[5; 32], &[6; 32]).verifying_key();
        let tampered = PersistedAuthority::Reference(Box::new(tampered));
        let valid = PersistedAuthority::Reference(Box::new(
            ReferenceAuthority::new(album, admin.verifying_key())
                .with_epoch(&admin, AmkVersion(1), &write_tier.verifying_key(), true)
                .to_ledger(),
        ));
        assert!(tampered.restore(album, &BTreeSet::from([1])).is_err());

        // Held and tampered: the import proceeds and adds the missing epoch.
        let mut store = AlbumStore::new();
        store.upsert(PersistedAlbum::recovered(
            album,
            "Trip",
            vec![(album, 1, [1; 32])],
            Some(tampered.clone()),
        ));
        let (added, adopted) = merge_album(
            &mut store,
            album,
            "Trip",
            vec![(album, 1, [1; 32]), (album, 2, [2; 32])],
            Some(valid),
        )
        .unwrap();
        assert_eq!((added, adopted), (1, false));
        assert_eq!(
            store.get(&album).unwrap().held_epochs(),
            BTreeSet::from([1, 2])
        );
        assert!(matches!(
            store.get(&album).unwrap().authority,
            Some(PersistedAuthority::Reference(ref l)) if l.admin_pub != admin.verifying_key()
        ));

        // Adopted and tampered: refused.
        let mut store = AlbumStore::new();
        assert!(matches!(
            merge_album(&mut store, album, "Trip", vec![(album, 1, [1; 32])], Some(tampered)),
            Err(LifecycleError::AlbumStore(AlbumStoreError::LedgerRejected(id))) if id == album
        ));
    }

    /// Decision 2 of #468: an album whose authority is an MLS group exports its content keys but
    /// no `authority/` entry, since the group's exported state holds private secrets.
    #[test]
    fn an_mls_authority_album_exports_without_an_authority_entry() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Group").unwrap();
        let photo = src.path().join("photo.jpg");
        fs::write(&photo, b"\xFF\xD8\xFF an mls album photo").unwrap();
        ws.import_asset(album, &photo).unwrap();
        ws.authorities.insert(
            album,
            Authority::OpenMls(Box::new(
                OpenMlsAuthority::create_self_group(album).unwrap(),
            )),
        );

        let archive = src.path().join("backup.tar");
        ws.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
            .unwrap();
        let artifact = BackupArtifact::open(
            &fs::read(&archive).unwrap(),
            b"pw",
            &ws.exporter_verifying_key(),
        )
        .unwrap();
        assert!(artifact.authority_ledgers().is_empty());
        assert!(artifact.amk_rows().iter().any(|row| row.0 == album));
    }

    /// The escrow verbs are a pair: what `escrow_master_key` mints is exactly what
    /// `verify_escrow` opens back to *this* device's master key, and a wrong secret does not
    /// verify. The master key itself never appears in either signature — that is the point of
    /// putting these on `Workspace` rather than exposing the raw bytes.
    #[test]
    fn escrow_round_trips_under_its_recovery_secret() {
        let lib = TempDir::new().unwrap();
        let ws = fast_workspace(lib.path());
        // The FFI/CLI callers use a `DeviceTier`; a test uses the fast cost directly through
        // the same `pwkdf::wrap` the tier resolves to, so the suite does not pay 256 MiB.
        let blob = crate::crypto::pwkdf::wrap_with(
            ws.account.master.as_bytes(),
            b"correct horse battery staple",
            Argon2Params {
                mem_kib: 64,
                t_cost: 1,
                p_cost: 1,
            },
        )
        .unwrap();

        assert_eq!(
            ws.verify_escrow(&blob, b"correct horse battery staple"),
            RecoveryVerdict::Verified
        );
        assert_eq!(
            ws.verify_escrow(&blob, b"the wrong secret"),
            RecoveryVerdict::NotVerified
        );
    }
}
