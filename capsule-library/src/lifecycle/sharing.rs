//! View-only share links — the [`ShareLinkIssuer`] half of the workspace and the scope
//! material it encapsulates.

use capsule_core::crypto::provenance::KeyMode;
use capsule_core::sharing::{
    LINK_SECRET_LEN, RevocationRecord, ScopeMaterial, ShareLink, ShareLinkId, ShareLinkIssuer,
    ShareLinkRecord, SharePublication, ShareScope, SharedAsset, SharingError, encapsulate_scope,
    generate_opaque_id, publish_share,
};
use uuid::Uuid;

use super::{AssetState, Workspace, asset_is_deleted, now_rfc3339};

impl Workspace {
    /// The issuer-held record for a share link, if this workspace issued it. Exposes the
    /// authoritative scope / expiry / revocation state the serving endpoint consults.
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
                if !album.amks.contains_key(&head.amk_version.0) {
                    return Err(SharingError::ScopeUnavailable);
                }
                // Carry the head's file key, honouring its `key_mode`: a derived key is folded
                // under the manifest's `nonce_prefix` (which the recipient re-uses to decrypt),
                // and an adopted drop's is unwrapped from `wrapped_file_key`.
                let file_key = self.original_file_key(album, head).map_err(|e| {
                    tracing::warn!(%asset_id, error = %e, "share link: asset file key recovery failed");
                    SharingError::Crypto("asset file key recovery failed")
                })?;
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

    /// The per-asset records a link over `scope` covers (#622): for each asset, the key-free
    /// parameters a viewer needs to fetch its original's ciphertext and decrypt it under the
    /// opened scope, read off the asset's **head** manifest and signed sidecar.
    ///
    /// - **Asset scope** covers that one asset. The scope material carries its file key
    ///   directly, so the record says `amk_version = 0`, `derived`, and no wrapped key, as
    ///   [`SharedAsset`] documents for an asset-scoped grant: the wrapped key would be a field the
    ///   viewer never reads.
    /// - **Album scope** covers every asset in the album that is not in trash, in asset-id order
    ///   so the same album publishes the same record. Each keeps its manifest's epoch, key mode
    ///   and wrapped key, because the viewer recovers the file key from the grant's AMKs.
    ///
    /// An asset in trash is not shareable: asset scope refuses it with
    /// [`SharingError::ScopeUnavailable`], and album scope leaves it out.
    ///
    /// # Errors
    ///
    /// [`SharingError::ScopeUnavailable`] when the scope is not managed by this workspace, or an
    /// asset-scoped asset is in trash.
    pub fn shared_assets(
        &self,
        scope: ShareScope,
    ) -> std::result::Result<Vec<SharedAsset>, SharingError> {
        let record = |asset: &AssetState, grant: Grant| {
            let head = &asset
                .chain
                .records()
                .last()
                .expect("provenance chain is never empty")
                .manifest
                .core;
            let (amk_version, key_mode, wrapped_file_key) = match grant {
                Grant::FileKey => (0, KeyMode::Derived, None),
                Grant::Amks => (
                    head.amk_version.0,
                    head.key_mode,
                    head.wrapped_file_key.clone(),
                ),
            };
            SharedAsset {
                asset_id: asset.asset_id,
                content_hash: head.ciphertext_hash,
                content_type: asset.sidecar.content_type.clone(),
                size: head.plaintext_size,
                amk_version,
                nonce_prefix: head.nonce_prefix,
                key_mode,
                wrapped_file_key,
            }
        };
        match scope {
            ShareScope::Asset(asset_id) => {
                let asset = self
                    .assets
                    .get(&asset_id)
                    .ok_or(SharingError::ScopeUnavailable)?;
                if asset_is_deleted(asset) {
                    tracing::info!(%asset_id, "share link: an asset in trash is not shareable");
                    return Err(SharingError::ScopeUnavailable);
                }
                Ok(vec![record(asset, Grant::FileKey)])
            }
            ShareScope::Album(album_id) => {
                if !self.albums.contains_key(&album_id) {
                    return Err(SharingError::ScopeUnavailable);
                }
                let mut covered: Vec<&AssetState> = self
                    .assets
                    .values()
                    .filter(|asset| asset.album_id == album_id && !asset_is_deleted(asset))
                    .collect();
                covered.sort_by_key(|asset| asset.asset_id);
                Ok(covered
                    .into_iter()
                    .map(|asset| record(asset, Grant::Amks))
                    .collect())
            }
        }
    }

