//! The portable backup artifact round-trip — the only `lifecycle` file that reaches
//! [`capsule_core::backup`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::Arc;

use capsule_core::backup::{
    self, BackupArtifact, BackupAsset, BackupDoc, BackupDocKind, BackupInput, RestoreMode,
    VerifyOutcome as RecoveryVerdict,
};
use capsule_core::crypto::authority::Authority;
use capsule_core::crypto::encryption::{blob_ciphertext_hash, blob_nonce, open_blob};
use capsule_core::crypto::hash::{self, Hash32};
use capsule_core::crypto::keys::albumstore::{AlbumStore, AmkRow, PersistedAuthority};
use capsule_core::crypto::keys::{Amk, HybridVerifyingKey};
#[cfg(any(test, feature = "test-support"))]
use capsule_core::crypto::primitives::Argon2Params;
use capsule_core::crypto::primitives::{CRYPTO_SUITE_ID, DeviceTier};
use capsule_core::crypto::provenance::{AssetManifest, ProvenanceChain};
use capsule_core::crypto::pwkdf::WrappedSecret;
use capsule_core::metadata::crdt::Lww;
use capsule_core::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};
use capsule_core::sidecar::{ALBUM_DOC_SCHEMA_V1, AlbumDocV1, STACK_DOC_SCHEMA_V1, StackDocV1};
use jiff::Timestamp;
use uuid::Uuid;

use super::album::{read_doc_file, stack_doc_path};
use super::{AssetState, LifecycleError, Result, StackPlacement, Workspace, now_rfc3339};
use crate::album_store::AlbumStoreFile;

/// Display name given to an album that arrives only as escrowed keys in a backup artifact. An
/// artifact that carries the album's label record replaces it with the album's own name once
/// that record is merged; only an artifact exported before the records existed leaves it.
const RECOVERED_ALBUM_NAME: &str = "Recovered album";

/// The assets a peer's delta artifact is cut from ([`Workspace::peer_export`]).
#[derive(Debug, Clone)]
pub struct PeerExport {
    /// Every managed asset, as a backup artifact carries it.
    pub assets: Vec<BackupAsset>,
    /// The AMK of each `(album, epoch)` an asset is sealed under.
    pub amks: BTreeMap<(Uuid, u32), [u8; 32]>,
}

impl Workspace {
    /// Wrap this account's master key under `recovery_secret` into the escrow blob a client
    /// stores server-side (`PUT /backup/escrow`; driven from the SDK by
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
    /// master key** — the derived-tag compare that backs the recovery verification cadence.
    ///
    /// Delegates wholesale to [`backup::verify_recovery_secret`]; nothing is compared here, and
    /// no key bytes are surfaced either way. The stale-cache rule (refresh the blob once before
    /// recording a genuine failure) belongs to the networked caller, not to this predicate.
    pub fn verify_escrow(&self, blob: &WrappedSecret, recovery_secret: &[u8]) -> RecoveryVerdict {
        backup::verify_recovery_secret(blob, recovery_secret, self.account.master.as_bytes())
    }

    /// The current provenance head hash for each managed asset (for backup reconciliation),
    /// and for each purged one the head of its kept chain, its final `delete`. A purged asset
    /// is held at that head, so an import or a peer offer never reads it as absent and
    /// re-applies an older copy as new.
    pub fn local_heads(&self) -> BTreeMap<Uuid, Hash32> {
        let purged = self
            .purged
            .iter()
            .map(|(id, head)| (*id, head.record_hash()));
        self.assets
            .iter()
            .filter_map(|(id, a)| a.chain.head().map(|h| (*id, h)))
            .chain(purged)
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
        write_artifact(out, |w| {
            backup::export_to(&input, passphrase, self.device_signer.as_ref(), w)
        })
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
        write_artifact(out, |w| {
            backup::export_with_params_to(
                &input,
                passphrase,
                params,
                self.device_signer.as_ref(),
                w,
            )
        })
    }

    /// Every managed asset as a backup artifact carries it, with the album key each one is
    /// sealed under: what a peer's delta artifact is cut from
    /// (`capsule_sdk::peering::DeltaExport`). The same assets [`export_backup`](Self::export_backup)
    /// writes, without the library-wide records (album authorities, label records, attestation
    /// pins) a peer delta does not carry.
    ///
    /// # Errors
    ///
    /// When an asset's upload bundle cannot be rebuilt or its evidence logs cannot be read: a
    /// delta silently missing an asset is not offered.
    #[tracing::instrument(skip_all)]
    pub fn peer_export(&self) -> Result<PeerExport> {
        let export = self.backup_assets()?;
        tracing::debug!(
            assets = export.assets.len(),
            epochs = export.amks.len(),
            "backup: assets collected for a peer delta"
        );
        Ok(export)
    }

