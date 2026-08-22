//! Container albums: key material (AMK epochs, write-tier + admin keys), the attested
//! [`Authority`] behind the album-authority seam, offline epoch rotation, and per-file key
//! re-derivation.

use std::collections::BTreeMap;

use uuid::Uuid;

use super::{AlbumKeys, LifecycleError, Result, Workspace};
use crate::crypto::authority::{Authority, ReferenceAuthority};
use crate::crypto::keys::{Amk, AmkVersion, HybridSigningKey};

impl Workspace {
    /// Create a container album: mint AMK_v1 + write-tier + admin keys and an attested
    /// authority. Returns the new album id.
    pub fn create_album(&mut self, name: &str) -> Uuid {
        self.create_album_with_id(Uuid::now_v7(), name)
    }

    /// Create an album with a specific id (e.g. the derived default-album id).
    pub fn create_album_with_id(&mut self, album_id: Uuid, name: &str) -> Uuid {
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
                write_tier,
                admin,
                current_epoch: 1,
            },
        );
        album_id
    }

    /// Rotate `album_id` to a fresh epoch: mint AMK_v{n+1} and a new write-tier key, have the
    /// album admin attest the new epoch, and advance the current epoch — the design's
    /// "AMK bump + write-tier rotation are one commit" atomicity. The admin key (the ledger
    /// root) is stable across epochs, and existing assets stay verifiable under their original
    /// epoch. Returns the new epoch. Membership changes / the MLS `Welcome` flow remain deferred
    /// (see `SLICES.md`).
    pub fn rotate_epoch(&mut self, album_id: Uuid) -> Result<u32> {
        let next = {
            let album = self
                .albums
                .get_mut(&album_id)
                .ok_or_else(|| LifecycleError::NotFound(format!("album {album_id}")))?;
            let next = album.current_epoch + 1;
            album.amks.insert(next, *Amk::generate().as_bytes());
            album.write_tier = HybridSigningKey::generate();
            album.current_epoch = next;
            next
        };
        // Disjoint fields: read the album's keys while mutably attesting in its authority. Offline
        // rotation is a reference-ledger attestation; the live MLS backend rotates via a
        // self-update commit through the membership ceremonies (S-X2), not this path — hence the
        // reference-only accessor.
        let album = self.albums.get(&album_id).expect("album just mutated");
        let authority = self
            .authorities
            .get_mut(&album_id)
            .and_then(Authority::as_reference_mut)
            .ok_or_else(|| LifecycleError::NotFound(format!("reference authority {album_id}")))?;
        authority.attest_epoch(
            &album.admin,
            AmkVersion(next),
            &album.write_tier.verifying_key(),
            true,
        );
        Ok(next)
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
        let album = ws.create_album("Trip");

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
