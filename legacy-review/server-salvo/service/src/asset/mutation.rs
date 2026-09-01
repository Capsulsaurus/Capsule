use ::entity::asset::{self, AssetType};
use ::entity::time;
use nanoid::nanoid;
use sea_orm::*;

pub struct Mutation;

impl Mutation {
    /// Create asset with uploaded=false (upload in progress).
    ///
    /// The row is the key-free reservation of the asset id the bundle's blobs reference —
    /// no plaintext dimensions, filename, or capture date (retired in S-G3; that metadata
    /// lives inside the encrypted metadata blob, readable only by authorized clients).
    #[allow(clippy::too_many_arguments)]
    pub async fn create_pending(
        db: &impl ConnectionTrait,
        owner_id: String,
        upload_user_id: String,
        album_id: Option<String>,
        asset_type: AssetType,
        file_size: i64,
        file_hash: String,
        content_type: String,
    ) -> Result<asset::Model, DbErr> {
        let model = asset::ActiveModel {
            id: Set(nanoid!()),
            owner_id: Set(owner_id),
            upload_user_id: Set(upload_user_id),
            album_id: Set(album_id),
            asset_type: Set(asset_type),
            file_size: Set(file_size),
            file_hash: Set(file_hash),
            content_type: Set(content_type),
            uploaded_at: Set(time::now_entity()),
            modified_at: Set(time::now_entity().into()),
            uploaded: Set(false),
            ..Default::default()
        };
        model.insert(db).await
    }

    /// Mark asset as uploaded. The server extracts no plaintext metadata (it holds no key);
    /// finalization only flips the `uploaded` flag and advances the server-visible clock.
    pub async fn mark_uploaded(
        db: &impl ConnectionTrait,
        asset_id: &str,
    ) -> Result<asset::Model, DbErr> {
        let asset = asset::Entity::find_by_id(asset_id)
            .one(db)
            .await?
            .ok_or_else(|| DbErr::Custom("Asset not found".to_string()))?;

        let mut model: asset::ActiveModel = asset.into();
        model.uploaded = Set(true);
        model.modified_at = Set(time::now_entity().into());
        model.update(db).await
    }

    /// Soft delete asset (move to trash)
    pub async fn soft_delete(
        db: &impl ConnectionTrait,
        asset_id: &str,
    ) -> Result<asset::Model, DbErr> {
        let asset = asset::Entity::find_by_id(asset_id)
            .one(db)
            .await?
            .ok_or_else(|| DbErr::Custom("Asset not found".to_string()))?;

        let mut model: asset::ActiveModel = asset.into();
        model.deleted_at = Set(Some(time::now_entity()));
        model.update(db).await
    }

    /// Restore asset from trash
    pub async fn restore(db: &impl ConnectionTrait, asset_id: &str) -> Result<asset::Model, DbErr> {
        let asset = asset::Entity::find_by_id(asset_id)
            .one(db)
            .await?
            .ok_or_else(|| DbErr::Custom("Asset not found".to_string()))?;

        let mut model: asset::ActiveModel = asset.into();
        model.deleted_at = Set(None);
        model.update(db).await
    }

    /// Delete asset permanently
    pub async fn delete(db: &impl ConnectionTrait, asset_id: &str) -> Result<DeleteResult, DbErr> {
        asset::Entity::delete_by_id(asset_id).exec(db).await
    }
}
