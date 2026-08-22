//! View-only share links — the [`ShareLinkIssuer`] half of the workspace and the scope
//! material it encapsulates.

use uuid::Uuid;

use super::{Workspace, now_rfc3339};
use crate::crypto::keys::Amk;
use crate::sharing::{
    LINK_SECRET_LEN, RevocationRecord, ScopeMaterial, ShareLink, ShareLinkId, ShareLinkIssuer,
    ShareLinkRecord, ShareScope, SharingError, encapsulate_scope, generate_opaque_id,
};

impl Workspace {
    /// The issuer-held record for a share link, if this workspace issued it. Exposes the
    /// authoritative scope / expiry / revocation state the serving endpoint (S-C4) consults.
    pub fn share_link(&self, link: &ShareLinkId) -> Option<&ShareLinkRecord> {
        self.share_links.get(link)
    }

    /// Resolve the scope's decryption material to encapsulate into a link. Fails with
    /// [`SharingError::ScopeUnavailable`] if the scope is not managed by this workspace.
    fn scope_material(
        &self,
        scope: ShareScope,
    ) -> std::result::Result<ScopeMaterial, SharingError> {
        match scope {
            ShareScope::Asset(asset_id) => {
                let asset = self
                    .assets
                    .get(&asset_id)
                    .ok_or(SharingError::ScopeUnavailable)?;
                let album = self
                    .albums
                    .get(&asset.album_id)
                    .ok_or(SharingError::ScopeUnavailable)?;
                let head = &asset
                    .chain
                    .records()
                    .last()
                    .expect("provenance chain is never empty")
                    .manifest
                    .core;
                let epoch = head.amk_version.0;
                let amk = album
                    .amks
                    .get(&epoch)
                    .ok_or(SharingError::ScopeUnavailable)?;
                // Carry the folded file key: the recipient re-uses the manifest's
                // `nonce_prefix` to decrypt, so the grant must derive under that same prefix.
                let file_key = Amk::from_bytes(*amk).derive_file_key(&asset_id, &head.nonce_prefix);
                Ok(ScopeMaterial::Asset {
                    file_id: asset_id,
                    file_key,
                })
            }
            ShareScope::Album(album_id) => {
                let album = self
                    .albums
                    .get(&album_id)
                    .ok_or(SharingError::ScopeUnavailable)?;
                Ok(ScopeMaterial::Album {
                    album_id,
                    amks: album.amks.clone(),
                })
            }
        }
    }
}

impl ShareLinkIssuer for Workspace {
    /// Issue a view-only share link: resolve the scope's decryption material, encapsulate
    /// it around a fresh CSPRNG link secret (and, if a passphrase is given, an Argon2id
    /// layer on top), draw a 128-bit opaque id, and record the link for revocation. The
    /// fragment secret is returned to the caller and never persisted server-side.
    fn create_link(
        &mut self,
        scope: ShareScope,
        expires_at: Option<String>,
        passphrase: Option<&str>,
    ) -> std::result::Result<ShareLink, SharingError> {
        let material = self.scope_material(scope)?;
        let fragment_secret = crate::crypto::rng::random_array::<LINK_SECRET_LEN>();
        let opaque_id = generate_opaque_id();
        let wrapped_scope = encapsulate_scope(
            &material,
            &fragment_secret,
            &opaque_id,
            passphrase,
            self.argon2_params,
        )?;
        let link_id = ShareLinkId(Uuid::now_v7());
        let created_at = now_rfc3339();
        self.share_links.insert(
            link_id,
            ShareLinkRecord {
                link_id,
                opaque_id,
                scope,
                wrapped_scope: wrapped_scope.clone(),
                expires_at: expires_at.clone(),
                created_at,
                revocation: None,
            },
        );
        Ok(ShareLink {
            link_id,
            opaque_id,
            scope,
            fragment_secret,
            wrapped_scope,
            expires_at,
        })
    }

