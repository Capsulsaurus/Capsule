//! Public share access endpoint

use salvo::oapi::extract::PathParam;
use salvo::prelude::*;
use serde::Serialize;

// TODO: Remember to delete share links if underlying asset is deleted
// Whether asset is deleted or unshared, just give a generic 404 indicating asset not found.

/// Shared content response
#[derive(Debug, Serialize, ToSchema)]
pub struct SharedContentResponse {
    /// Type of shared content
    pub content_type: String,
    /// Expiry timestamp
    pub expires_at: Option<String>,
}

/// Possible responses for shared content access
#[allow(dead_code)]
pub enum SharedContentResponses {
    /// Successful retrieval
    Ok(SharedContentResponse),
    /// Shared content not found or expired
    NotFound(String),
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[async_trait]
impl Writer for SharedContentResponses {
    async fn write(mut self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Ok(data) => {
                res.status_code(StatusCode::OK);
                Json(data).write(req, depot, res).await;
            }
            Self::NotFound(msg) => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(ErrorResponse { error: msg }));
            }
        }
    }
}

impl salvo::oapi::EndpointOutRegister for SharedContentResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Successful retrieval").add_content(
                "application/json",
                salvo::oapi::Content::new(SharedContentResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("Shared content not found"),
        );
    }
}

/// Access shared content via token
#[endpoint(operation_id = "get_shared_content", tags("share"))]
pub async fn get_shared_content(
    _req: &mut Request,
    _depot: &mut Depot,
    _token: PathParam<String>,
) -> SharedContentResponses {
    // TODO: Implement share access
    todo!("Implement share access")
}
