use data_encoding::{BASE64, HEXLOWER};
use entity::public_share;
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set};

use super::{PublishShare, ShareError, StoredAsset, StoredMetadata, encode_wrapped};

pub struct Mutation;

impl Mutation {
    /// Publish (register) a share link — the server half of the issuer's Provision step. The
    /// `WrappedScope` and each asset's sidecar are stored **opaquely** (canonical CBOR, base64);
    /// the server never opens the material. Returns the owner-held revocation handle (`link_id`).
    #[tracing::instrument(skip(db, share), fields(owner_id = %share.owner_id, opaque_id = %share.opaque_id))]
    pub async fn publish_share(
        db: &DatabaseConnection,
        share: PublishShare,
    ) -> Result<String, ShareError> {
        let wrapped_scope = encode_wrapped(&share.wrapped_scope)?;
        let passphrase_protected = share.wrapped_scope.is_passphrase_protected();

        let assets = share
            .assets
            .iter()
            .map(|a| StoredAsset {
                asset_id: a.asset_id.clone(),
                content_hash: a.content_hash.clone(),
                content_type: a.content_type.clone(),
                size: a.size,
                sidecar_cbor_b64: BASE64.encode(&a.sidecar.to_canonical_vec()),
                nonce_prefix_hex: HEXLOWER.encode(&a.nonce_prefix),
                amk_version: a.amk_version,
            })
            .collect();
        let served_metadata = serde_json::to_value(StoredMetadata { assets })
            .map_err(|_| ShareError::Encoding("served_metadata serialize"))?;

        let link_id = uuid::Uuid::now_v7().to_string();
        public_share::ActiveModel {
            link_id: Set(link_id.clone()),
            opaque_id: Set(share.opaque_id),
            owner_id: Set(share.owner_id),
            home_server: Set(share.home_server),
            scope_kind: Set(share.scope_kind),
            scope_id: Set(share.scope_id),
            wrapped_scope: Set(wrapped_scope),
            passphrase_protected: Set(passphrase_protected),
            served_metadata: Set(served_metadata),
            expires_at: Set(share.expires_at.map(entity::time::ts_to_entity)),
            revoked_at: Set(None),
            created_at: Set(entity::time::now_entity()),
        }
        .insert(db)
        .await?;
        tracing::info!(%link_id, "share link published");
        Ok(link_id)
    }

    /// Revoke a share link (idempotent — a second revoke is a no-op). The serve path refuses it
    /// within its fail-closed cache window. Returns `false` if no such link is owned by `owner_id`.
    #[tracing::instrument(skip(db))]
    pub async fn revoke_share(
        db: &DatabaseConnection,
        owner_id: &str,
        link_id: &str,
    ) -> Result<bool, ShareError> {
        let Some(link) = public_share::Entity::find_by_id(link_id)
            .filter(public_share::Column::OwnerId.eq(owner_id))
            .one(db)
            .await?
        else {
            return Ok(false);
        };
        if link.revoked_at.is_none() {
            let mut am: public_share::ActiveModel = link.into();
            am.revoked_at = Set(Some(entity::time::now_entity()));
            am.update(db).await?;
            tracing::info!(link_id, "share link revoked");
        }
        Ok(true)
    }
}