    /// Everything the server needs to serve `link` (#622): the sealed `share-metadata/v1`
    /// record over [`shared_assets`](Self::shared_assets), its content address, the `serves`
    /// set, and for a passphrase link the wrapped secret — [`publish_share`] under the link's
    /// own fragment, opaque id, scope encapsulation and expiry.
    ///
    /// The record is sealed with a fresh nonce on every call, so two calls for one link yield
    /// two different `metadata_hash`es; publish once per link and register what that call
    /// returned.
    ///
    /// # Errors
    ///
    /// What [`shared_assets`](Self::shared_assets) and [`publish_share`] return.
    #[tracing::instrument(skip(self, link), fields(opaque_id = %link.opaque_id_hex()))]
    pub fn publish_link(
        &self,
        link: &ShareLink,
    ) -> std::result::Result<SharePublication, SharingError> {
        let assets = self.shared_assets(link.scope)?;
        let covered = assets.len();
        let publication = publish_share(
            &link.wrapped_scope,
            &link.fragment_secret,
            &link.opaque_id,
            link.expires_at.clone(),
            assets,
        )?;
        tracing::info!(
            covered,
            serves = publication.serves.len(),
            passphrase = link.is_passphrase_protected(),
            "share link: metadata record published"
        );
        Ok(publication)
    }
}

/// How a link's scope material yields a covered asset's file key, which decides what that
/// asset's [`SharedAsset`] record must carry.
#[derive(Clone, Copy)]
enum Grant {
    /// The material is the file key itself (an asset-scoped grant).
    FileKey,
    /// The material is the album's AMK ledger, from which the viewer recovers the key.
    Amks,
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
        let fragment_secret = capsule_core::crypto::rng::random_array::<LINK_SECRET_LEN>();
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

    use capsule_core::crypto::encryption::{decrypt_asset_vec, encrypt_asset_vec_with_prefix};
    use jiff::Timestamp;
    use tempfile::TempDir;

    use super::super::fast_workspace;
    use super::*;

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
        let material = capsule_core::sharing::open_scope(
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
            encrypt_asset_vec_with_prefix(&expected, head.nonce_prefix, plaintext);
        let back = decrypt_asset_vec(&recovered, &head.nonce_prefix, &ciphertext).unwrap();
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
            capsule_core::sharing::open_scope(
                &link.wrapped_scope,
                &link.opaque_id,
                &link.fragment_secret,
                Some("nope")
            ),
            Err(SharingError::WrongPassphrase),
        );
        assert_eq!(
            capsule_core::sharing::open_scope(
                &link.wrapped_scope,
                &link.opaque_id,
                &link.fragment_secret,
                None
            ),
            Err(SharingError::PassphraseRequired),
        );
        let material = capsule_core::sharing::open_scope(
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

    /// Stage and adopt a guest drop into a fresh album; returns `(album, asset, ciphertext)`.
    fn adopted_drop(ws: &mut Workspace, plaintext: &[u8]) -> (Uuid, Uuid, Vec<u8>) {
        use capsule_core::drop::{DropAdopter, LinkCaps, UploadLinkIssuer, seal_drop};
        let album = ws.create_album("Guests").unwrap();
        let link = UploadLinkIssuer::create_link(ws, LinkCaps::default(), None).unwrap();
        let sealed = seal_drop(plaintext, &link.drop_pubkey, "image/jpeg").unwrap();
        let ciphertext = sealed.ciphertext.clone();
        let drop_id = ws.receive_drop(link.link_id, sealed).unwrap();
        let adopted = DropAdopter::adopt(ws, drop_id, album).unwrap();
        assert_eq!(
            adopted.manifest.core.key_mode,
            capsule_core::crypto::provenance::KeyMode::Wrapped
        );
        (album, adopted.asset_id(), ciphertext)
    }

    fn head_core(
        ws: &Workspace,
        asset_id: &Uuid,
    ) -> capsule_core::crypto::provenance::ManifestCore {
        ws.asset(asset_id)
            .unwrap()
            .chain
            .records()
            .last()
            .unwrap()
            .manifest
            .core
            .clone()
    }

    /// #551: an asset-scoped link to an adopted drop carries the guest's key — unwrapped
    /// from `wrapped_file_key`, not re-derived — so the recipient decrypts the unchanged
    /// guest ciphertext.
    #[test]
    fn share_link_asset_scope_grants_an_adopted_drop() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let plaintext = b"\xFF\xD8\xFF an adopted guest photo, shared";
        let (_, asset_id, ciphertext) = adopted_drop(&mut ws, plaintext);

        let link = ws
            .create_link(ShareScope::Asset(asset_id), None, None)
            .unwrap();
        let material = capsule_core::sharing::open_scope(
            &link.wrapped_scope,
            &link.opaque_id,
            &link.fragment_secret,
            None,
        )
        .unwrap();
        let head = head_core(&ws, &asset_id);
        let key = material.file_key_for_manifest(&head).unwrap();
        assert_eq!(
            decrypt_asset_vec(&key, &head.nonce_prefix, &ciphertext).unwrap(),
            plaintext
        );
    }

