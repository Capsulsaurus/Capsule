use async_graphql::*;
use sea_orm::DatabaseConnection;
use service::stack::Query as StackService;

use super::types::AssetStack;

#[derive(Default)]
pub struct StackQuery;

#[Object]
impl StackQuery {
    /// Get a stack by its ID
    async fn stack(&self, ctx: &Context<'_>, id: ID) -> Result<Option<AssetStack>> {
        let db = ctx.data::<DatabaseConnection>()?;
        let stack = StackService::find_by_id(db, &id).await?;
        Ok(stack.map(|model| AssetStack { model }))
    }

    /// Find the stack containing a specific asset
    async fn stack_by_asset(&self, ctx: &Context<'_>, asset_id: ID) -> Result<Option<AssetStack>> {
        let db = ctx.data::<DatabaseConnection>()?;
        let stack = StackService::find_by_member_asset(db, &asset_id).await?;
        Ok(stack.map(|model| AssetStack { model }))
    }
}
