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
use crate::crypto::keys::albumstore::{
    AlbumStore, AlbumStoreError, EscrowCoverage, PersistedAlbum, PersistedAuthority,
};
use crate::crypto::keys::{Amk, AmkVersion, HybridSigningKey};
use crate::metadata::crdt::{AddId, OrSet};
use crate::sidecar::{ALBUM_DOC_SCHEMA_V1, AlbumDocV1, normalize_label};
use crate::utils::paths::tmp_path;

/// What [`Workspace::plan_album_key_escrows`] decided, per album, in album id order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumKeyEscrowPlan {
    /// Albums whose escrow the server lacks or that this device strictly extends: store each.
    pub to_store: Vec<Uuid>,
    /// Albums whose server escrow already covers every epoch this device holds.
    pub current: usize,
    /// Albums whose server escrow disagrees with this device or does not open under its master
    /// key: left alone, and this device's epochs of them are not covered by the server.
    pub uncovered: Vec<Uuid>,
}

/// What [`Workspace::adopt_album_key_escrows`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AdoptedAlbumKeys {
    /// Albums the library did not hold and now does.
    pub adopted: usize,
    /// The epochs those albums brought with them.
    pub epochs: usize,
    /// Escrowed albums the library already held, left as they were.
    pub already_held: usize,
    /// Escrows that did not open under this account's master key — sealed under another one —
    /// and were skipped.
    pub unopenable: usize,
}

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
    ///
    /// The name follows the label rule, as [`rename_album`](Self::rename_album)'s does, so
    /// creation never accepts a name a rename would refuse: [`LifecycleError::InvalidLabel`]
    /// before any key is minted.
    #[tracing::instrument(skip_all, fields(album_id = %album_id, name = name))]
    pub fn create_album_with_id(&mut self, album_id: Uuid, name: &str) -> Result<Uuid> {
        if self.albums.contains_key(&album_id) {
            return Err(LifecycleError::AlbumExists(album_id));
        }
        let name = normalize_label(name)?;
        let name = name.as_str();
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

    /// `album_id`'s server-side key escrow: the whole album — every epoch's AMK, its signing
    /// seeds and its authority — sealed under the account master key
    /// ([`PersistedAlbum::seal_escrow`]). The bytes a client stores at
    /// `PUT /v1/auth/escrow/albums/{album_id}`; the master key never leaves the workspace.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::NotFound`] for an album this workspace holds no keys for, and
    /// [`LifecycleError::AlbumStore`] when the album does not encode.
    #[tracing::instrument(skip_all, fields(album_id = %album_id))]
    pub fn album_key_escrow(&self, album_id: &Uuid) -> Result<Vec<u8>> {
        let store = self.album_store_snapshot()?;
        let album = store
            .get(album_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("album {album_id}")))?;
        let blob = album.seal_escrow(&self.account.master)?;
        tracing::debug!(
            epochs = album.amks.len(),
            bytes = blob.len(),
            "album key escrow sealed under the master key"
        );
        Ok(blob)
    }

    /// What to do about each album's key escrow, given `escrowed` (album id → the escrow the
    /// server holds): which albums to store, and which this device's epochs the server's escrow
    /// does not cover and cannot safely be made to.
    ///
    /// **An escrow is never shrunk.** An album is stored only when the server holds none for it,
    /// or holds one this album strictly extends ([`EscrowCoverage::Superseded`]). An escrow that
    /// holds epochs this device lacks ([`EscrowCoverage::Behind`], another device rotated
    /// further), or the same epochs plus a signing seed or authority this device lacks (a
    /// read-only device restored from a backup), is left alone and counts as covering this
    /// device's epochs. One that disagrees
    /// with this device ([`EscrowCoverage::Divergent`]) or does not open under this master key
    /// ([`EscrowCoverage::Unreadable`]) is left alone too, and the album is reported
    /// [`uncovered`](AlbumKeyEscrowPlan::uncovered): replacing it could lose epochs a recovery
    /// needs, so the caller holds the album's assets back instead.
    ///
    /// Network-free. A client fetches what the server holds, asks this, and stores
    /// [`album_key_escrow`](Self::album_key_escrow) for each album in
    /// [`to_store`](AlbumKeyEscrowPlan::to_store) — before any asset of that album or epoch
    /// leaves the device, so the server never holds ciphertext whose key it does not also hold
    /// sealed under the master key.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::AlbumStore`] when the live album state cannot be captured.
    pub fn plan_album_key_escrows(
        &self,
        escrowed: &BTreeMap<Uuid, Vec<u8>>,
    ) -> Result<AlbumKeyEscrowPlan> {
        let store = self.album_store_snapshot()?;
        let mut plan = AlbumKeyEscrowPlan::default();
        for album in store.albums() {
            let Some(blob) = escrowed.get(&album.album_id) else {
                plan.to_store.push(album.album_id);
                continue;
            };
            match album.escrow_coverage(blob, &self.account.master) {
                EscrowCoverage::Current => plan.current += 1,
                EscrowCoverage::Behind => {
                    tracing::info!(
                        album_id = %album.album_id,
                        "album key escrow holds epochs or capabilities this device lacks; left \
                         as it is"
                    );
                    plan.current += 1;
                }
                EscrowCoverage::Superseded => plan.to_store.push(album.album_id),
                coverage @ (EscrowCoverage::Divergent | EscrowCoverage::Unreadable) => {
                    tracing::warn!(
                        album_id = %album.album_id,
                        ?coverage,
                        "album key escrow cannot be replaced without losing epochs or \
                         capabilities; the album is held back"
                    );
                    plan.uncovered.push(album.album_id);
                }
            }
        }
        tracing::debug!(
            albums = store.albums().len(),
            escrowed = escrowed.len(),
            to_store = plan.to_store.len(),
            current = plan.current,
            uncovered = plan.uncovered.len(),
            "album key escrows compared against what the server holds"
        );
        Ok(plan)
    }

    /// Adopt the account's escrowed albums into this library — the step `capsule auth recover`
    /// runs on a library it has just created from the master key, **before** anything calls
    /// [`ensure_album`](Self::ensure_album). An album recovered here is held as the device that
    /// escrowed it held it, so `ensure_album` resolves the derived default album rather than
    /// minting a fresh AMK under an id the account's other devices already wrote under.
    ///
    /// Every escrow is opened and every carried authority re-verified before the keystore is
    /// written, and an escrow that opens but is inconsistent refuses the whole adoption with
    /// nothing persisted — a library that silently lacked one album would mint over it at the
    /// next `ensure_album`. An album this workspace already holds keeps its own keys and is
    /// counted in [`AdoptedAlbumKeys::already_held`]: a held content key is never swapped
    /// underneath the authority that attests it.
    ///
    /// An escrow that does **not open** under this master key is skipped and counted in
    /// [`AdoptedAlbumKeys::unopenable`]. It is sealed under another master key — the escrow was
    /// later replaced from another library of the account — and its albums are that key's, never
    /// this one's: their ids cannot collide with this master key's derived default album, and
    /// refusing them would lock the account out of recovery for good, since nothing can remove
    /// them. Skipping one costs no more than a server omitting it from the listing would.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::AlbumStore`] when an escrow opens but is not an album key escrow, holds
    /// another album than the id it was served under, or carries an authority that does not
    /// re-verify, or when the keystore cannot be written.
    #[tracing::instrument(skip_all, fields(escrowed = escrowed.len()))]
    pub fn adopt_album_key_escrows(
        &mut self,
        escrowed: &[(Uuid, Vec<u8>)],
    ) -> Result<AdoptedAlbumKeys> {
        let mut store = self.album_store_snapshot()?;
        let mut adopted = Vec::new();
        let mut already_held = 0;
        let mut unopenable = 0;
        for (album_id, blob) in escrowed {
            let album = match PersistedAlbum::open_escrow(blob, &self.account.master, *album_id) {
                Ok(album) => album,
                Err(AlbumStoreError::Crypto(error)) => {
                    tracing::warn!(
                        album_id = %album_id,
                        %error,
                        "an album key escrow is sealed under another master key; skipped"
                    );
                    unopenable += 1;
                    continue;
                }
                Err(error) => {
                    tracing::warn!(album_id = %album_id, %error, "an album key escrow is inconsistent");
                    return Err(error.into());
                }
            };
            if store.get(album_id).is_some() {
                tracing::info!(album_id = %album_id, "album already held; its escrow is not adopted");
                already_held += 1;
                continue;
            }
            if let Some(authority) = &album.authority {
                authority.restore(*album_id, &album.held_epochs())?;
            }
            tracing::debug!(
                album_id = %album_id,
                epochs = album.amks.len(),
                writable = album.write_tier_seed64.is_some(),
                "album key escrow opened"
            );
            adopted.push(album);
        }
        let report = AdoptedAlbumKeys {
            adopted: adopted.len(),
            epochs: adopted.iter().map(|album| album.amks.len()).sum(),
            already_held,
            unopenable,
        };
        if adopted.is_empty() {
            tracing::info!(already_held, unopenable, "no album key escrow to adopt");
            return Ok(report);
        }
        for album in adopted {
            store.upsert(album);
        }
        store.save(&self.root, &self.account.master)?;
        self.apply_album_store(&store);
        tracing::info!(
            adopted = report.adopted,
            epochs = report.epochs,
            already_held,
            unopenable,
            "album key escrows adopted into the keystore"
        );
        Ok(report)
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

    /// Creation applies the same label rule to the name as a rename does: a name a rename
    /// would refuse is refused before any key is minted, and an accepted one is stored
    /// normalized in both the keystore and the record.
    #[test]
    fn creation_and_rename_apply_the_same_name_rule() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let too_long = "x".repeat(257);
        for bad in ["", "   ", too_long.as_str()] {
            assert!(matches!(
                ws.create_album(bad),
                Err(LifecycleError::InvalidLabel(_))
            ));
        }
        assert!(ws.albums().is_empty(), "a refused name mints no album");

        let album = ws.create_album("  Spaced  ").unwrap();
        assert_eq!(ws.albums(), vec![(album, "Spaced".to_string())]);
        assert_eq!(ws.album_doc(&album).unwrap().current_name(), Some("Spaced"));
        for bad in ["", "   ", too_long.as_str()] {
            assert!(matches!(
                ws.rename_album(album, bad),
                Err(LifecycleError::InvalidLabel(_))
            ));
        }
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

    /// A persisted album record that was altered after signing, or filed under another album's
    /// id, is refused when read rather than trusted; one that does not decode is a document
    /// error.
    #[test]
    fn a_tampered_persisted_album_record_is_refused() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let album = ws.create_album("Trip").unwrap();
        let other = ws.create_album("Other").unwrap();
        let path = album_doc_path(lib.path(), &album);

        let mut doc = ws.album_doc(&album).unwrap();
        doc.name.set(
            "Hijacked".into(),
            now_rfc3339(),
            ws.account.device.device_id,
        );
        fs::write(&path, doc.to_canonical_vec()).unwrap();
        assert!(matches!(
            ws.album_doc(&album),
            Err(LifecycleError::UnverifiedDocument(id)) if id == album
        ));

        fs::copy(album_doc_path(lib.path(), &other), &path).unwrap();
        assert!(matches!(
            ws.album_doc(&album),
            Err(LifecycleError::UnverifiedDocument(id)) if id == album
        ));

        fs::write(&path, b"not a record").unwrap();
        assert!(matches!(
            ws.album_doc(&album),
            Err(LifecycleError::Document(_))
        ));
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

    /// Every album's escrow, keyed as the server lists them.
    fn escrows_of(ws: &Workspace) -> BTreeMap<Uuid, Vec<u8>> {
        ws.albums()
            .into_iter()
            .map(|(id, _)| (id, ws.album_key_escrow(&id).unwrap()))
            .collect()
    }

    /// #741: an album is stored while the server lacks its escrow, current once it holds it, and
    /// stored again once an epoch is rotated in — the escrow only ever grows. Bytes that do not
    /// open as the album are left alone and the album reported uncovered, never overwritten.
    #[test]
    fn an_album_key_escrow_is_stored_until_it_holds_the_current_state() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let default = ws.default_album_id();
        ws.ensure_album(default, "Imports").unwrap();
        let trip = ws.create_album("Trip").unwrap();
        let mut both = vec![default, trip];
        both.sort();

        let plan = ws.plan_album_key_escrows(&BTreeMap::new()).unwrap();
        assert_eq!(plan.to_store, both);
        let mut escrowed = escrows_of(&ws);
        let plan = ws.plan_album_key_escrows(&escrowed).unwrap();
        assert!(plan.to_store.is_empty());
        assert_eq!(plan.current, 2);

        ws.rotate_epoch(trip).unwrap();
        assert_eq!(
            ws.plan_album_key_escrows(&escrowed).unwrap().to_store,
            vec![trip],
            "a rotated-in epoch strictly extends the escrow"
        );

        escrowed.insert(trip, ws.album_key_escrow(&trip).unwrap());
        escrowed.insert(default, b"not an escrow".to_vec());
        let plan = ws.plan_album_key_escrows(&escrowed).unwrap();
        assert!(
            plan.to_store.is_empty(),
            "an unreadable escrow is never overwritten"
        );
        assert_eq!(plan.uncovered, vec![default]);
        assert!(matches!(
            ws.album_key_escrow(&Uuid::now_v7()),
            Err(LifecycleError::NotFound(_))
        ));
    }

    /// #741 D1: a device **behind** the escrow — it holds fewer epochs of an album than another
    /// device escrowed — never replaces it, so a later recovery keeps every epoch; and two
    /// devices that each rotated independently leave the escrow alone and report the album
    /// uncovered rather than one dropping the other's epoch.
    #[test]
    fn a_device_behind_or_diverged_from_the_escrow_never_shrinks_it() {
        let src = TempDir::new().unwrap();
        let mut a = fast_workspace(src.path());
        let trip = a.create_album("Trip").unwrap();
        let at_epoch_1: Vec<(Uuid, Vec<u8>)> = escrows_of(&a).into_iter().collect();

        // B is recovered while the album is at epoch 1; A then rotates and escrows epoch 2.
        let dst = TempDir::new().unwrap();
        let mut b = recovered_from(&a, dst.path());
        b.adopt_album_key_escrows(&at_epoch_1).unwrap();
        a.rotate_epoch(trip).unwrap();
        let ahead = escrows_of(&a);

        let plan = b.plan_album_key_escrows(&ahead).unwrap();
        assert!(
            plan.to_store.is_empty(),
            "B's epoch-1 state would shrink A's"
        );
        assert!(
            plan.uncovered.is_empty(),
            "every epoch B holds is in A's escrow"
        );
        assert_eq!(plan.current, 1);

        // B rotates on its own: epoch 2 under another key. Neither may replace the other.
        b.rotate_epoch(trip).unwrap();
        let plan = b.plan_album_key_escrows(&ahead).unwrap();
        assert!(plan.to_store.is_empty());
        assert_eq!(plan.uncovered, vec![trip]);

        // A recovery from what the server holds still opens both of A's epochs.
        let again = TempDir::new().unwrap();
        let mut c = recovered_from(&a, again.path());
        c.adopt_album_key_escrows(&ahead.into_iter().collect::<Vec<_>>())
            .unwrap();
        assert_eq!(c.albums[&trip].amks, a.albums[&trip].amks);
    }

    /// #741: a library created from the master key alone adopts every escrowed album whole —
    /// every epoch, its signing keys and its authority — so `ensure_album` resolves the default
    /// album rather than minting over it, the recovered device can still author into it, and its
    /// own escrows match the ones it adopted.
    #[test]
    fn a_recovered_library_adopts_every_escrowed_album_before_ensure_album() {
        let src = TempDir::new().unwrap();
        let mut a = fast_workspace(src.path());
        let default = a.default_album_id();
        a.ensure_album(default, "Imports").unwrap();
        let trip = a.create_album("Trip").unwrap();
        a.rotate_epoch(trip).unwrap();
        let escrowed: Vec<(Uuid, Vec<u8>)> = escrows_of(&a).into_iter().collect();

        let dst = TempDir::new().unwrap();
        let mut b = Workspace::create_from_master(
            dst.path(),
            b"device b",
            FAST_PARAMS,
            crate::crypto::keys::MasterKey::from_bytes(*a.account.master.as_bytes()),
            a.user_id(),
            &a.sealed_identity(),
        )
        .unwrap();
        assert!(b.albums().is_empty());
        let adopted = b.adopt_album_key_escrows(&escrowed).unwrap();
        assert_eq!(
            adopted,
            AdoptedAlbumKeys {
                adopted: 2,
                epochs: 3,
                already_held: 0,
                unopenable: 0,
            }
        );
        assert_eq!(b.albums(), a.albums());
        assert_eq!(b.albums[&trip].amks, a.albums[&trip].amks);
        assert_eq!(b.albums[&trip].current_epoch, 2);

        b.ensure_album(default, "Imports").unwrap();
        assert_eq!(
            b.albums[&default].amks, a.albums[&default].amks,
            "ensure_album resolved the escrowed default album and minted nothing"
        );
        let plan = b.plan_album_key_escrows(&escrows_of(&a)).unwrap();
        assert!(
            plan.to_store.is_empty() && plan.uncovered.is_empty(),
            "the recovered library escrows exactly what it adopted"
        );

        // It writes into the adopted album under the escrowed epoch, and the asset verifies.
        let file = dst.path().join("photo.bin");
        fs::write(&file, b"recovered device bytes").unwrap();
        let asset = b.import_asset(trip, &file).unwrap();
        assert_eq!(b.verify(&asset).unwrap(), VerifyOutcome::Accept);

        // Adopting again holds what it has, and the adoption survives a reopen.
        let again = b.adopt_album_key_escrows(&escrowed).unwrap();
        assert_eq!(again.adopted, 0);
        assert_eq!(again.already_held, 2);
        drop(b);
        let b = Workspace::open(dst.path(), b"device b", FAST_PARAMS).unwrap();
        assert_eq!(b.albums(), a.albums());
    }

    /// A library created from `a`'s master key and identity, holding no album yet.
    fn recovered_from(a: &Workspace, dir: &Path) -> Workspace {
        Workspace::create_from_master(
            dir,
            b"device b",
            FAST_PARAMS,
            crate::crypto::keys::MasterKey::from_bytes(*a.account.master.as_bytes()),
            a.user_id(),
            &a.sealed_identity(),
        )
        .unwrap()
    }

    /// #741: an escrow that opens but is inconsistent — served under another album's id —
    /// refuses the whole adoption, and nothing is persisted: a library missing one album would
    /// mint over it at the next `ensure_album`.
    #[test]
    fn an_inconsistent_escrow_refuses_the_whole_adoption() {
        let src = TempDir::new().unwrap();
        let mut a = fast_workspace(src.path());
        let default = a.default_album_id();
        a.ensure_album(default, "Imports").unwrap();
        let trip = a.create_album("Trip").unwrap();

        let dst = TempDir::new().unwrap();
        let mut b = recovered_from(&a, dst.path());
        let escrowed = vec![
            (default, a.album_key_escrow(&default).unwrap()),
            (Uuid::now_v7(), a.album_key_escrow(&trip).unwrap()),
        ];
        assert!(matches!(
            b.adopt_album_key_escrows(&escrowed),
            Err(LifecycleError::AlbumStore(
                AlbumStoreError::EscrowAlbumMismatch { .. }
            ))
        ));
        assert!(b.albums().is_empty());
        drop(b);
        let b = Workspace::open(dst.path(), b"device b", FAST_PARAMS).unwrap();
        assert!(b.albums().is_empty(), "nothing was persisted");
    }

    /// #741: an escrow sealed under another master key — left by another library of the account
    /// whose escrow was replaced — is skipped and counted, and the rest are adopted: refusing it
    /// would lock the account out of recovery, since nothing removes it.
    #[test]
    fn an_escrow_under_another_master_key_is_skipped_not_fatal() {
        let src = TempDir::new().unwrap();
        let mut a = fast_workspace(src.path());
        let trip = a.create_album("Trip").unwrap();
        let other = TempDir::new().unwrap();
        let mut stranger = fast_workspace(other.path());
        let theirs = stranger.create_album("Theirs").unwrap();

        let dst = TempDir::new().unwrap();
        let mut b = recovered_from(&a, dst.path());
        let escrowed = vec![
            (trip, a.album_key_escrow(&trip).unwrap()),
            (theirs, stranger.album_key_escrow(&theirs).unwrap()),
        ];
        let report = b.adopt_album_key_escrows(&escrowed).unwrap();
        assert_eq!(report.adopted, 1);
        assert_eq!(report.unopenable, 1);
        assert_eq!(b.albums(), a.albums());
        assert!(!b.has_album(&theirs));
    }
}