    /// #551: an album-scoped link covers an adopted drop: from the grant's AMKs and the
    /// asset's manifest, the recipient unwraps the guest's key and decrypts the ciphertext.
    /// The derived-only triple path cannot, which is why the manifest path exists.
    #[test]
    fn share_link_album_scope_grants_an_adopted_drop() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let plaintext = b"\xFF\xD8\xFF an adopted guest photo, album-shared";
        let (album_id, asset_id, ciphertext) = adopted_drop(&mut ws, plaintext);

        let link = ws
            .create_link(ShareScope::Album(album_id), None, None)
            .unwrap();
        let material = capsule_core::sharing::open_scope(
            &link.wrapped_scope,
            &link.opaque_id,
            &link.fragment_secret,
            None,
        )
        .unwrap();
        let head = head_core(&ws, &asset_id);
        let key = material.file_key_for_manifest(&head).unwrap();
        assert_eq!(
            decrypt_asset_vec(&key, &head.nonce_prefix, &ciphertext).unwrap(),
            plaintext
        );
        let derived = material
            .file_key_for(&asset_id, head.amk_version.0, &head.nonce_prefix)
            .unwrap();
        assert!(decrypt_asset_vec(&derived, &head.nonce_prefix, &ciphertext).is_err());

        // #598: the field-wise recovery the browser viewer uses agrees with the manifest path.
        let by_fields = material
            .file_key_for_key_mode(
                &asset_id,
                head.amk_version.0,
                &head.nonce_prefix,
                head.key_mode,
                head.wrapped_file_key.as_ref(),
            )
            .unwrap();
        assert_eq!(by_fields, key);
        assert_eq!(
            material.file_key_for_key_mode(
                &asset_id,
                99,
                &head.nonce_prefix,
                head.key_mode,
                head.wrapped_file_key.as_ref(),
            ),
            Err(SharingError::ScopeUnavailable)
        );

        // An epoch the grant holds no AMK for is not covered.
        let mut unheld = head.clone();
        unheld.amk_version = capsule_core::crypto::keys::AmkVersion(99);
        assert_eq!(
            material.file_key_for_manifest(&unheld),
            Err(SharingError::ScopeUnavailable)
        );

