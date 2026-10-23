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
use super::{AssetState, LifecycleError, Result, Workspace, now_rfc3339};
use crate::album_store::AlbumStoreFile;

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
            // The evidence logs the artifact carried go back beside the chain, byte for byte —
            // and **before** it: the chain on disk is what marks the asset present, and a later
            // restore skips a present asset, so a crash between the two must leave the logs
            // written and the asset absent (retried whole), never the reverse.
            restore_evidence_log(
                &crate::library::receipts_path(&self.root, &restored.asset_id, Some(capture_utc)),
                &restored.receipts,
            )?;
            restore_evidence_log(
                &crate::library::attestations_path(
                    &self.root,
                    &restored.asset_id,
                    Some(capture_utc),
                ),
                &restored.attestations,
            )?;
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
mod tests {
    use std::collections::BTreeSet;

    use capsule_core::crypto::authority::{OpenMlsAuthority, ReferenceAuthority};
    use capsule_core::crypto::keys::albumstore::{AlbumStoreError, PersistedAlbum};
    use capsule_core::crypto::keys::{AmkVersion, HybridSigningKey, MasterKey};
    use capsule_core::crypto::primitives::Argon2Params;
    use capsule_core::crypto::verify_asset::{RejectReason, VerifyOutcome};
    use tempfile::TempDir;

    use super::*;
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
        use capsule_core::domain::StackType;
        use capsule_core::sidecar::sidecar_v1::{StackMembership, StackRole};

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

    /// #747: the evidence logs a library keeps beside an asset — custody receipts and signed
    /// storage attestations — ride the backup and come back byte for byte on a restored device,
    /// where a reopen still finds the asset's original rather than mistaking a log for it.
    #[test]
    fn evidence_logs_ride_the_backup_and_restore_byte_for_byte() {
        use capsule_core::crypto::keys::HybridSigningKey;
        use capsule_core::crypto::storage_attestation::{
            AttestedAsset, STORAGE_ATTESTATION_VERSION, StorageAttestation, StorageAttestationCore,
        };

        let (lib_a, lib_b, src) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let mut a = fast_workspace(lib_a.path());
        let album = a.create_album("Trip").unwrap();
        let photo = src.path().join("photo.jpg");
        let original = b"\xFF\xD8\xFF a photo with evidence".to_vec();
        fs::write(&photo, &original).unwrap();
        let asset = a.import_asset(album, &photo).unwrap();
        let capture = Some(a.asset(&asset).unwrap().capture_utc);

        let server = HybridSigningKey::from_seed64(&[3; 64]);
        let core = StorageAttestationCore {
            version: STORAGE_ATTESTATION_VERSION.to_owned(),
            server_id: "capsule.example".to_owned(),
            server_key_id: Hash32([0x11; 32]),
            account: a.user_id().to_string(),
            nonce: [4; 32],
            checked_at: "2026-10-21T00:00:00Z".to_owned(),
            verdicts: vec![AttestedAsset {
                asset_id: asset.to_string(),
                durable: true,
                blobs: Vec::new(),
            }],
        };
        let attestation = StorageAttestation {
            server_sig: server.sign(&core.signing_bytes()),
            core,
        }
        .to_canonical_cbor();
        crate::library::append_attestation(lib_a.path(), &asset, capture, &attestation).unwrap();
        // The receipt log is carried verbatim, so any bytes stand in for it here.
        fs::write(
            crate::library::receipts_path(lib_a.path(), &asset, capture),
            b"receipt log",
        )
        .unwrap();
        let attestation_log = fs::read(crate::library::attestations_path(
            lib_a.path(),
            &asset,
            capture,
        ))
        .unwrap();

        let archive = src.path().join("backup.tar");
        a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
            .unwrap();
        let mut b = same_account_device(&a, lib_b.path());
        b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
            .unwrap();

        let restored_capture = Some(b.asset(&asset).unwrap().capture_utc);
        assert_eq!(
            fs::read(crate::library::attestations_path(
                lib_b.path(),
                &asset,
                restored_capture
            ))
            .unwrap(),
            attestation_log,
            "the attestation log restores byte for byte"
        );
        assert_eq!(
            fs::read(crate::library::receipts_path(
                lib_b.path(),
                &asset,
                restored_capture
            ))
            .unwrap(),
            b"receipt log",
            "the receipt log restores byte for byte"
        );
        let decoded =
            crate::library::load_attestations(lib_b.path(), &asset, restored_capture).unwrap();
        assert_eq!(decoded.len(), 1);
        assert!(decoded[0].verify_under(&server.verifying_key()));

        // A reopen walks the media directory: the logs are neither sidecars nor the original.
        drop(b);
        let b = Workspace::open(lib_b.path(), b"device b", FAST_PARAMS).unwrap();
        assert_eq!(b.read_plaintext(&asset).unwrap(), original);
        assert!(b.unmigrated_sidecars().is_empty());
    }