    /// Everything the artifact needs about this library, collected once so both export entry
    /// points read the same state rather than each walking the library themselves.
    fn backup_input(&self) -> Result<BackupInput> {
        let PeerExport { assets, mut amks } = self.backup_assets()?;

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

        // The pinned attestation-key history of every server this library pushed to. Like the
        // evidence logs, a pin that exists and cannot be read or decoded fails the export: an
        // artifact silently without it restores a library that trusts its servers' next
        // publication on first use again.
        let attestation_key_pins = crate::library::read_attestation_key_pins(&self.root)
            .map_err(backup::BackupError::from)?
            .unwrap_or_default();

        Ok(BackupInput {
            assets,
            amks,
            authorities,
            docs,
            attestation_key_pins,
            exporter_device: self.account.device.device_id,
            source_library_version: "1".into(),
            export_timestamp: now_rfc3339(),
        })
    }

    /// Every managed asset as the artifact carries it, and the AMK of each `(album, epoch)` one
    /// is sealed under.
    fn backup_assets(&self) -> Result<PeerExport> {
        let mut assets = Vec::new();
        let mut amks: BTreeMap<(Uuid, u32), [u8; 32]> = BTreeMap::new();

        for asset in self.assets.values() {
            // The per-asset re-encrypt (plaintext → the ciphertext the manifest's recorded
            // nonce prefix pins) is the `upload_bundle` accessor's job — one copy of that
            // crypto, shared with `capsule push`. It also self-checks the re-derived
            // ciphertext against the manifest's content address, which this loop never did.
            let bundle = self.upload_bundle(&asset.asset_id)?;
            let album = self.album(&asset.album_id)?;
            amks.insert(
                (asset.album_id, bundle.amk_version),
                album.amks[&bundle.amk_version],
            );
            // The asset's evidence logs — custody receipts and signed storage attestations — if
            // the client has persisted them beside the chain. A log that exists and cannot be
            // read fails the export: an artifact silently missing its evidence is the loss the
            // artifact exists to prevent.
            let capture_utc = Some(asset.capture_utc);
            let receipts = read_evidence_log(&crate::library::receipts_path(
                &self.root,
                &asset.asset_id,
                capture_utc,
            ))?;
            let attestations = read_evidence_log(&crate::library::attestations_path(
                &self.root,
                &asset.asset_id,
                capture_utc,
            ))?;
            assets.push(BackupAsset {
                album_id: asset.album_id,
                asset_id: asset.asset_id,
                // Export the exact sealed blob the manifest committed to (re-sealing would draw
                // a fresh nonce and break the `metadata_blob_hash` content address).
                metadata_blob: bundle.metadata_blob,
                // The original as a source, not its bytes: the export streams it into the
                // artifact one STREAM chunk at a time, as a push does, so exporting never holds
                // the library's ciphertext. `upload_bundle` has just verified it re-derives to
                // the manifest's address; the export checks the bytes it writes again.
                ciphertext: Arc::new(bundle.original),
                provenance: asset.chain.records().to_vec(),
                receipts,
                attestations,
            });
        }
        Ok(PeerExport { assets, amks })
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

        // The pinned attestation-key history first: it is the one step that can refuse on the
        // library's own state (a pin that diverges from the carried one), and a refusal must
        // come before anything else is written. Folding it in is a union, so a later failure
        // leaves a pin that only ever grew by keys this account had already pinned.
        if let Some(carried) = artifact.attestation_key_pins() {
            let outcome = crate::library::restore_attestation_key_pins(&self.root, carried)
                .map_err(backup::BackupError::from)?;
            tracing::info!(?outcome, "backup: attestation-key pin restored");
        }

        // Fold the artifact's escrowed AMKs into the durable keystore **before** writing any
        // asset: this is the step that makes a keyless library whole again, and an asset on disk
        // whose key was never persisted is exactly the failure the durable album keystore exists
        // to prevent. An album already known keeps its write capability and authority and simply
        // gains missing epochs; an unknown album lands read-only with the public authority the artifact carried, since
        // a backup escrows content keys and the admin-signed ledger but no signing capability.
        self.absorb_recovered_amks(&artifact)?;
        // Then the albums' and stacks' label records, which need those keys to open: this is
        // what names a recovered album.
        self.absorb_recovered_docs(&artifact)?;

        let mut added = 0;
        for restored in &report.applied {
            self.write_restored(restored, SidecarSource::StandIn)?;
            added += 1;
        }
        tracing::info!(added, "backup: import complete");
        Ok(added)
    }