        // Another album's manifest at an epoch number the grant does hold is not covered
        // either: epochs are per album, so the grant's AMK for that number is not its key.
        let (_, other) = imported_asset(&mut ws, b"\xFF\xD8\xFF not in the shared album");
        let foreign = head_core(&ws, &other);
        assert_ne!(foreign.album_id, album_id);
        assert_eq!(foreign.amk_version, head.amk_version);
        assert_eq!(
            material.file_key_for_manifest(&foreign),
            Err(SharingError::ScopeUnavailable)
        );
    }

    /// A tampered `wrapped` manifest in the shared album: its key sealed under another AMK,
    /// or missing altogether. Each one fails closed with a `Crypto` error and yields no key.
    fn tampered_wrapped_keys(
        core: &capsule_core::crypto::provenance::ManifestCore,
    ) -> [Option<capsule_core::crypto::provenance::WrappedFileKey>; 2] {
        let foreign_amk = capsule_core::crypto::keys::Amk::from_bytes([0x99; 32]);
        [
            Some(capsule_core::crypto::provenance::WrappedFileKey(
                capsule_core::crypto::encryption::seal_file_key(
                    &foreign_amk,
                    &core.file_id,
                    &[0x77; 32],
                ),
            )),
            None,
        ]
    }

    /// The album-scope recipient path fails closed on a wrapped key that does not open.
    #[test]
    fn album_scope_wrapped_key_that_does_not_open_fails_closed() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let (album_id, asset_id, _) = adopted_drop(&mut ws, b"\xFF\xD8\xFF tampered drop");
        let link = ws
            .create_link(ShareScope::Album(album_id), None, None)
            .unwrap();
        let material = capsule_core::sharing::open_scope(
            &link.wrapped_scope,
            &link.opaque_id,
            &link.fragment_secret,
            None,
        )
        .unwrap();
        let head = head_core(&ws, &asset_id);
        for wrapped in tampered_wrapped_keys(&head) {
            let mut core = head.clone();
            core.wrapped_file_key = wrapped;
            assert_eq!(
                material.file_key_for_manifest(&core),
                Err(SharingError::Crypto("asset file key recovery failed"))
            );
            assert_eq!(
                material.file_key_for_key_mode(
                    &core.file_id,
                    core.amk_version.0,
                    &core.nonce_prefix,
                    core.key_mode,
                    core.wrapped_file_key.as_ref(),
                ),
                Err(SharingError::Crypto("asset file key recovery failed"))
            );
        }
    }

    /// Asset-scope issue fails closed, minting no link, when the head's wrapped key does not
    /// open under the album's AMK.
    #[test]
    fn asset_scope_issue_with_a_wrapped_key_that_does_not_open_fails_closed() {
        use capsule_core::crypto::provenance::ProvenanceChain;
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let (_, asset_id, _) = adopted_drop(&mut ws, b"\xFF\xD8\xFF tampered drop");
        let record = ws
            .asset(&asset_id)
            .unwrap()
            .chain
            .records()
            .last()
            .unwrap()
            .clone();
        for wrapped in tampered_wrapped_keys(&record.manifest.core) {
            let mut tampered = record.clone();
            tampered.manifest.core.wrapped_file_key = wrapped;
            let mut chain = ProvenanceChain::new();
            chain.append(tampered).unwrap();
            ws.assets.get_mut(&asset_id).unwrap().chain = chain;
            let issued = ws.share_links.len();
            assert_eq!(
                ws.create_link(ShareScope::Asset(asset_id), None, None)
                    .map(|_| ()),
                Err(SharingError::Crypto("asset file key recovery failed"))
            );
            assert_eq!(ws.share_links.len(), issued, "no link is recorded");
        }
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

    /// Open what `publish_link` produced the way a viewer does, with nothing but the URL (and the
    /// passphrase): the sealed record, then the scope from the record or from the wrapped secret,
    /// then every covered asset's ciphertext as the server would serve it. Returns each asset's
    /// decrypted plaintext keyed by asset id.
    fn open_as_viewer(
        ws: &Workspace,
        link: &ShareLink,
        publication: &SharePublication,
        passphrase: Option<&str>,
    ) -> Vec<(Uuid, Vec<u8>)> {
        use capsule_core::sharing::{WrappedScope, open_scope, open_share_metadata};

        let record = open_share_metadata(
            &publication.metadata_blob,
            &link.opaque_id,
            &link.fragment_secret,
        )
        .unwrap();
        assert_eq!(record.expires_at, link.expires_at);
        let scope = match (&record.link_scope, &publication.wrapped_secret) {
            (Some(scope), None) => scope.clone(),
            (None, Some(secret)) => capsule_core::cbor::from_slice::<WrappedScope>(secret).unwrap(),
            other => panic!("the scope travels in exactly one place, got {other:?}"),
        };
        let material = open_scope(&scope, &link.opaque_id, &link.fragment_secret, passphrase)
            .expect("the scope opens");

        record
            .assets
            .iter()
            .map(|asset| {
                let served = ws
                    .upload_bundle(&asset.asset_id)
                    .unwrap()
                    .original
                    .to_vec()
                    .unwrap();
                assert_eq!(
                    capsule_core::crypto::hash::hash_bytes(&served),
                    asset.content_hash,
                    "the record names the original the push uploads"
                );
                assert!(publication.serves.contains(&asset.content_hash));
                let key = material
                    .file_key_for_key_mode(
                        &asset.asset_id,
                        asset.amk_version,
                        &asset.nonce_prefix,
                        asset.key_mode,
                        asset.wrapped_file_key.as_ref(),
                    )
                    .unwrap();
                let plaintext = decrypt_asset_vec(&key, &asset.nonce_prefix, &served).unwrap();
                assert_eq!(plaintext.len() as u64, asset.size);
                (asset.asset_id, plaintext)
            })
            .collect()
    }

    /// #622: an album link published from the workspace opens with the URL alone and decrypts
    /// every covered asset — one derived import written before an epoch rotation, one written
    /// after it, and one adopted guest drop in `wrapped` mode — and leaves out an asset in trash.
    #[test]
    fn a_published_album_link_opens_every_covered_asset() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let (album_id, adopted, _) = adopted_drop(&mut ws, b"\xFF\xD8\xFF guest photo");

        let src = TempDir::new().unwrap();
        let import = |ws: &mut Workspace, name: &str, bytes: &[u8]| {
            let path = src.path().join(name);
            fs::write(&path, bytes).unwrap();
            ws.import_asset(album_id, &path).unwrap()
        };
        let before = import(&mut ws, "before.jpg", b"\xFF\xD8\xFF before rotation");
        ws.rotate_epoch(album_id).unwrap();
        let after = import(&mut ws, "after.jpg", b"\xFF\xD8\xFF after rotation");
        let trashed = import(&mut ws, "trashed.jpg", b"\xFF\xD8\xFF in trash");
        ws.soft_delete(&trashed, 30).unwrap();

        let expiry = (Timestamp::now() + jiff::SignedDuration::from_hours(24)).to_string();
        let link = ws
            .create_link(ShareScope::Album(album_id), Some(expiry), None)
            .unwrap();
        let publication = ws.publish_link(&link).unwrap();
        assert_eq!(
            publication.wrapped_secret, None,
            "no passphrase, no wrapped secret"
        );
        assert_eq!(
            publication.serves[0], publication.metadata_hash,
            "the record leads the serves set"
        );
        assert_eq!(
            publication.metadata_hash,
            capsule_core::crypto::hash::hash_bytes(&publication.metadata_blob)
        );

        let mut expected = vec![
            (adopted, b"\xFF\xD8\xFF guest photo".to_vec()),
            (before, b"\xFF\xD8\xFF before rotation".to_vec()),
            (after, b"\xFF\xD8\xFF after rotation".to_vec()),
        ];
        expected.sort_by_key(|(id, _)| *id);
        assert_eq!(open_as_viewer(&ws, &link, &publication, None), expected);
        assert_eq!(publication.serves.len(), 1 + expected.len());

        let records = ws.shared_assets(ShareScope::Album(album_id)).unwrap();
        let guest = records.iter().find(|a| a.asset_id == adopted).unwrap();
        assert_eq!(
            guest.key_mode,
            capsule_core::crypto::provenance::KeyMode::Wrapped
        );
        assert!(guest.wrapped_file_key.is_some());
        let epochs: Vec<u32> = [before, after]
            .iter()
            .map(|id| {
                records
                    .iter()
                    .find(|a| a.asset_id == *id)
                    .unwrap()
                    .amk_version
            })
            .collect();
        assert_ne!(epochs[0], epochs[1], "each asset keeps its own epoch");
    }

    /// #622: an asset link behind a passphrase carries its scope only in the wrapped secret, and
    /// its record is the asset-scoped shape: epoch `0`, `derived`, no wrapped key — also for an
    /// adopted drop, whose file key the scope carries directly.
    #[test]
    fn a_published_asset_link_opens_behind_its_passphrase() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let plaintext = b"\xFF\xD8\xFF a single adopted photo";
        let (_, asset_id, _) = adopted_drop(&mut ws, plaintext);

        let link = ws
            .create_link(ShareScope::Asset(asset_id), None, Some("hunter2"))
            .unwrap();
        let publication = ws.publish_link(&link).unwrap();
        assert!(publication.wrapped_secret.is_some());
        assert_eq!(publication.serves.len(), 2);

        let records = ws.shared_assets(ShareScope::Asset(asset_id)).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].amk_version, 0);
        assert_eq!(
            records[0].key_mode,
            capsule_core::crypto::provenance::KeyMode::Derived
        );
        assert_eq!(records[0].wrapped_file_key, None);

        assert_eq!(
            open_as_viewer(&ws, &link, &publication, Some("hunter2")),
            vec![(asset_id, plaintext.to_vec())]
        );
    }

    /// An asset in trash cannot be shared on its own, and a scope the workspace does not manage
    /// publishes nothing.
    #[test]
    fn publishing_refuses_a_trashed_asset_and_an_unknown_scope() {
        let lib = TempDir::new().unwrap();
        let mut ws = fast_workspace(lib.path());
        let (_, asset_id) = imported_asset(&mut ws, b"\xFF\xD8\xFF trash me");
        ws.soft_delete(&asset_id, 30).unwrap();
        assert_eq!(
            ws.shared_assets(ShareScope::Asset(asset_id)),
            Err(SharingError::ScopeUnavailable)
        );
        assert_eq!(
            ws.shared_assets(ShareScope::Album(Uuid::now_v7())),
            Err(SharingError::ScopeUnavailable)
        );
        assert_eq!(
            ws.shared_assets(ShareScope::Asset(Uuid::now_v7())),
            Err(SharingError::ScopeUnavailable)
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