    /// A library with one photo in one album, exported to `archive`, with `pins` pinned first.
    fn exported_with_pins(
        lib: &Path,
        src: &Path,
        pins: &[(&str, &[HybridVerifyingKey])],
    ) -> (Workspace, Uuid, std::path::PathBuf) {
        let mut a = fast_workspace(lib);
        let album = a.create_album("Trip").unwrap();
        let photo = src.join("photo.jpg");
        fs::write(&photo, b"\xFF\xD8\xFF a photo pushed somewhere").unwrap();
        let asset = a.import_asset(album, &photo).unwrap();
        for (origin, keys) in pins {
            crate::library::pin_attestation_keys(lib, origin, keys).unwrap();
        }
        let archive = src.join("backup.tar");
        a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
            .unwrap();
        (a, asset, archive)
    }

    fn server_key(seed: u8) -> HybridVerifyingKey {
        HybridSigningKey::from_seed64(&[seed; 64]).verifying_key()
    }

    const SERVER: &str = "https://capsule.example";

    /// #805: the pin rides the backup byte for byte, and a run after the restore still refuses
    /// a server that dropped a key pinned before the backup.
    #[test]
    fn the_attestation_key_pin_rides_the_backup_byte_for_byte() {
        let (lib_a, lib_b, src) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let (a, asset, archive) = exported_with_pins(
            lib_a.path(),
            src.path(),
            &[(SERVER, &[server_key(1), server_key(2)])],
        );
        let pin = fs::read(crate::library::attestation_key_pins_path(lib_a.path())).unwrap();

        let mut b = same_account_device(&a, lib_b.path());
        assert_eq!(
            b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
                .unwrap(),
            1
        );

        assert_eq!(
            fs::read(crate::library::attestation_key_pins_path(lib_b.path())).unwrap(),
            pin,
            "the pin restores byte for byte"
        );
        assert!(b.asset(&asset).is_some());
        let error = crate::library::pin_attestation_keys(lib_b.path(), SERVER, &[server_key(2)])
            .unwrap_err();
        assert!(
            matches!(
                error,
                crate::library::KeyPinError::HistoryRewritten { missing: 1, .. }
            ),
            "{error:?}"
        );
    }

