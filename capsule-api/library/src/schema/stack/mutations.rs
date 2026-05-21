use async_graphql::*;
use sea_orm::DatabaseConnection;
use service::stack::Mutation as StackMutationType;

use super::types::{AddStackMemberInput, AssetStack, CreateStackInput, StackMember};
use crate::context::UserContext;

#[derive(Default)]
pub struct StackMutation;

#[Object]
impl StackMutation {
    /// Create a new stack
    async fn create_stack(
        &self,
        ctx: &Context<'_>,
        _input: CreateStackInput,
    ) -> Result<AssetStack> {
        let _db = ctx.data::<DatabaseConnection>()?;
        let _user = ctx.data::<UserContext>()?;

        // TODO: Validate user owns the assets provided in member_ids

        // Hacky: Assuming first asset is primary for initial creation logic if not explicitly handled
        // in service layer perfectly matching this input structure yet.
        // The service needs `member_assets` tuple vector.
        // For MVP, we'll map input to service args.

        todo!("Implement create_stack mutation logic mapping input to service call")
    }

    /// Add a member to an existing stack
    async fn add_stack_member(
        &self,
        _ctx: &Context<'_>,
        _input: AddStackMemberInput,
    ) -> Result<StackMember> {
        todo!("Implement add_stack_member logic")
    }

    /// Remove a member from a stack
    async fn remove_stack_member(
        &self,
        ctx: &Context<'_>,
        stack_id: ID,
        asset_id: ID,
    ) -> Result<bool> {
        let db = ctx.data::<DatabaseConnection>()?;
        // TODO: Check ownership

        StackMutationType::remove_member(db, &stack_id, &asset_id).await?;
        Ok(true)
    }

    /// Set the primary asset (hero) of a stack
    async fn set_stack_primary(
        &self,
        ctx: &Context<'_>,
        stack_id: ID,
        asset_id: ID,
    ) -> Result<AssetStack> {
        let db = ctx.data::<DatabaseConnection>()?;
        let stack = StackMutationType::set_primary_asset(db, &stack_id, &asset_id).await?;
        Ok(AssetStack { model: stack })
    }

    /// Set collapsed state of a stack
    async fn set_stack_collapsed(
        &self,
        ctx: &Context<'_>,
        stack_id: ID,
        collapsed: bool,
    ) -> Result<AssetStack> {
        let db = ctx.data::<DatabaseConnection>()?;
        let stack = StackMutationType::set_collapsed(db, &stack_id, collapsed).await?;
        Ok(AssetStack { model: stack })
    }

    /// Delete a stack (unstack assets)
    async fn delete_stack(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let db = ctx.data::<DatabaseConnection>()?;
        // TODO: Check ownership

        let res = StackMutationType::delete_stack(db, &id).await?;
        Ok(res.rows_affected > 0)
    }
}
