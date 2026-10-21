//! Container albums: key material (AMK epochs, write-tier + admin keys), the attested
//! [`Authority`] behind the album-authority seam, offline epoch rotation, and per-file key
//! re-derivation.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use serde_bytes::ByteBuf;
use uuid::Uuid;

use super::{AlbumKeys, LifecycleError, Result, Workspace, now_rfc3339};
use crate::crypto::authority::{Authority, ReferenceAuthority};
use crate::crypto::keys::albumstore::{AlbumStore, PersistedAlbum, PersistedAuthority};
use crate::crypto::keys::{Amk, AmkVersion, HybridSigningKey};
use crate::metadata::crdt::{AddId, OrSet};
use crate::sidecar::{ALBUM_DOC_SCHEMA_V1, AlbumDocV1, normalize_label};
use crate::utils::paths::tmp_path;

impl Workspace {
    /// Create a container album: mint AMK_v1 + write-tier + admin keys and an attested
    /// authority, then **persist** the album keystore. Returns the new album id.
    ///
    /// Fallible since `S-A10`: the keys only exist once they are on disk, and a workspace that
    /// reports an album it could not persist would hand the caller an album whose assets become
    /// undecryptable at the next close.
    pub fn create_album(&mut self, name: &str) -> Result<Uuid> {
        self.create_album_with_id(Uuid::now_v7(), name)
    }

    /// Create an album with a specific id (e.g. the derived default-album id).
    ///
    /// Refuses with [`LifecycleError::AlbumExists`] if the workspace already holds key material
    /// for `album_id`: minting over it would discard the AMKs every existing asset in that album
    /// was encrypted under. Use [`ensure_album`](Self::ensure_album) for resolve-or-create.
    #[tracing::instrument(skip_all, fields(album_id = %album_id, name = name))]
    pub fn create_album_with_id(&mut self, album_id: Uuid, name: &str) -> Result<Uuid> {
        if self.albums.contains_key(&album_id) {
            return Err(LifecycleError::AlbumExists(album_id));
        }
        let amk = Amk::generate();
        let write_tier = HybridSigningKey::generate();
        let admin = HybridSigningKey::generate();
        let mut amks = BTreeMap::new();
        amks.insert(1, *amk.as_bytes());

        let authority = ReferenceAuthority::new(album_id, admin.verifying_key()).with_epoch(
            &admin,
            AmkVersion(1),
            &write_tier.verifying_key(),
            true,
        );
        // Offline default: the reference ledger authority. The live `OpenMlsAuthority` drops into
        // the same `Authority` slot behind the seam once membership ceremonies land (S-X2).
        self.authorities
            .insert(album_id, Authority::Reference(Box::new(authority)));
        self.albums.insert(
            album_id,
            AlbumKeys {
                album_id,
                name: name.to_string(),
                amks,
                write_tier: Some(write_tier),
                admin: Some(admin),
                current_epoch: 1,
            },
        );
        self.persist_albums()?;
        // The album's label record, carrying the name so a restored or newly joined device sees
        // it rather than `Recovered album`.
        let mut doc = AlbumDocV1::new(album_id);
        doc.name.set(
            name.to_string(),
            now_rfc3339(),
            self.account.device.device_id,
        );
        self.store_album_doc(doc)?;
        tracing::info!("album created: AMK_v1 minted, authority attested, keystore persisted");
        Ok(album_id)
    }

    /// Resolve `album_id` to the album this workspace already holds, or create it under `name`.
    ///
    /// This is the verb a client's "default album" wiring wants: before `S-A10` the CLI minted a
    /// fresh `Imports` album on **every run**, which is precisely how a reopened library ended up
    /// unable to decrypt or extend its own prior imports.
    pub fn ensure_album(&mut self, album_id: Uuid, name: &str) -> Result<Uuid> {
        if self.albums.contains_key(&album_id) {
            tracing::debug!(album_id = %album_id, "album resolved from the durable keystore");
            return Ok(album_id);
        }
        self.create_album_with_id(album_id, name)
    }

    /// Whether this workspace holds key material for `album_id`.
    pub fn has_album(&self, album_id: &Uuid) -> bool {
        self.albums.contains_key(album_id)
    }

