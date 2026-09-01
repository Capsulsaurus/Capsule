//! Container albums: key material (AMK epochs, write-tier + admin keys), the attested
//! [`Authority`] behind the album-authority seam, offline epoch rotation, and per-file key
//! re-derivation.

use std::collections::{BTreeMap, HashMap};

use serde_bytes::ByteBuf;
use uuid::Uuid;

use super::{AlbumKeys, LifecycleError, Result, Workspace};
use crate::crypto::authority::{Authority, ReferenceAuthority};
use crate::crypto::keys::albumstore::{AlbumStore, PersistedAlbum, PersistedAuthority};
use crate::crypto::keys::{Amk, AmkVersion, HybridSigningKey};

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
    /// write goes through [`encrypt_asset_rekey`], which draws the nonce and derives together.
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
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;
    use crate::crypto::verify_asset::VerifyOutcome;

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
        ws.export_backup(&backup_path, b"recovery-pass").unwrap();
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
