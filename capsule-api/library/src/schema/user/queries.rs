use super::{User, statistics::UserStatisticsQuery};
use async_graphql::*;
use chrono::Utc;
pub struct UserQuery {
    pub statistics: UserStatisticsQuery,
}

#[Object]
impl UserQuery {
    async fn get_user(&self, _ctx: &Context<'_>, _id: ID) -> Result<User> {
        // TODO: Implement
        Ok(User {
            id: "123".to_string(),
            username: "testuser".to_string(),
            name: "Test User".to_string(),
            email: "test@test.com".to_string(),
            account_verified: true,
            is_admin: false,
            needs_onboarding: false,
            created_at: Utc::now(),
            modified_at: Utc::now(),
            deleted_at: None,
            registered_via: None,
        })
    }

    async fn list_users(&self, _ctx: &Context<'_>) -> Result<Vec<User>> {
        todo!()
    }

    async fn me(&self, _ctx: &Context<'_>) -> Result<Option<User>> {
        // let auth_service = ctx.data::<YourAuthService>()?;

        // // Get authorization header from context
        // let token = ctx.data::<String>()?;

        // auth_service.get_current_user(token).await

        todo!()
    }

    /// Get current user's statistics
    async fn statistics(&self) -> &UserStatisticsQuery {
        // TODO: Implement auth for either yourself or admin
        &self.statistics
    }
}