    /// Every album this workspace holds key material for, as `(album_id, name)`.
    pub fn albums(&self) -> Vec<(Uuid, String)> {
        let mut out: Vec<(Uuid, String)> = self
            .albums
            .values()
            .map(|a| (a.album_id, a.name.clone()))
            .collect();
        out.sort_by_key(|(id, _)| *id);
        out
    }

    /// Rotate `album_id` to a fresh epoch: mint AMK_v{n+1} and a new write-tier key, have the
    /// album admin attest the new epoch, and advance the current epoch — the design's
    /// "AMK bump + write-tier rotation are one commit" atomicity. The admin key (the ledger
    /// root) is stable across epochs, and existing assets stay verifiable under their original
    /// epoch. Returns the new epoch. Membership changes / the MLS `Welcome` flow remain deferred
    /// (see `SLICES.md`).
    #[tracing::instrument(skip_all, fields(album_id = %album_id))]
    pub fn rotate_epoch(&mut self, album_id: Uuid) -> Result<u32> {
        // Refuse up front on a read-only (backup-recovered) album: a rotation needs the admin key
        // to attest the new epoch, so half-mutating the AMK map first would leave the workspace
        // holding a content key no authority covers.
        self.album(&album_id)?.admin_signer()?;
        let next = {
            let album = self
                .albums
                .get_mut(&album_id)
                .ok_or_else(|| LifecycleError::NotFound(format!("album {album_id}")))?;
            let next = album.current_epoch + 1;
            album.amks.insert(next, *Amk::generate().as_bytes());
            album.write_tier = Some(HybridSigningKey::generate());
            album.current_epoch = next;
            next
        };
        // Disjoint fields: read the album's keys while mutably attesting in its authority. Offline
        // rotation is a reference-ledger attestation; the live MLS backend rotates via a
        // self-update commit through the membership ceremonies (S-X2), not this path — hence the
        // reference-only accessor.
        let album = self.albums.get(&album_id).expect("album just mutated");
        let admin = album.admin_signer()?.clone();
        let write_tier_pub = album.write_tier_signer()?.verifying_key();
        let authority = self
            .authorities
            .get_mut(&album_id)
            .and_then(Authority::as_reference_mut)
            .ok_or_else(|| LifecycleError::NotFound(format!("reference authority {album_id}")))?;
        authority.attest_epoch(&admin, AmkVersion(next), &write_tier_pub, true);
        // The new AMK and write-tier key exist only once they are durable; an unpersisted
        // rotation would make every asset written under the new epoch unreadable after a close.
        self.persist_albums()?;
        tracing::info!(epoch = next, "album epoch rotated and keystore persisted");
        Ok(next)
    }

