//! View-only share links — the [`ShareLinkIssuer`] half of the workspace and the scope
//! material it encapsulates.

use capsule_core::crypto::provenance::KeyMode;
use capsule_core::sharing::{
    LINK_SECRET_LEN, RevocationRecord, ScopeMaterial, ShareLink, ShareLinkId, ShareLinkIssuer,
    ShareLinkRecord, SharePublication, ShareScope, SharedAsset, SharingError, encapsulate_scope,
    generate_opaque_id, publish_share,
};
use capsule_core::sidecar::{ExportPrivacy, sidecar_for_export};
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
    /// Each record carries the asset's sidecar as it may cross the boundary: the
    /// [Privacy on Export] strip ([`sidecar_for_export`]) under `privacy`, the per-export
    /// choice the caller passes for this one link. The workspace never stores it, and its own
    /// sidecar is untouched.
    ///
    /// # Errors
    ///
    /// [`SharingError::ScopeUnavailable`] when the scope is not managed by this workspace, or an
    /// asset-scoped asset is in trash.
    ///
    /// [Privacy on Export]: https://docs/design/metadata/#privacy-on-export
    pub fn shared_assets(
        &self,
        scope: ShareScope,
        privacy: ExportPrivacy,
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
                sidecar: Some(sidecar_for_export(&asset.sidecar, privacy).to_canonical_vec()),
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
    /// `privacy` is this export's [Privacy on Export] choice: every covered sidecar is stripped
    /// under it before the record is sealed, so the blob the server stores is already stripped.
    /// [`ExportPrivacy::default`] strips every row; an opt-in applies to this link only and is
    /// never stored.
    ///
    /// # Errors
    ///
    /// What [`shared_assets`](Self::shared_assets) and [`publish_share`] return.
    ///
    /// [Privacy on Export]: https://docs/design/metadata/#privacy-on-export
    #[tracing::instrument(skip(self, link), fields(opaque_id = %link.opaque_id_hex()))]
    pub fn publish_link(
        &self,
        link: &ShareLink,
        privacy: ExportPrivacy,
    ) -> std::result::Result<SharePublication, SharingError> {
        if privacy != ExportPrivacy::default() {
            tracing::info!(
                ?privacy,
                "share link: the issuer opted in to retain metadata"
            );
        }
        let assets = self.shared_assets(link.scope, privacy)?;
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
mod tests;
