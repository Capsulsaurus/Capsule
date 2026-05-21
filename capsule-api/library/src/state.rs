use std::sync::Arc;

use auth::service::AuthService;
use sea_orm::DatabaseConnection;

use crate::{config::GraphqlServerConfig, schema::AppSchema};

#[derive(Clone)]
pub struct AppState {
    pub schema: AppSchema,
    pub conn: DatabaseConnection,
    #[allow(dead_code)]
    pub config: GraphqlServerConfig,
    pub auth_service: Arc<AuthService>,
}