    /// The album's authority, behind the [`AlbumAuthority`](crate::crypto::authority::AlbumAuthority)
    /// seam. A [`NotFound`](LifecycleError::NotFound) rather than a panic: a backup-recovered
    /// album holds content keys with no attested authority, and asking it to verify must be a
    /// typed refusal, not an index-out-of-bounds.
    pub(super) fn authority(&self, album_id: &Uuid) -> Result<&Authority> {
        self.authorities
            .get(album_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("authority for album {album_id}")))
    }

    /// Snapshot the workspace's live album state as a [`AlbumStore`] — the single conversion
    /// between the in-memory maps and the sealed on-disk form, so the two can never drift.
    pub(super) fn album_store_snapshot(&self) -> Result<AlbumStore> {
        let mut store = AlbumStore::new();
        for (album_id, keys) in &self.albums {
            let authority = self
                .authorities
                .get(album_id)
                .map(PersistedAuthority::capture)
                .transpose()?;
            store.upsert(PersistedAlbum {
                album_id: *album_id,
                name: keys.name.clone(),
                amks: keys
                    .amks
                    .iter()
                    .map(|(epoch, amk)| (*album_id, *epoch, *amk))
                    .collect(),
                current_epoch: keys.current_epoch,
                write_tier_seed64: keys
                    .write_tier
                    .as_ref()
                    .map(|k| ByteBuf::from(k.to_seed_bytes().to_vec())),
                admin_seed64: keys
                    .admin
                    .as_ref()
                    .map(|k| ByteBuf::from(k.to_seed_bytes().to_vec())),
                authority,
            });
        }
        Ok(store)
    }

    /// Seal and durably replace `{root}/.library/albums.cbor` from the live album maps. Called
    /// after every mutation of album key material.
    pub(super) fn persist_albums(&self) -> Result<()> {
        self.album_store_snapshot()?
            .save(&self.root, &self.account.master)?;
        Ok(())
    }

    /// Replace the live album + authority maps with `store`'s contents.
    ///
    /// A per-album failure is a `warn` and a skip rather than a failed open: one album whose
    /// authority ledger no longer verifies (or whose MLS state this build cannot decode) must not
    /// make the whole library unopenable. The album's **content keys are restored regardless** —
    /// an unusable authority costs verification and new writes, never read access — so a warn here
    /// is a degraded album, not a lost one.
    pub(super) fn apply_album_store(&mut self, store: &AlbumStore) {
        let mut albums = HashMap::new();
        let mut authorities = HashMap::new();
        for persisted in store.albums() {
            let album_id = persisted.album_id;
            let (write_tier, admin) = match (persisted.write_tier_key(), persisted.admin_key()) {
                (Ok(w), Ok(a)) => (w, a),
                (Err(e), _) | (_, Err(e)) => {
                    tracing::warn!(
                        album_id = %album_id,
                        error = %e,
                        "album store: undecodable signing seed; album restored read-only"
                    );
                    (None, None)
                }
            };
            if let Some(persisted_authority) = &persisted.authority {
                match persisted_authority.restore(album_id, &persisted.held_epochs()) {
                    Ok(authority) => {
                        authorities.insert(album_id, authority);
                    }
                    Err(e) => tracing::warn!(
                        album_id = %album_id,
                        error = %e,
                        "album store: authority could not be restored; album is readable but \
                         cannot verify or author writes until its authority is re-established"
                    ),
                }
            }
            albums.insert(
                album_id,
                AlbumKeys {
                    album_id,
                    name: persisted.name.clone(),
                    amks: persisted
                        .amks
                        .iter()
                        .map(|(_, epoch, amk)| (*epoch, *amk))
                        .collect(),
                    write_tier,
                    admin,
                    current_epoch: persisted.current_epoch,
                },
            );
        }
        tracing::info!(
            albums = albums.len(),
            authorities = authorities.len(),
            "album store: applied to the workspace"
        );
        self.albums = albums;
        self.authorities = authorities;
    }

    pub(super) fn album(&self, album_id: &Uuid) -> Result<&AlbumKeys> {
        self.albums
            .get(album_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("album {album_id}")))
    }

    /// Re-derive the per-file key under a *specific* epoch's AMK for a known `nonce_prefix`
    /// (the value recorded in the manifest). Callers pass the epoch the asset was written
    /// under (`amk_version`), never assuming the album's current epoch — so an asset imported
    /// before a rotation still derives the key it was encrypted with. Because the fresh
    /// `nonce_prefix` is folded into the salt, this is the read/regenerate path; a *fresh*
    /// write goes through
    /// [`encrypt_asset_rekey`](crate::crypto::encryption::encrypt_asset_rekey), which draws the
    /// nonce and derives together.
    pub(super) fn file_key(
        &self,
        album: &AlbumKeys,
        epoch: u32,
        file_id: &Uuid,
        nonce_prefix: &[u8],
    ) -> [u8; 32] {
        let amk = Amk::from_bytes(album.amks[&epoch]);
        amk.derive_file_key(file_id, nonce_prefix)
    }

    /// The file key of the **original** a manifest describes, honouring its `key_mode`.
    ///
    /// Resolves the epoch's AMK from `album` and recovers the key through
    /// [`recover_file_key`](crate::crypto::encryption::recover_file_key): a `derived` original
    /// re-derives, a `wrapped` one — an adopted web-upload drop, whose key a guest chose and
    /// nobody can derive — unwraps `wrapped_file_key` under the epoch's AMK
    /// (`asset-keywrap/v1`). Every workspace reader of an original's key goes through here —
    /// verify, upload and share-link issue — so an adopted asset behaves exactly as an
    /// imported one does.
    pub(super) fn original_file_key(
        &self,
        album: &AlbumKeys,
        core: &crate::crypto::provenance::ManifestCore,
    ) -> Result<[u8; 32]> {
        let epoch = core.amk_version.0;
        let amk_bytes = album
            .amks
            .get(&epoch)
            .ok_or_else(|| LifecycleError::NotFound(format!("album epoch {epoch} key")))?;
        Ok(crate::crypto::encryption::recover_file_key(
            &Amk::from_bytes(*amk_bytes),
            core,
        )?)
    }
}

