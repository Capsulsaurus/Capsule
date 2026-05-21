use ::entity::asset::{self, AssetType};
use chrono::{DateTime, Utc};
use nanoid::nanoid;
use sea_orm::*;

pub struct Mutation;

impl Mutation {
    /// Create asset with uploaded=false (upload in progress)
    #[allow(clippy::too_many_arguments)]
    pub async fn create_pending(
        db: &impl ConnectionTrait,
        owner_id: String,
        upload_user_id: String,
        album_id: Option<String>,
        asset_type: AssetType,
        original_filename: String,
        file_size: i64,
        file_hash: String,
        content_type: String,
        captured_at: Option<DateTime<Utc>>,
    ) -> Result<asset::Model, DbErr> {
        let model = asset::ActiveModel {
            id: Set(nanoid!()),
            owner_id: Set(owner_id),
            upload_user_id: Set(upload_user_id),
            album_id: Set(album_id),
            width: Set(0),  // Will be updated on finalize
            height: Set(0), // Will be updated on finalize
            asset_type: Set(asset_type),
            original_filename: Set(original_filename),
            file_size: Set(file_size),
            file_hash: Set(file_hash),
            content_type: Set(content_type),
            captured_at: Set(captured_at),
            uploaded_at: Set(Utc::now()),
            modified_at: Set(Utc::now().into()),
            uploaded: Set(false),
            ..Default::default()
        };
        model.insert(db).await
    }

    /// Mark asset as uploaded and update server-extracted metadata
    pub async fn mark_uploaded(
        db: &impl ConnectionTrait,
        asset_id: &str,
        width: i32,
        height: i32,
        captured_at: Option<DateTime<Utc>>,
    ) -> Result<asset::Model, DbErr> {
        let asset = asset::Entity::find_by_id(asset_id)
            .one(db)
            .await?
            .ok_or_else(|| DbErr::Custom("Asset not found".to_string()))?;

        let mut model: asset::ActiveModel = asset.into();
        model.uploaded = Set(true);
        model.width = Set(width);
        model.height = Set(height);
        if captured_at.is_some() {
            model.captured_at = Set(captured_at);
        }
        model.modified_at = Set(Utc::now().into());
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
        model.deleted_at = Set(Some(Utc::now()));
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