    /// #805: a restore into a library whose pin diverges from the artifact's is refused before
    /// it writes anything — neither pin, nor any asset or key.
    #[test]
    fn a_diverging_pin_refuses_the_restore_and_writes_nothing() {
        let (lib_a, lib_b, src) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let (a, asset, archive) = exported_with_pins(
            lib_a.path(),
            src.path(),
            &[(SERVER, &[server_key(1), server_key(2)])],
        );
        let mut b = same_account_device(&a, lib_b.path());
        crate::library::pin_attestation_keys(lib_b.path(), SERVER, &[server_key(1), server_key(9)])
            .unwrap();
        let pin_before = fs::read(crate::library::attestation_key_pins_path(lib_b.path())).unwrap();

        let error = b
            .import_backup(&archive, b"pw", &a.exporter_verifying_key())
            .unwrap_err();

        assert!(
            matches!(
                &error,
                LifecycleError::Backup(backup::BackupError::AttestationKeyPinsDiverge {
                    origin,
                    only_carried: 1,
                    only_local: 1,
                }) if origin == SERVER
            ),
            "{error:?}"
        );
        assert_eq!(
            fs::read(crate::library::attestation_key_pins_path(lib_b.path())).unwrap(),
            pin_before,
            "the library's pin is untouched"
        );
        assert!(b.asset(&asset).is_none(), "no asset was restored");
        let album = a.asset(&asset).unwrap().album_id;
        assert!(b.album(&album).is_err(), "no album key was absorbed");
    }

    /// An artifact whose pin has a schema newer than this build reads still restores its assets:
    /// the pin is skipped with a warning, never a reason to refuse the backup.
    #[test]
    fn a_pin_of_a_newer_schema_does_not_block_the_restore() {
        let (lib_a, lib_b, src) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let (a, asset, _) = exported_with_pins(lib_a.path(), src.path(), &[]);
        #[derive(serde::Serialize)]
        struct FuturePinFile {
            version: u8,
            servers: String,
        }
        let mut input = a.backup_input().unwrap();
        // A pin file as a later build might write it: a newer version, a shape this one lacks.
        input.attestation_key_pins = capsule_core::cbor::to_canonical_vec(&FuturePinFile {
            version: u8::MAX,
            servers: "from the future".to_owned(),
        })
        .unwrap();
        let archive = src.path().join("newer-pin.tar");
        let bytes =
            backup::export_with_params(&input, b"pw", FAST_PARAMS, a.device_signer.as_ref())
                .unwrap();
        fs::write(&archive, bytes).unwrap();
        let mut b = same_account_device(&a, lib_b.path());

        assert_eq!(
            b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
                .unwrap(),
            1
        );

        assert!(b.asset(&asset).is_some());
        assert!(!crate::library::attestation_key_pins_path(lib_b.path()).exists());
    }

    /// An artifact from a library that pinned nothing — or from before the pin rode the
    /// artifact — restores as before and leaves no pin behind.
    #[test]
    fn an_artifact_without_a_pin_restores_as_before() {
        let (lib_a, lib_b, src) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let (a, asset, archive) = exported_with_pins(lib_a.path(), src.path(), &[]);
        let mut b = same_account_device(&a, lib_b.path());

        assert_eq!(
            b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
                .unwrap(),
            1
        );

        assert!(b.asset(&asset).is_some());
        assert!(!crate::library::attestation_key_pins_path(lib_b.path()).exists());
    }

    /// A restore never overwrites an evidence log already on disk, and writes none when the
    /// artifact carried none.
    #[test]
    fn restoring_an_evidence_log_keeps_one_already_on_disk() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("media").join("x.attestations.cbor");

        restore_evidence_log(&path, &[]).unwrap();
        assert!(!path.exists(), "nothing carried, nothing written");