// ── Album labels and name: the album's `AlbumDocV1` (owner: Organization — Labels) ──────────
//
// The record is persisted beside the album keystore as signed canonical CBOR — local plaintext,
// like every sidecar — and sealed under the album's current AMK whenever it leaves the device.
// An album that predates the record reads as one whose name was written at the Unix epoch by
// this device, so any real rename on any device wins the LWW merge over it.

/// The stamp of a name an album held before it had an [`AlbumDocV1`].
const LEGACY_NAME_TS: &str = "1970-01-01T00:00:00Z";

fn docs_dir(root: &Path, kind: &str) -> PathBuf {
    root.join(".library").join("docs").join(kind)
}

/// Where an album's signed label record lives in the library.
pub(super) fn album_doc_path(root: &Path, album_id: &Uuid) -> PathBuf {
    docs_dir(root, "albums").join(format!("{}.cbor", album_id.simple()))
}

/// Where a stack's signed label record lives in the library.
pub(super) fn stack_doc_path(root: &Path, stack_id: &Uuid) -> PathBuf {
    docs_dir(root, "stacks").join(format!("{}.cbor", stack_id.simple()))
}

/// Temp-then-rename, so a crash mid-write leaves the previous record intact.
pub(super) fn write_doc_file(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| LifecycleError::Io(e.to_string()))?;
    }
    let tmp = tmp_path(path);
    fs::write(&tmp, bytes).map_err(|e| LifecycleError::Io(e.to_string()))?;
    fs::rename(&tmp, path).map_err(|e| LifecycleError::Io(e.to_string()))
}

/// Read a record file, `None` when absent.
pub(super) fn read_doc_file(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(LifecycleError::Io(e.to_string())),
    }
}

/// Remove every live add of `label` from `labels` (observed-remove), returning how many.
pub(super) fn remove_label(labels: &mut OrSet<String>, label: &str) -> usize {
    let targets: Vec<AddId> = labels
        .entries()
        .into_iter()
        .filter(|(_, el)| el == label)
        .map(|(id, _)| id)
        .collect();
    for id in &targets {
        labels
            .remove(*id)
            .expect("a live entry's add_id was observed");
    }
    targets.len()
}

impl Workspace {
    /// Issue an `add_id` for an add into `set`, a label set held in a document rather than a
    /// sidecar. The open-time counter reseed sweeps sidecars only, so the issued counter is
    /// floored at one past the highest this device ever wrote into `set` — never re-issuing an
    /// `add_id` the set already holds.
    pub(super) fn issue_add_id_for(&mut self, set: &OrSet<String>) -> AddId {
        let mut id = self.counter.issue();
        if let Some(max) = set.max_add_counter_for(&id.device)
            && id.counter <= max
        {
            id.counter = max + 1;
        }
        id
    }

    /// `album_id`'s label record: the persisted one, or — for an album that predates the
    /// record — one holding the keystore's name under the legacy stamp.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::NotFound`] for an album this workspace holds no keys for;
    /// [`LifecycleError::UnverifiedDocument`] when the persisted record does not verify.
    pub fn album_doc(&self, album_id: &Uuid) -> Result<AlbumDocV1> {
        let album = self.album(album_id)?;
        if let Some(bytes) = read_doc_file(&album_doc_path(&self.root, album_id))? {
            let doc = AlbumDocV1::from_canonical_slice(&bytes, ALBUM_DOC_SCHEMA_V1)?;
            if doc.album_id != *album_id || !doc.verify(&self.account.user_ik.verifying_key()) {
                return Err(LifecycleError::UnverifiedDocument(*album_id));
            }
            return Ok(doc);
        }
        let mut doc = AlbumDocV1::new(*album_id);
        doc.name.set(
            album.name.clone(),
            LEGACY_NAME_TS,
            self.account.device.device_id,
        );
        doc.sign(&self.account.user_ik);
        Ok(doc)
    }

