use salvo::oapi::ToSchema;
use salvo::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, ToSchema, Clone)]
pub struct VersionResponse {
    pub name: String,
    pub version: String,
}

/// Get API version info
#[endpoint(operation_id = "get_version", tags("api"))]
pub async fn get_version() -> Json<VersionResponse> {
    let name = env!("CARGO_PKG_NAME");
    let version = env!("CARGO_PKG_VERSION");

    Json(VersionResponse {
        name: name.to_string(),
        version: version.to_string(),
    })
}
