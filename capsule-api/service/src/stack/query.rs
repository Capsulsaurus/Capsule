use ::entity::{
    asset::{self, Entity as Asset},
    asset_stack::{self, Entity as AssetStack, StackType},
    stack_member::{self, Entity as StackMember},
};
use sea_orm::{prelude::Expr, *};

pub struct Query;

impl Query {
    /// Find stack by ID
    pub async fn find_by_id(
        db: &impl ConnectionTrait,
        id: &str,
    ) -> Result<Option<asset_stack::Model>, DbErr> {
        AssetStack::find_by_id(id).one(db).await
    }

    /// Find stack by primary asset ID
    pub async fn find_by_primary_asset(
        db: &impl ConnectionTrait,
        asset_id: &str,
    ) -> Result<Option<asset_stack::Model>, DbErr> {
        AssetStack::find()
            .filter(asset_stack::Column::PrimaryAssetId.eq(asset_id))
            .one(db)
            .await
    }

    /// Find stack containing an asset (as any member type)
    pub async fn find_by_member_asset(
        db: &impl ConnectionTrait,
        asset_id: &str,
    ) -> Result<Option<asset_stack::Model>, DbErr> {
        // Optimized: Check asset table first for stack_id
        // This is much faster than joining stack_members
        let asset = Asset::find_by_id(asset_id).one(db).await?;

        if let Some(asset) = asset
            && let Some(stack_id) = asset.stack_id
        {
            return AssetStack::find_by_id(stack_id).one(db).await;
        }

        Ok(None)
    }

    /// Get all stacks for an owner
    pub async fn find_by_owner(
        db: &impl ConnectionTrait,
        owner_id: &str,
    ) -> Result<Vec<asset_stack::Model>, DbErr> {
        AssetStack::find()
            .filter(asset_stack::Column::OwnerId.eq(owner_id))
            .order_by_desc(asset_stack::Column::CreatedAt)
            .all(db)
            .await
    }

    /// Get all members of a stack (ordered)
    pub async fn get_members(
        db: &impl ConnectionTrait,
        stack_id: &str,
    ) -> Result<Vec<stack_member::Model>, DbErr> {
        StackMember::find()
            .filter(stack_member::Column::StackId.eq(stack_id))
            .order_by_asc(stack_member::Column::SequenceOrder)
            .all(db)
            .await
    }

    /// Check if asset is in any stack
    pub async fn is_asset_in_stack(
        db: &impl ConnectionTrait,
        asset_id: &str,
    ) -> Result<bool, DbErr> {
        let asset = Asset::find_by_id(asset_id).one(db).await?;
        Ok(asset.map(|a| a.stack_id.is_some()).unwrap_or(false))
    }

    // ===== Optimized Queries for UI Views =====

    /// Get all unique stacks represented in an album (for album detail view)
    /// Uses composite index (album_id, stack_id)
    pub async fn find_stacks_in_album(
        db: &impl ConnectionTrait,
        album_id: &str,
    ) -> Result<Vec<asset_stack::Model>, DbErr> {
        // We find assets in the album that have a stack_id, getting unique stack_ids
        let stack_ids: Vec<String> = Asset::find()
            .select_only()
            .column(asset::Column::StackId)
            .filter(asset::Column::AlbumId.eq(album_id))
            .filter(asset::Column::StackId.is_not_null())
            .distinct()
            .into_tuple()
            .all(db)
            .await?;

        // Then fetch the stack models
        // Note: For massive albums, we might want to paginate this, but for now this is efficient enough
        if stack_ids.is_empty() {
            return Ok(Vec::new());
        }

        AssetStack::find()
            .filter(asset_stack::Column::Id.is_in(stack_ids))
            .all(db)
            .await
    }

    /// Get stack counts grouped by type for an owner (for stats/dashboard)
    pub async fn get_stack_counts_by_type(
        db: &impl ConnectionTrait,
        owner_id: &str,
    ) -> Result<Vec<(StackType, i64)>, DbErr> {
        AssetStack::find()
            .select_only()
            .column(asset_stack::Column::StackType)
            .column_as(asset_stack::Column::Id.count(), "count")
            .filter(asset_stack::Column::OwnerId.eq(owner_id))
            .group_by(asset_stack::Column::StackType)
            .into_tuple()
            .all(db)
            .await
    }

    /// Get primary assets for all stacks in owner's library (for grid view optimization)
    /// This helps identifying which assets to show as "Stack Leaders"
    pub async fn get_primary_assets_for_owner(
        db: &impl ConnectionTrait,
        owner_id: &str,
    ) -> Result<Vec<asset::Model>, DbErr> {
        // Find all stacks for owner, join with their primary asset
        let assets: Vec<asset::Model> = Asset::find()
            .join(JoinType::InnerJoin, asset::Relation::Stack.def())
            .filter(asset_stack::Column::OwnerId.eq(owner_id))
            .filter(asset_stack::Column::IsCollapsed.eq(true)) // Only care about collapsed stacks
            .filter(Expr::col(asset::Column::Id).eq(Expr::col(asset_stack::Column::PrimaryAssetId))) // Pivot on primary asset
            .all(db)
            .await?;

        Ok(assets)
    }
}