    /// Sign and persist `doc`, and make the keystore's name follow the record's.
    fn store_album_doc(&mut self, mut doc: AlbumDocV1) -> Result<()> {
        if !doc.verify(&self.account.user_ik.verifying_key()) {
            doc.sign(&self.account.user_ik);
        }
        write_doc_file(
            &album_doc_path(&self.root, &doc.album_id),
            &doc.to_canonical_vec(),
        )?;
        if let Some(name) = doc.current_name() {
            let album = self
                .albums
                .get_mut(&doc.album_id)
                .ok_or_else(|| LifecycleError::NotFound(format!("album {}", doc.album_id)))?;
            if album.name != name {
                album.name = name.to_string();
                self.persist_albums()?;
            }
        }
        tracing::debug!(
            album_id = %doc.album_id,
            labels = doc.labels.value().len(),
            "album label record persisted"
        );
        Ok(())
    }

    /// Rename an album: a stamped write to its record's name register, so concurrent renames
    /// on two devices converge on the later one. The name follows the label rule.
    #[tracing::instrument(skip(self, name), fields(album_id = %album_id))]
    pub fn rename_album(&mut self, album_id: Uuid, name: &str) -> Result<()> {
        let name = normalize_label(name)?;
        let mut doc = self.album_doc(&album_id)?;
        doc.name
            .set(name, now_rfc3339(), self.account.device.device_id);
        doc.signature = None;
        self.store_album_doc(doc)?;
        tracing::info!("album renamed");
        Ok(())
    }

    /// Add a label to an album.
    #[tracing::instrument(skip(self, label), fields(album_id = %album_id))]
    pub fn add_album_label(&mut self, album_id: Uuid, label: &str) -> Result<()> {
        let label = normalize_label(label)?;
        let mut doc = self.album_doc(&album_id)?;
        let add_id = self.issue_add_id_for(&doc.labels);
        doc.labels.add(label, add_id);
        doc.signature = None;
        self.store_album_doc(doc)
    }

    /// Remove a label from an album: every add of it this device has observed. Returns how
    /// many adds were removed (0 when the album does not carry the label).
    #[tracing::instrument(skip(self, label), fields(album_id = %album_id))]
    pub fn remove_album_label(&mut self, album_id: Uuid, label: &str) -> Result<usize> {
        let label = normalize_label(label)?;
        let mut doc = self.album_doc(&album_id)?;
        let removed = remove_label(&mut doc.labels, &label);
        if removed > 0 {
            doc.signature = None;
            self.store_album_doc(doc)?;
        }
        Ok(removed)
    }

    /// The album's current labels.
    pub fn album_labels(&self, album_id: &Uuid) -> Result<BTreeSet<String>> {
        Ok(self.album_doc(album_id)?.labels.value())
    }

    /// Seal the album's record under its current-epoch AMK — the bytes that travel to the
    /// album's other members and into a backup.
    pub fn seal_album_doc(&self, album_id: &Uuid) -> Result<Vec<u8>> {
        let album = self.album(album_id)?;
        let amk = Amk::from_bytes(album.amks[&album.current_epoch]);
        Ok(self.album_doc(album_id)?.seal(&amk)?)
    }

    /// Open a sealed record another device of this account sent for `album_id`, verify it under
    /// this account's identity key, and merge it into the local one. Every epoch this workspace
    /// holds is tried, newest first, since the sender may be an epoch ahead or behind.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::Document`] when no held AMK opens it or it does not decode;
    /// [`LifecycleError::UnverifiedDocument`] when it does not verify. Nothing is merged.
    #[tracing::instrument(skip(self, wire), fields(album_id = %album_id))]
    pub fn apply_remote_album_doc(&mut self, album_id: Uuid, wire: &[u8]) -> Result<()> {
        let album = self.album(&album_id)?;
        let mut last_err = None;
        let mut opened = None;
        for amk in album.amks.values().rev() {
            match AlbumDocV1::open_sealed(
                wire,
                &Amk::from_bytes(*amk),
                &album_id,
                ALBUM_DOC_SCHEMA_V1,
            ) {
                Ok(doc) => {
                    opened = Some(doc);
                    break;
                }
                Err(e) => last_err = Some(e),
            }
        }
        let remote = match (opened, last_err) {
            (Some(doc), _) => doc,
            (None, Some(e)) => return Err(e.into()),
            (None, None) => return Err(LifecycleError::NotFound(format!("album {album_id} keys"))),
        };
        if !remote.verify(&self.account.user_ik.verifying_key()) {
            tracing::warn!("remote album label record does not verify; refused");
            return Err(LifecycleError::UnverifiedDocument(album_id));
        }
        self.merge_album_doc(&remote)
    }

