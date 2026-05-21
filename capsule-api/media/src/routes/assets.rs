//! Asset media serving endpoints with OpenAPI documentation

use crate::state::AppState;
use derive_more::From;
use entity::asset;
use model::errors::InternalServerError;
use salvo::fs::NamedFile;
use salvo::oapi::extract::{JsonBody, PathParam, QueryParam};
use salvo::prelude::*;
use sea_orm::EntityTrait;
use serde::{Deserialize, Serialize};
use service::storage::{StorageConfig, StorageService};
use std::str::FromStr;
use uuid::Uuid;

// TODO: authorization via access token for asset routes

// ============================================================================
// Request/Response Types
// ============================================================================

/// Query parameters for media transformation
#[allow(dead_code)]
#[derive(Debug, Deserialize, ToParameters, ToSchema)]
#[salvo(parameters(default_parameter_in = Query))]
pub struct MediaQueryParams {
    /// Max width
    #[salvo(parameter(required = false))]
    pub w: Option<u32>,
    /// Max height
    #[salvo(parameter(required = false))]
    pub h: Option<u32>,
    /// Quality (1-100)
    #[salvo(parameter(required = false))]
    pub q: Option<u8>,
    /// Output format
    #[salvo(parameter(required = false))]
    pub f: Option<String>,
}

/// Batch download request
#[allow(dead_code)]
#[derive(Debug, Deserialize, ToSchema)]
pub struct BatchDownloadRequest {
    /// Asset IDs to download
    pub asset_ids: Vec<String>,
    /// Include metadata JSON sidecar
    #[serde(default)]
    pub include_metadata: bool,
    /// Quality option
    pub quality: Option<String>,
}

/// Batch download response
#[derive(Debug, Serialize, ToSchema)]
pub struct BatchDownloadResponse {
    /// Job ID for tracking
    pub job_id: String,
    /// Current status
    pub status: String,
    /// Estimated size in bytes
    pub estimated_size_bytes: Option<u64>,
}

// ============================================================================
// Endpoint Handlers
// ============================================================================

/// Possible responses for asset serving
#[derive(From, Debug)]
pub enum AssetResponses {
    /// Successful file serving
    Ok(Box<NamedFile>),
    /// Asset or file not found
    NotFound(String),
    /// Internal server error
    InternalServerError(InternalServerError),
}

#[async_trait]
impl Writer for AssetResponses {
    async fn write(mut self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Ok(file) => file.write(req, depot, res).await,
            Self::NotFound(msg) => {
                res.status_code(StatusCode::NOT_FOUND);
                res.render(Json(ErrorResponse { error: msg }));
            }
            Self::InternalServerError(e) => {
                // Delegate to InternalServerError's Writer impl for consistent obfuscation
                e.write(req, depot, res).await;
            }
        }
    }
}

impl salvo::oapi::EndpointOutRegister for AssetResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Success").add_content(
                "application/octet-stream",
                salvo::oapi::Content::new(String::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("404"),
            salvo::oapi::Response::new("Asset not found"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal server error"),
        );
    }
}

/// Helper to serve asset file
async fn serve_asset_file(depot: &mut Depot, asset_id_str: &str) -> AssetResponses {
    let state = match depot.obtain::<AppState>() {
        Ok(s) => s,
        Err(_) => {
            return AssetResponses::InternalServerError(
                eyre::eyre!("Failed to get app state").into(),
            );
        }
    };

    // Fetch asset metadata
    let asset = match asset::Entity::find_by_id(asset_id_str)
        .one(&state.conn)
        .await
    {
        Ok(Some(a)) => a,
        Ok(None) => return AssetResponses::NotFound("Asset not found".to_string()),
        Err(e) => return AssetResponses::InternalServerError(e.into()),
    };

    // Parse UUID for storage path logic
    let uuid = match Uuid::from_str(&asset.id) {
        Ok(u) => u,
        Err(e) => return AssetResponses::InternalServerError(e.into()),
    };

    // Determine path
    let storage = StorageService::new(StorageConfig {
        upload_dir: state.config.upload_dir.clone(),
    });

    // Derive extension from filename or use a default
    let ext = std::path::Path::new(&asset.original_filename)
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("bin");

    let path =
        storage.get_upload_path_by_ids(&uuid, &asset.owner_id, asset.album_id.as_deref(), ext);

    if !path.exists() {
        return AssetResponses::NotFound("File not found on disk".into());
    }

    match NamedFile::builder(path).build().await {
        Ok(f) => AssetResponses::Ok(Box::new(f)),
        Err(e) => AssetResponses::InternalServerError(eyre::eyre!(e).into()),
    }
}