    /// Write one asset a peer's delta artifact restored (`capsule_sdk::peering::PeerRestore`'s
    /// `applied`) into this library: a new asset, or a forward update of one this library holds,
    /// whose chain extends the local head.
    ///
    /// The asset's album, and the epoch its head manifest is sealed under, must already be
    /// held: a peer delta carries no album authority or label record, so an asset of an album
    /// this library does not hold would be one it could neither push nor export. A forward
    /// update keeps the held asset's media path (its extension and date bucket) and stack
    /// placement, so the original is rewritten in place, and keeps any evidence log this device
    /// already holds over the peer's copy. The sidecar written is the one the peer's sealed
    /// metadata blob carries, opened and verified, so the peer's metadata arrives and none of
    /// what was held before its head is lost.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::Purged`] for an asset this workspace purged,
    /// [`LifecycleError::NotFound`] when the album or its epoch is not held, and
    /// [`LifecycleError::RestoredMetadata`] when the sealed metadata does not stand (nothing is
    /// written in either case), or the I/O, chain or index failure that stopped the write.
    #[tracing::instrument(skip_all, fields(asset_id = %restored.asset_id, album_id = %restored.album_id))]
    pub fn apply_peer_restored(&mut self, restored: &backup::RestoredAsset) -> Result<()> {
        if self.purged.contains_key(&restored.asset_id) {
            tracing::warn!(asset_id = %restored.asset_id, "peer: the asset was purged here; refusing to resurrect it");
            return Err(LifecycleError::Purged(restored.asset_id));
        }
        let epoch = restored
            .provenance
            .last()
            .ok_or_else(|| LifecycleError::Cbor("restored provenance is empty".into()))?
            .manifest
            .core
            .amk_version
            .0;
        if !self.album(&restored.album_id)?.amks.contains_key(&epoch) {
            tracing::warn!(
                epoch,
                "peer: the asset's album epoch is not held; not writing it"
            );
            return Err(LifecycleError::NotFound(format!(
                "album {} epoch {epoch}",
                restored.album_id
            )));
        }
        let forward = self.assets.contains_key(&restored.asset_id);
        self.write_restored(restored, SidecarSource::Sealed)?;
        tracing::info!(forward, "peer: restored asset written");
        Ok(())
    }