    /// Merge an already-trusted record into the local one and persist the result.
    pub(super) fn merge_album_doc(&mut self, remote: &AlbumDocV1) -> Result<()> {
        let mut doc = self.album_doc(&remote.album_id)?;
        doc.merge(remote)?;
        self.store_album_doc(doc)?;
        tracing::info!(album_id = %remote.album_id, "album label record merged");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::super::{FAST_PARAMS, fast_workspace};
    use super::*;
    use crate::crypto::verify_asset::VerifyOutcome;

    /// A second device of the same account, holding `album`'s keys as the first does.
    fn second_device(a: &Workspace, dir: &Path) -> Workspace {
        let mut b = Workspace::create_from_master(
            dir,
            b"device b",
            FAST_PARAMS,
            crate::crypto::keys::MasterKey::from_bytes(*a.account.master.as_bytes()),
            a.user_id(),
            &a.sealed_identity(),
        )
        .unwrap();
        b.apply_album_store(&a.album_store_snapshot().unwrap());
        b.persist_albums().unwrap();
        b
    }

    /// #667: the album's name lives in its signed record from creation, survives a reopen, and
    /// a rename is a stamped write.
    #[test]
    fn album_name_and_labels_live_in_the_signed_record() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        assert_eq!(ws.album_doc(&album).unwrap().current_name(), Some("Trip"));

        ws.rename_album(album, "  Lisbon 2026 ").unwrap();
        ws.add_album_label(album, "family").unwrap();
        ws.add_album_label(album, "summer").unwrap();
        assert_eq!(ws.remove_album_label(album, "summer").unwrap(), 1);
        assert_eq!(ws.remove_album_label(album, "absent").unwrap(), 0);
        assert!(matches!(
            ws.add_album_label(album, "   "),
            Err(LifecycleError::InvalidLabel(_))
        ));

        drop(ws);
        let ws = Workspace::open(lib.path(), b"passphrase", FAST_PARAMS).unwrap();
        assert_eq!(ws.albums(), vec![(album, "Lisbon 2026".to_string())]);
        let doc = ws.album_doc(&album).unwrap();
        assert!(doc.verify(&ws.user_ik_public()));
        assert_eq!(doc.labels.value(), BTreeSet::from(["family".to_string()]));
    }