    /// Publish a revocation record for `link`; the serve path then refuses it. Revoking an
    /// unknown link is [`SharingError::NotFound`]; revoking an already-revoked link is
    /// idempotent (the original revocation timestamp is kept).
    fn revoke_link(&mut self, link: ShareLinkId) -> std::result::Result<(), SharingError> {
        let record = self
            .share_links
            .get_mut(&link)
            .ok_or(SharingError::NotFound)?;
        if record.revocation.is_none() {
            record.revocation = Some(RevocationRecord {
                link_id: link,
                revoked_at: now_rfc3339(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use jiff::Timestamp;
    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;
    use crate::crypto::encryption::stream;

    fn imported_asset(ws: &mut Workspace, bytes: &[u8]) -> (Uuid, Uuid) {
        let src = TempDir::new().unwrap();
        let img = src.path().join("photo.jpg");
        fs::write(&img, bytes).unwrap();
        let album = ws.create_album("Shared").unwrap();
        let asset = ws.import_asset(album, &img).unwrap();
        // Keep the tempdir alive only until import copies the bytes in; return ids.
        (album, asset)
    }

    /// The encapsulated scope key genuinely grants the recipient decryption: a client with
    /// only the fragment secret + opaque id opens the scope and recovers the exact file key
    /// the asset was encrypted under, and decrypts the ciphertext back to the plaintext.
    #[test]
    fn share_link_asset_scope_grants_decryption() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let plaintext = b"\xFF\xD8\xFF shared-asset plaintext bytes";
        let (album_id, asset_id) = imported_asset(&mut ws, plaintext);

        let link = ws
            .create_link(ShareScope::Asset(asset_id), None, None)
            .unwrap();
        // Opaque id is a full 128 bits; the fragment secret is ≥128 bits; neither is empty.
        assert_eq!(link.opaque_id.len(), 16);
        assert_eq!(link.fragment_secret.len(), 32);
        assert!(!link.is_passphrase_protected());

        // Recipient side: open with only the URL material.
        let material = crate::sharing::open_scope(
            &link.wrapped_scope,
            &link.opaque_id,
            &link.fragment_secret,
            None,
        )
        .unwrap();

        let head = ws
            .asset(&asset_id)
            .unwrap()
            .chain
            .records()
            .last()
            .unwrap()
            .manifest
            .core
            .clone();
        let epoch = head.amk_version.0;
        let recovered = material
            .file_key_for(&asset_id, epoch, &head.nonce_prefix)
            .unwrap();

        // The recovered key equals the real per-file key ...
        let expected = ws.file_key(
            ws.album(&album_id).unwrap(),
            epoch,
            &asset_id,
            &head.nonce_prefix,
        );
        assert_eq!(recovered, expected, "recipient recovers the true file key");
        // ... and it actually decrypts the asset ciphertext back to plaintext.
        let (_, ciphertext) =
            stream::encrypt_asset_vec_with_prefix(&expected, head.nonce_prefix, plaintext);
        let back = stream::decrypt_asset_vec(&recovered, &head.nonce_prefix, &ciphertext).unwrap();
        assert_eq!(back, plaintext);
    }

    /// An album-scoped link with a passphrase: the material is passphrase-wrapped, unwraps
    /// client-side, and covers the album's epoch AMKs; the wrong passphrase is rejected.
    #[test]
    fn share_link_album_scope_with_passphrase() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let (album_id, asset_id) = imported_asset(&mut ws, b"\xFF\xD8\xFF album share");
        // Rotate so the album spans two epochs; the album grant must cover both.
        ws.rotate_epoch(album_id).unwrap();

        let link = ws
            .create_link(ShareScope::Album(album_id), None, Some("hunter2"))
            .unwrap();
        assert!(link.is_passphrase_protected());

        // Wrong passphrase → refused; no passphrase → refused; correct → material.
        assert_eq!(
            crate::sharing::open_scope(
                &link.wrapped_scope,
                &link.opaque_id,
                &link.fragment_secret,
                Some("nope")
            ),
            Err(SharingError::WrongPassphrase),
        );
        assert_eq!(
            crate::sharing::open_scope(
                &link.wrapped_scope,
                &link.opaque_id,
                &link.fragment_secret,
                None
            ),
            Err(SharingError::PassphraseRequired),
        );
        let material = crate::sharing::open_scope(
            &link.wrapped_scope,
            &link.opaque_id,
            &link.fragment_secret,
            Some("hunter2"),
        )
        .unwrap();

        // The album grant derives the asset's file key under its written epoch.
        let core = ws.asset(&asset_id).unwrap().chain.records()[0]
            .manifest
            .core
            .clone();
        let epoch = core.amk_version.0;
        assert_eq!(
            material
                .file_key_for(&asset_id, epoch, &core.nonce_prefix)
                .unwrap(),
            ws.file_key(
                ws.album(&album_id).unwrap(),
                epoch,
                &asset_id,
                &core.nonce_prefix
            ),
        );
    }

    /// Revocation & expiry: the issuer's record drives the serve-path liveness predicate.
    #[test]
    fn share_link_revocation_and_expiry() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let (album_id, _) = imported_asset(&mut ws, b"\xFF\xD8\xFF revoke me");
        let now = Timestamp::now();

        // A future-expiry link is live; a past-expiry link is not (fail-closed on expiry).
        let future = (now + jiff::SignedDuration::from_hours(24)).to_string();
        let live = ws
            .create_link(ShareScope::Album(album_id), Some(future), None)
            .unwrap();
        assert!(ws.share_link(&live.link_id).unwrap().is_live_at(now));

        let past = (now - jiff::SignedDuration::from_hours(24)).to_string();
        let expired = ws
            .create_link(ShareScope::Album(album_id), Some(past), None)
            .unwrap();
        assert!(!ws.share_link(&expired.link_id).unwrap().is_live_at(now));

        // Revoke the live link → its record is no longer live and carries a revocation record.
        ws.revoke_link(live.link_id).unwrap();
        let rec = ws.share_link(&live.link_id).unwrap();
        assert!(!rec.is_live_at(now));
        let revoked_at = rec.revocation.as_ref().unwrap().revoked_at.clone();

        // Re-revocation is idempotent; the original timestamp is kept.
        ws.revoke_link(live.link_id).unwrap();
        assert_eq!(
            ws.share_link(&live.link_id)
                .unwrap()
                .revocation
                .as_ref()
                .unwrap()
                .revoked_at,
            revoked_at,
        );

        // Revoking an unknown link is NotFound.
        assert_eq!(
            ws.revoke_link(ShareLinkId(Uuid::now_v7())),
            Err(SharingError::NotFound),
        );
    }

    /// A scope the workspace does not manage cannot be shared.
    #[test]
    fn share_link_unknown_scope_is_unavailable() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        assert_eq!(
            ws.create_link(ShareScope::Album(Uuid::now_v7()), None, None)
                .unwrap_err(),
            SharingError::ScopeUnavailable,
        );
        assert_eq!(
            ws.create_link(ShareScope::Asset(Uuid::now_v7()), None, None)
                .unwrap_err(),
            SharingError::ScopeUnavailable,
        );
    }
}