    /// Write a restored asset's original, signed artifacts, evidence logs and index rows, and
    /// hold it in this workspace. An asset already held keeps its media path and stack.
    fn write_restored(
        &mut self,
        restored: &backup::RestoredAsset,
        source: SidecarSource,
    ) -> Result<()> {
        let head = &restored
            .provenance
            .last()
            .ok_or_else(|| LifecycleError::Cbor("restored provenance is empty".into()))?
            .manifest;
        let held = self.assets.get(&restored.asset_id);
        let mut chain = ProvenanceChain::new();
        for rec in &restored.provenance {
            chain
                .append(rec.clone())
                .map_err(|e| LifecycleError::Cbor(format!("restore chain: {e}")))?;
        }
        let sidecar = match source {
            SidecarSource::Sealed => {
                self.open_restored_sidecar(restored, head, held.map(|h| &h.sidecar))?
            }
            SidecarSource::StandIn => self.stand_in_sidecar(restored, head),
        };
        // Keep `capture_utc` and the sidecar's `capture_timestamp` in agreement: a reopened
        // workspace derives an asset's media directory from that field. A new asset is filed
        // under its sidecar's capture time (wall-clock "now" would file it under a month its
        // own sidecar disagrees with); a held one keeps its own, so its original is rewritten
        // where it already is.
        let (capture_utc, ext, held_stack) = match held {
            Some(held) => (held.capture_utc, held.ext.clone(), held.stack.clone()),
            None => (
                sidecar
                    .capture_timestamp
                    .parse::<Timestamp>()
                    .or_else(|_| head.core.timestamp.parse::<Timestamp>())
                    .map_or_else(|_| Timestamp::now().as_second(), Timestamp::as_second),
                "bin".to_string(),
                None,
            ),
        };
        // The sidecar's signed `stack_membership` register is the durable placement, as a
        // reopen reads it; an unwritten register keeps the placement already held.
        let stack = match sidecar.stack_membership.get() {
            Some(membership) => membership.as_ref().map(StackPlacement::from_membership),
            None => held_stack,
        };
        let asset = AssetState {
            asset_id: restored.asset_id,
            album_id: restored.album_id,
            ext,
            capture_utc,
            chain,
            sidecar,
            // The artifact preserves the exact sealed blob the manifest committed to.
            metadata_blob: restored.metadata_blob.clone(),
            stack,
        };
        // The evidence logs the artifact carried go back beside the chain, byte for byte —
        // and **before** it: the chain on disk is what marks the asset present, and a later
        // restore skips a present asset, so a crash between the two must leave the logs
        // written and the asset absent (retried whole), never the reverse.
        restore_evidence_log(
            &crate::library::receipts_path(&self.root, &restored.asset_id, Some(capture_utc)),
            &restored.receipts,
        )?;
        restore_evidence_log(
            &crate::library::attestations_path(&self.root, &restored.asset_id, Some(capture_utc)),
            &restored.attestations,
        )?;
        self.write_asset_files(&asset, &restored.plaintext)?;
        self.index_asset_row(&asset)?;
        self.index_original_representation(&asset, restored.plaintext.len())?;
        self.assets.insert(restored.asset_id, asset);
        Ok(())
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
    /// [`AlbumStore::merge_amks`]: capsule_core::crypto::keys::albumstore::AlbumStore::merge_amks
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

    /// The signed sidecar a restored asset's sealed `metadata_blob` carries: bound to the
    /// latest manifest of its chain that commits to a metadata blob, opened under the album key
    /// of that manifest's epoch, decoded, and verified under the account's user IK, as
    /// `apply_remote_entry` admits a server entry. The sidecar is adopted byte for byte, so the
    /// blob kept beside it still round-trips to the committed hash.
    ///
    /// An empty blob is a library written before sealed metadata was durable: a held asset
    /// keeps its own sidecar, and a new one gets a stand-in from the head manifest.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::RestoredMetadata`] when the blob is not the one the chain commits to,
    /// its epoch's key is not held, or it does not open, decode, verify or describe this asset.
    fn open_restored_sidecar(
        &self,
        restored: &backup::RestoredAsset,
        head: &AssetManifest,
        held: Option<&SidecarV1>,
    ) -> Result<SidecarV1> {
        let refuse = |why: &str| {
            tracing::warn!(
                why,
                "restore: the asset's sealed metadata does not stand; not writing it"
            );
            LifecycleError::RestoredMetadata(format!("asset {}: {why}", restored.asset_id))
        };
        let blob = restored.metadata_blob.as_slice();
        if blob.is_empty() {
            tracing::warn!(
                held = held.is_some(),
                "restore: the asset carries no sealed metadata blob; keeping what is held"
            );
            return Ok(match held {
                Some(held) => held.clone(),
                None => self.stand_in_sidecar(restored, head),
            });
        }
        let Some(binding) = restored
            .provenance
            .iter()
            .rev()
            .map(|rec| &rec.manifest.core)
            .find(|core| core.action.binds_metadata_blob())
        else {
            return Err(refuse(
                "no manifest in its chain commits to a metadata blob",
            ));
        };
        if binding.metadata_blob_hash != Some(blob_ciphertext_hash(blob)) {
            return Err(refuse(
                "the metadata blob is not the one its chain commits to",
            ));
        }
        let epoch = binding.amk_version.0;
        let Some(amk) = self.album(&restored.album_id)?.amks.get(&epoch).copied() else {
            return Err(refuse(
                "the album key of the metadata blob's epoch is not held",
            ));
        };
        let Some(nonce) = blob_nonce(blob) else {
            return Err(refuse("the metadata blob is truncated"));
        };
        let blob_key = Amk::from_bytes(amk).derive_blob_key(&restored.asset_id, &nonce);
        let plaintext = open_blob(&blob_key, blob)
            .map_err(|_| refuse("the metadata blob does not open under its album key"))?;
        let sidecar = SidecarV1::from_canonical_slice(&plaintext, SIDECAR_SCHEMA_V1)
            .map_err(|_| refuse("the sealed sidecar does not decode"))?;
        if !sidecar.verify(&self.user_ik_public()) {
            return Err(refuse("the sealed sidecar's signature does not verify"));
        }
        if sidecar.uuid != restored.asset_id
            || sidecar.hash != hash::hash_bytes(&restored.plaintext)
        {
            return Err(refuse("the sealed sidecar describes another original"));
        }
        tracing::debug!(epoch, "restore: sealed sidecar opened and verified");
        Ok(sidecar)
    }

    /// A stand-in sidecar from the head manifest, for an asset whose library never persisted
    /// a sealed metadata blob: no user metadata survives, because none was carried.
    fn stand_in_sidecar(
        &self,
        restored: &backup::RestoredAsset,
        head: &AssetManifest,
    ) -> SidecarV1 {
        let mut sidecar = SidecarV1 {
            sidecar_schema: SIDECAR_SCHEMA_V1,
            crypto_suite_id: CRYPTO_SUITE_ID,
            uuid: restored.asset_id,
            hash: hash::hash_bytes(&restored.plaintext),
            capture_timestamp: head.core.timestamp.clone(),
            capture_timestamp_user: Lww::new(),
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
            gps_user: Lww::new(),
            // Mirror the head manifest's prior (the sealing invariant for its action).
            provenance_chain_hash: head.core.prior_provenance_hash,
            unknown: BTreeMap::new(),
            signature: None,
        };
        sidecar.sign(&self.account.user_ik);
        sidecar
    }
}

/// Where [`Workspace::write_restored`] takes a restored asset's sidecar from.
#[derive(Debug, Clone, Copy)]
enum SidecarSource {
    /// The sealed metadata blob, opened and verified under this account's user IK: a peer of
    /// the same account sealed it, so its metadata is facts this library adopts.
    Sealed,
    /// A stand-in from the head manifest, re-signed by this library: a backup may come from
    /// another account, whose sidecar signature this account cannot vouch for.
    StandIn,
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

/// An evidence log's bytes for the artifact: empty when the asset has none, an error when one
/// exists and cannot be read.
fn read_evidence_log(path: &Path) -> Result<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(LifecycleError::Io(format!(
            "evidence log {}: {e}",
            path.display()
        ))),
    }
}