        restore_evidence_log(&path, b"from the artifact").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"from the artifact");
        assert!(
            !crate::library::tmp_path(&path).exists(),
            "the staged temporary was renamed into place"
        );

        restore_evidence_log(&path, b"a different copy").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"from the artifact");
    }

    /// An evidence log that is absent reads as empty; one that cannot be read fails the export
    /// rather than leaving the artifact silently without it.
    #[test]
    fn an_unreadable_evidence_log_fails_rather_than_vanishing() {
        let dir = TempDir::new().unwrap();
        assert!(
            read_evidence_log(&dir.path().join("absent.cbor"))
                .unwrap()
                .is_empty()
        );
        // A directory where the log should be is a read error that is not "not found".
        assert!(matches!(
            read_evidence_log(dir.path()),
            Err(LifecycleError::Io(_))
        ));
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
        use capsule_core::domain::StackType;
        use capsule_core::sidecar::sidecar_v1::{StackMembership, StackRole};

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
        use capsule_core::drop::{DropAdopter, LinkCaps, UploadLinkIssuer, seal_drop};

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
        let blob = capsule_core::crypto::pwkdf::wrap_with(
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

    /// `count` distinct originals of `len` bytes each, imported into one album of `a`.
    fn import_originals(a: &mut Workspace, src: &Path, count: u8, len: usize) -> Vec<Uuid> {
        let album = a.create_album("Large").unwrap();
        (0..count)
            .map(|n| {
                let mut bytes = b"\xFF\xD8\xFF a large original ".to_vec();
                bytes.extend((0..len).map(|i| (i as u8).wrapping_mul(31).wrapping_add(n)));
                let photo = src.join(format!("large-{n}.jpg"));
                fs::write(&photo, &bytes).unwrap();
                a.import_asset(album, &photo).unwrap()
            })
            .collect()
    }

    /// #759: exporting holds one STREAM chunk of an original at a time, not the library. The
    /// originals here sum to several times the ceiling and each alone exceeds it, so an export
    /// that buffered even one of them whole would fail — and the artifact still restores every
    /// original byte for byte.
    #[test]
    fn exporting_a_backup_holds_a_chunk_not_the_library() {
        const CEILING: usize = 2 << 20;
        const ORIGINAL: usize = 3 << 20;
        let (lib_a, lib_b, src) = (
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
            TempDir::new().unwrap(),
        );
        let mut a = fast_workspace(lib_a.path());
        let assets = import_originals(&mut a, src.path(), 4, ORIGINAL);
        let archive = src.path().join("backup.tar");

        let (exported, peak) = crate::test_heap::peak_during(|| {
            a.export_backup_with_params(&archive, b"pw", FAST_PARAMS)
        });
        exported.unwrap();
        assert!(
            peak < CEILING,
            "peak heap {peak} B exceeds the {CEILING} B ceiling over {} B of originals",
            assets.len() * ORIGINAL
        );

        let mut b = same_account_device(&a, lib_b.path());
        assert_eq!(
            b.import_backup(&archive, b"pw", &a.exporter_verifying_key())
                .unwrap(),
            assets.len()
        );
        for id in &assets {
            assert_eq!(
                fs::read(b.media_path(&b.assets[id])).unwrap(),
                fs::read(a.media_path(&a.assets[id])).unwrap(),
                "asset {id} restores byte for byte"
            );
        }
    }

    /// A streamed export that fails part-way — here an original rewritten on disk after its
    /// manifest was signed, so its bytes no longer hash to the address the MANIFEST already
    /// committed to — leaves no partial artifact and does not clobber the backup already at
    /// the destination.
    #[test]
    fn a_failed_export_leaves_the_previous_backup_untouched() {
        let (lib, src) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut a = fast_workspace(lib.path());
        let id = import_originals(&mut a, src.path(), 1, 4096)[0];
        let archive = src.path().join("backup.tar");
        fs::write(&archive, b"the previous backup").unwrap();

        let input = a.backup_input().unwrap();
        let media = a.media_path(&a.assets[&id]);
        let mut changed = fs::read(&media).unwrap();
        *changed.last_mut().unwrap() ^= 0xFF;
        fs::write(&media, &changed).unwrap();

        let err = write_artifact(&archive, |w| {
            backup::export_with_params_to(&input, b"pw", FAST_PARAMS, a.device_signer.as_ref(), w)
        })
        .unwrap_err();
        assert!(
            matches!(err, LifecycleError::Backup(backup::BackupError::Corrupt(_))),
            "{err:?}"
        );
        assert_eq!(fs::read(&archive).unwrap(), b"the previous backup");
        assert!(!crate::library::tmp_path(&archive).exists());
    }
}