/// Get original asset file
#[endpoint(operation_id = "get_original", tags("media"))]
pub async fn get_original(
    _req: &mut Request,
    depot: &mut Depot,
    asset_id: PathParam<String>,
    _queries: QueryParam<MediaQueryParams, false>,
) -> AssetResponses {
    serve_asset_file(depot, &asset_id.into_inner()).await
}

/// Get asset thumbnail (LQIP)
#[endpoint(operation_id = "get_thumbnail", tags("media"))]
pub async fn get_thumbnail(
    _req: &mut Request,
    depot: &mut Depot,
    asset_id: PathParam<String>,
    _queries: QueryParam<MediaQueryParams, false>,
) -> AssetResponses {
    // For now, serve original as thumbnail fallback
    serve_asset_file(depot, &asset_id.into_inner()).await
}

/// Get asset preview (web quality)
#[endpoint(operation_id = "get_preview", tags("media"))]
pub async fn get_preview(
    _req: &mut Request,
    depot: &mut Depot,
    asset_id: PathParam<String>,
    _queries: QueryParam<MediaQueryParams, false>,
) -> AssetResponses {
    // Fallback to original
    serve_asset_file(depot, &asset_id.into_inner()).await
}

/// Get asset as download
#[endpoint(operation_id = "get_download", tags("media"))]
pub async fn get_download(
    _req: &mut Request,
    depot: &mut Depot,
    asset_id: PathParam<String>,
) -> AssetResponses {
    serve_asset_file(depot, &asset_id.into_inner()).await
}

/// Get video stream
#[endpoint(operation_id = "get_stream", tags("media"))]
pub async fn get_stream(
    _req: &mut Request,
    depot: &mut Depot,
    asset_id: PathParam<String>,
) -> AssetResponses {
    serve_asset_file(depot, &asset_id.into_inner()).await
}

/// Possible responses for batch download
#[allow(dead_code)]
#[derive(From, Debug)]
pub enum BatchDownloadResponses {
    /// Job created successfully
    Ok(BatchDownloadResponse),
    /// Unauthorized access
    #[from(ignore)]
    Unauthorized(String),
    /// Internal server error
    #[from(ignore)]
    InternalServerError(String),
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

#[async_trait]
impl Writer for BatchDownloadResponses {
    async fn write(mut self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Ok(data) => {
                res.status_code(StatusCode::OK);
                Json(data).write(req, depot, res).await;
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Json(ErrorResponse { error: msg }));
            }
            Self::InternalServerError(msg) => {
                res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                res.render(Json(ErrorResponse { error: msg }));
            }
        }
    }
}

impl salvo::oapi::EndpointOutRegister for BatchDownloadResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Job created successfully").add_content(
                "application/json",
                salvo::oapi::Content::new(BatchDownloadResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Unauthorized"),
        );
        operation.responses.insert(
            String::from("500"),
            salvo::oapi::Response::new("Internal Server Error"),
        );
    }
}

/// Request batch download (creates zip job)
#[endpoint(
    operation_id = "batch_download",
    tags("media"),
    // security(("bearer" = []))
)]
pub async fn batch_download(
    _req: &mut Request,
    _depot: &mut Depot,
    _body: JsonBody<BatchDownloadRequest>,
) -> BatchDownloadResponses {
    // TODO: Implement batch download with job queue
    todo!("Implement batch download with job queue")
}
