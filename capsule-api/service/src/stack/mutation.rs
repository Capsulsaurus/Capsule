use ::entity::{
    asset::{self, Entity as Asset},
    asset_stack::{self, Entity as AssetStack, StackType},
    stack_member::{self, Entity as StackMember, MemberRole},
};
use chrono::Utc;
use nanoid::nanoid;
use sea_orm::{prelude::Expr, *};

pub struct Mutation;

impl Mutation {
    /// Create a new stack with initial members
    pub async fn create_stack(
        db: &impl ConnectionTrait,
        owner_id: String,
        stack_type: StackType,
        primary_asset_id: String,
        cover_asset_id: Option<String>,
        metadata: Option<String>,
        member_assets: Vec<(String, i32, MemberRole, Option<String>)>, // (asset_id, order, role, metadata)
    ) -> Result<asset_stack::Model, DbErr> {
        let stack_id = nanoid!();

        let metadata_json = match metadata {
            Some(s) => Some(
                serde_json::from_str(&s)
                    .map_err(|e| DbErr::Custom(format!("Invalid stack metadata JSON: {}", e)))?,
            ),
            None => None,
        };

        // Create stack
        let stack = asset_stack::ActiveModel {
            id: Set(stack_id.clone()),
            owner_id: Set(owner_id),
            stack_type: Set(stack_type),
            primary_asset_id: Set(primary_asset_id.clone()),
            cover_asset_id: Set(cover_asset_id),
            metadata: Set(metadata_json),
            is_collapsed: Set(true),
            is_auto_generated: Set(true),
            created_at: Set(Utc::now()),
            modified_at: Set(Utc::now()),
        };
        let stack = stack.insert(db).await?;

        // Add members
        for (asset_id, order, role, meta) in member_assets {
            Self::add_member_internal(db, &stack_id, asset_id, order, role, meta).await?;
        }

        Ok(stack)
    }

    /// Add a member to existing stack
    pub async fn add_member(
        db: &impl ConnectionTrait,
        stack_id: &str,
        asset_id: String,
        sequence_order: i32,
        member_role: MemberRole,
        metadata: Option<String>,
    ) -> Result<stack_member::Model, DbErr> {
        Self::add_member_internal(
            db,
            stack_id,
            asset_id,
            sequence_order,
            member_role,
            metadata,
        )
        .await
    }

    async fn add_member_internal(
        db: &impl ConnectionTrait,
        stack_id: &str,
        asset_id: String,
        sequence_order: i32,
        member_role: MemberRole,
        metadata: Option<String>,
    ) -> Result<stack_member::Model, DbErr> {
        // Update asset's stack_id and is_stack_hidden
        let is_primary = member_role == MemberRole::Primary;
        let asset_model = Asset::find_by_id(&asset_id)
            .one(db)
            .await?
            .ok_or_else(|| DbErr::Custom(format!("Asset {} not found", asset_id)))?;

        let mut asset: asset::ActiveModel = asset_model.into();
        asset.stack_id = Set(Some(stack_id.to_string()));
        // If it's primary, it's NOT hidden. If it's not primary, it IS hidden.
        asset.is_stack_hidden = Set(!is_primary);
        asset.update(db).await?;

        // Create membership
        let metadata_json = match metadata {
            Some(s) => Some(
                serde_json::from_str(&s)
                    .map_err(|e| DbErr::Custom(format!("Invalid member metadata JSON: {}", e)))?,
            ),
            None => None,
        };

        let member = stack_member::ActiveModel {
            id: Set(nanoid!()),
            stack_id: Set(stack_id.to_string()),
            asset_id: Set(asset_id),
            sequence_order: Set(sequence_order),
            member_role: Set(member_role),
            metadata: Set(metadata_json),
            created_at: Set(Utc::now()),
        };
        member.insert(db).await
    }

    /// Remove member from stack
    pub async fn remove_member(
        db: &impl ConnectionTrait,
        stack_id: &str,
        asset_id: &str,
    ) -> Result<(), DbErr> {
        // Clear asset's stack reference
        let asset_model = Asset::find_by_id(asset_id)
            .one(db)
            .await?
            .ok_or_else(|| DbErr::Custom("Asset not found".to_string()))?;

        let mut asset: asset::ActiveModel = asset_model.into();
        asset.stack_id = Set(None);
        asset.is_stack_hidden = Set(false);
        asset.update(db).await?;

        // Delete membership
        StackMember::delete_many()
            .filter(stack_member::Column::StackId.eq(stack_id))
            .filter(stack_member::Column::AssetId.eq(asset_id))
            .exec(db)
            .await?;

        Ok(())
    }

    /// Set primary asset of stack
    pub async fn set_primary_asset(
        db: &impl ConnectionTrait,
        stack_id: &str,
        new_primary_id: &str,
    ) -> Result<asset_stack::Model, DbErr> {
        // Get current primary and hide it
        let stack = AssetStack::find_by_id(stack_id)
            .one(db)
            .await?
            .ok_or_else(|| DbErr::Custom("Stack not found".to_string()))?;

        // Hide old primary
        let old_primary_model = Asset::find_by_id(&stack.primary_asset_id)
            .one(db)
            .await?
            .ok_or_else(|| DbErr::Custom("Old primary not found".to_string()))?;

        let mut old_primary: asset::ActiveModel = old_primary_model.into();
        old_primary.is_stack_hidden = Set(true);
        old_primary.update(db).await?;

        // Show new primary
        let new_primary_model = Asset::find_by_id(new_primary_id)
            .one(db)
            .await?
            .ok_or_else(|| DbErr::Custom("New primary not found".to_string()))?;

        let mut new_primary: asset::ActiveModel = new_primary_model.into();
        new_primary.is_stack_hidden = Set(false);
        new_primary.update(db).await?;

        // Update stack's primary
        let mut stack_active: asset_stack::ActiveModel = stack.into();
        stack_active.primary_asset_id = Set(new_primary_id.to_string());
        stack_active.modified_at = Set(Utc::now());
        stack_active.update(db).await
    }

    /// Toggle stack collapsed state
    pub async fn set_collapsed(
        db: &impl ConnectionTrait,
        stack_id: &str,
        is_collapsed: bool,
    ) -> Result<asset_stack::Model, DbErr> {
        let stack = AssetStack::find_by_id(stack_id)
            .one(db)
            .await?
            .ok_or_else(|| DbErr::Custom("Stack not found".to_string()))?;

        let mut stack_active: asset_stack::ActiveModel = stack.into();
        stack_active.is_collapsed = Set(is_collapsed);
        stack_active.modified_at = Set(Utc::now());
        stack_active.update(db).await
    }

    /// Delete stack (clears asset references, removes memberships)
    pub async fn delete_stack(
        db: &impl ConnectionTrait,
        stack_id: &str,
    ) -> Result<DeleteResult, DbErr> {
        // Clear all member assets' stack references
        Asset::update_many()
            .col_expr(asset::Column::StackId, Expr::value(Option::<String>::None))
            .col_expr(asset::Column::IsStackHidden, Expr::value(false))
            .filter(asset::Column::StackId.eq(stack_id))
            .exec(db)
            .await?;

        // Memberships cascade delete via FK (because stack is deleted)
        // AssetStack table has ON DELETE CASCADE for members?
        // Wait, migration said:
        // fk_stack_members_stack_id: ON DELETE CASCADE
        // So deleting stack deletes members automatically.

        AssetStack::delete_by_id(stack_id).exec(db).await
    }
}
