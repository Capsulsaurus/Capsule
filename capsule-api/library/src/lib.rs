use std::sync::Arc;

use async_graphql::http::GraphiQLSource;
use async_graphql::{Request as GraphQLRequest, Response as GraphQLResponse};
use auth::config::AuthConfig;
use auth::service::AuthService;
use config::GraphqlServerConfig;
use context::{AppContext, DbContext, UserContext};
use eyre::Result;
use loaders::Loaders;
use salvo::prelude::*;
use schema::{AppSchema, create_schema};
use sea_orm::DatabaseConnection;
use state::AppState;
use tracing::trace;

mod config;
mod constants;
mod context;
pub mod loaders;
pub mod schema;
mod state;

/// GraphQL handler for Salvo
#[handler]
async fn graphql_handler(req: &mut Request, depot: &mut Depot, res: &mut Response) {
    let state = depot.obtain::<AppState>().expect("AppState not found");

    // Get headers for user context
    let headers = req.headers();
    let user_context = match UserContext::from_salvo_headers(headers, &state.auth_service) {
        Ok(ctx) => ctx,
        Err(e) => {
            let error_response = GraphQLResponse::from_errors(vec![e.into()]);
            res.render(Json(error_response));
            return;
        }
    };
    trace!("User context created: {:?}", user_context);

    // Parse GraphQL request from body
    let body = match req.parse_json::<GraphQLRequest>().await {
        Ok(request) => request,
        Err(e) => {
            res.status_code(StatusCode::BAD_REQUEST);
            res.render(Json(serde_json::json!({
                "error": format!("Failed to parse GraphQL request: {}", e)
            })));
            return;
        }
    };

    // Add app context to request
    let request = body.data(AppContext {
        user: user_context,
        db: DbContext {
            conn: state.conn.clone(),
        },
    });

    // Execute and respond
    let response = state.schema.execute(request).await;
    res.render(Json(response));
}

/// GraphiQL playground handler
#[handler]
async fn graphiql_handler(res: &mut Response) {
    let html = GraphiQLSource::build()
        .endpoint("/v1/library")
        .title("Capsule API")
        .finish();
    res.render(Text::Html(html));
}

pub async fn get_router<C: Into<GraphqlServerConfig>>(
    conn: DatabaseConnection,
    config: C,
    auth_config: AuthConfig,
) -> Result<Router> {
    let config = config.into();
    // Create loaders
    let loaders = Loaders::new(conn.clone());

    // Build GraphQL schema
    let schema: AppSchema = create_schema(loaders);

    // Create auth service
    let auth_service = AuthService::new(conn.clone(), auth_config);

    // Define state
    let state = AppState {
        schema,
        conn,
        config,
        auth_service: Arc::new(auth_service),
    };

    // Build router
    let mut router = Router::new()
        .hoop(affix_state::inject(state))
        .get(graphql_handler)
        .post(graphql_handler);

    #[cfg(debug_assertions)]
    {
        router = router.push(Router::with_path("playground").get(graphiql_handler));
    }

    Ok(router)
}