/// Stream an artifact into `out` through `export`, atomically.
///
/// The artifact is written to a sibling temporary file and renamed over `out` only once it is
/// complete and synced. A streamed export can fail part-way — an original that changed on disk
/// since its manifest was signed, a full disk — and must then leave neither a truncated artifact
/// nor a clobbered earlier backup at `out`: the temporary file is removed and `out` is untouched.
fn write_artifact(
    out: &Path,
    export: impl FnOnce(&mut dyn std::io::Write) -> std::result::Result<u64, backup::BackupError>,
) -> Result<()> {
    let tmp = crate::library::tmp_path(out);
    let written = stream_then_rename(&tmp, out, export).inspect_err(|e| {
        tracing::error!(
            out = %out.display(),
            error = %e,
            "backup: export failed; discarding the partial artifact"
        );
        let _ = fs::remove_file(&tmp);
    })?;
    tracing::info!(out = %out.display(), bytes = written, "backup: export complete");
    Ok(())
}

/// The fallible body of [`write_artifact`]: export into `tmp`, sync it, rename it over `out`.
fn stream_then_rename(
    tmp: &Path,
    out: &Path,
    export: impl FnOnce(&mut dyn std::io::Write) -> std::result::Result<u64, backup::BackupError>,
) -> Result<u64> {
    let io = |e: std::io::Error| LifecycleError::Io(format!("backup {}: {e}", out.display()));
    let mut writer = std::io::BufWriter::new(fs::File::create(tmp).map_err(io)?);
    let written = export(&mut writer)?;
    let file = writer.into_inner().map_err(|e| io(e.into_error()))?;
    file.sync_all().map_err(io)?;
    fs::rename(tmp, out).map_err(io)?;
    Ok(written)
}

/// Write an evidence log a restore carried back to `path`, verbatim.
///
/// Nothing is written when the artifact carried none. A log already on disk is kept, not
/// overwritten: a restore only adds assets absent locally, so one there is a stray, but it is
/// still evidence, and the artifact's copy stays in the artifact.
///
/// The log is staged in a temporary sibling, flushed, and renamed into place, so a crash leaves
/// either no log or the whole one — never a torn log a retried restore would then keep.
fn restore_evidence_log(path: &Path, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    if path.exists() {
        tracing::warn!(
            log = %path.display(),
            "backup: an evidence log is already on disk; keeping it over the artifact's copy"
        );
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| LifecycleError::Io(e.to_string()))?;
    }
    let io =
        |e: std::io::Error| LifecycleError::Io(format!("evidence log {}: {e}", path.display()));
    let tmp = crate::library::tmp_path(path);
    {
        let mut file = fs::File::create(&tmp).map_err(io)?;
        std::io::Write::write_all(&mut file, bytes).map_err(io)?;
        file.sync_all().map_err(io)?;
    }
    fs::rename(&tmp, path).map_err(io)?;
    tracing::debug!(log = %path.display(), bytes = bytes.len(), "backup: evidence log restored");
    Ok(())
}

#[cfg(test)]
mod tests;