    /// An album that predates its record reads its keystore name under the legacy stamp, so
    /// any real rename wins over it.
    #[test]
    fn an_album_without_a_record_reads_its_keystore_name() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Old").unwrap();
        fs::remove_file(album_doc_path(lib.path(), &album)).unwrap();
        let doc = ws.album_doc(&album).unwrap();
        assert_eq!(doc.current_name(), Some("Old"));
        assert_eq!(
            doc.name.current.as_ref().unwrap().ts,
            LEGACY_NAME_TS,
            "a legacy name is stamped at the epoch"
        );
        ws.rename_album(album, "New").unwrap();
        assert_eq!(ws.albums(), vec![(album, "New".to_string())]);
    }

    /// Two devices rename and label the same album concurrently; exchanging the sealed records
    /// in either order converges both on the later name and the union of the labels.
    #[test]
    fn concurrent_album_edits_converge_through_sealed_records() {
        let (lib_a, lib_b) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut a = fast_workspace(lib_a.path());
        let album = a.create_album("Trip").unwrap();
        let mut b = second_device(&a, lib_b.path());

        a.add_album_label(album, "beach").unwrap();
        b.add_album_label(album, "family").unwrap();
        a.rename_album(album, "From A").unwrap();
        b.rename_album(album, "From B").unwrap(); // stamped later

        let from_a = a.seal_album_doc(&album).unwrap();
        let from_b = b.seal_album_doc(&album).unwrap();
        a.apply_remote_album_doc(album, &from_b).unwrap();
        b.apply_remote_album_doc(album, &from_a).unwrap();

        for ws in [&a, &b] {
            assert_eq!(ws.albums(), vec![(album, "From B".to_string())]);
            assert_eq!(
                ws.album_labels(&album).unwrap(),
                BTreeSet::from(["beach".to_string(), "family".to_string()])
            );
        }
        // Replaying a stale record changes nothing.
        a.apply_remote_album_doc(album, &from_a).unwrap();
        assert_eq!(a.albums(), vec![(album, "From B".to_string())]);
    }

    /// A record signed by another account is refused, and a record sealed for another album
    /// does not open.
    #[test]
    fn a_foreign_album_record_is_refused() {
        let (lib_a, lib_c) = (TempDir::new().unwrap(), TempDir::new().unwrap());
        let mut a = fast_workspace(lib_a.path());
        let album = a.create_album("Trip").unwrap();
        let other = a.create_album("Other").unwrap();

        // Another account that somehow holds the same album keys.
        let mut c = fast_workspace(lib_c.path());
        c.apply_album_store(&a.album_store_snapshot().unwrap());
        c.persist_albums().unwrap();
        c.rename_album(album, "Hijacked").unwrap();
        let forged = c.seal_album_doc(&album).unwrap();
        assert!(matches!(
            a.apply_remote_album_doc(album, &forged),
            Err(LifecycleError::UnverifiedDocument(id)) if id == album
        ));
        assert!(a.apply_remote_album_doc(other, &forged).is_err());
        assert_eq!(a.album_doc(&album).unwrap().current_name(), Some("Trip"));
    }

    #[test]
    fn epoch_rotation_keeps_old_assets_verifiable_and_backs_up() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let a = src.path().join("a.jpg");
        let b = src.path().join("b.jpg");
        fs::write(&a, b"\xFF\xD8\xFF first photo, written at epoch 1").unwrap();
        fs::write(&b, b"\xFF\xD8\xFF second photo, written at epoch 2").unwrap();

        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();

        // Import at epoch 1, rotate the album, import at epoch 2.
        let id_a = ws.import_asset(album, &a).unwrap();
        assert_eq!(ws.rotate_epoch(album).unwrap(), 2);
        let id_b = ws.import_asset(album, &b).unwrap();

        // Each asset recorded the epoch it was written under...
        let epoch_of = |ws: &Workspace, id| {
            ws.asset(id).unwrap().chain.records()[0]
                .manifest
                .core
                .amk_version
        };
        assert_eq!(epoch_of(&ws, &id_a), AmkVersion(1));
        assert_eq!(epoch_of(&ws, &id_b), AmkVersion(2));
        // ...and BOTH still verify — the pre-rotation asset under its original epoch key (the
        // regression guard for the `current_epoch` file-key bug).
        assert_eq!(ws.verify(&id_a).unwrap(), VerifyOutcome::Accept);
        assert_eq!(ws.verify(&id_b).unwrap(), VerifyOutcome::Accept);

        // A cross-epoch backup escrows each asset's own-epoch AMK; restore into a fresh library
        // is byte-equal for both (guards the export file-key / blob-key / escrow-value epochs).
        let backup_path = src.path().join("backup.tar");
        ws.export_backup_with_params(&backup_path, b"recovery-pass", FAST_PARAMS)
            .unwrap();
        let exporter_pub = ws.exporter_verifying_key();

        let fresh = TempDir::new().unwrap();
        let mut ws2 = fast_workspace(fresh.path());
        let added = ws2
            .import_backup(&backup_path, b"recovery-pass", &exporter_pub)
            .unwrap();
        assert_eq!(added, 2);
        assert_eq!(
            ws2.read_plaintext(&id_a).unwrap(),
            ws.read_plaintext(&id_a).unwrap()
        );
        assert_eq!(
            ws2.read_plaintext(&id_b).unwrap(),
            ws.read_plaintext(&id_b).unwrap()
        );
    }
}
