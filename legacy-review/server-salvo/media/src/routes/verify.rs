//! Storage verification — `POST /storage/verify` (contract skeleton; slice `S-C3` in the
//! repo-root `SLICES.md`; SSoT: <https://docs/design/import/storage-verification/>).
//!
//! The key-free durability query: for each asset, confirm every declared blob is
//! **stored** (present in `blobs/` at its content address), **indexed** (a committed
//! `uploaded = true` row references it), and **retrievable** (refcount > 0, not mid-GC,
//! not quarantined). The verdict gates every destructive local cleanup on clients (the
//! verify-before-destroy rule; the client-side predicate is
//! `capsule_core::library::release_is_safe`). The request is a read and writes no state.

use salvo::oapi::extract::JsonBody;
use salvo::prelude::*;
use serde::{Deserialize, Serialize};

/// One asset to verify: the exact blob hashes the client is relying on.
#[derive(Debug, Deserialize, ToSchema)]
#[allow(dead_code)]
pub(super) struct AssetVerifyRequest {
    /// The asset id.
    pub asset_id: String,
    /// Content addresses (hex) of every blob the client relies on.
    pub blob_hashes: Vec<String>,
}

/// The `POST /storage/verify` request body.
#[derive(Debug, Deserialize, ToSchema)]
#[allow(dead_code)]
pub(super) struct StorageVerifyRequest {
    /// The assets to verify.
    pub assets: Vec<AssetVerifyRequest>,
    /// Re-read and re-hash blob bytes instead of trusting stat + index (rate-limited and
    /// coalesced server-side).
    #[serde(default)]
    pub deep: bool,
}

/// One blob's verdict.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct BlobVerdictResponse {
    /// The declared content address (hex).
    pub hash: String,
    /// `original | metadata | derivative | provenance` (closed enum).
    pub role: String,
    /// Present in the blob store at its content address.
    pub stored: bool,
    /// Referenced by a committed, `uploaded = true` row.
    pub indexed: bool,
    /// Refcount > 0, not `collectable_since`, not quarantined.
    pub retrievable: bool,
}

/// One asset's verdict.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct StorageVerdictResponse {
    /// The asset id.
    pub asset_id: String,
    /// All required blobs stored ∧ indexed ∧ retrievable.
    pub durable: bool,
    /// Per-blob detail, one entry per declared hash (a hash the server does not associate
    /// with the asset comes back `stored=false, indexed=false` — never silently omitted).
    pub blobs: Vec<BlobVerdictResponse>,
    /// The server's trusted clock at verification (RFC 3339).
    pub checked_at: String,
}

/// The `POST /storage/verify` response body.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct StorageVerifyResponse {
    /// One verdict per requested asset.
    pub verdicts: Vec<StorageVerdictResponse>,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
}

/// Possible responses for storage verification.
#[allow(dead_code)]
pub(super) enum StorageVerifyResponses {
    /// Verdicts computed (a non-durable asset is still a 200 — the verdict carries it).
    Ok(StorageVerifyResponse),
    /// Structurally invalid request (unknown asset id shape, malformed hash).
    BadRequest(String),
}

#[async_trait]
impl Writer for StorageVerifyResponses {
    async fn write(mut self, req: &mut Request, depot: &mut Depot, res: &mut Response) {
        match self {
            Self::Ok(data) => {
                res.status_code(StatusCode::OK);
                Json(data).write(req, depot, res).await;
            }
            Self::BadRequest(msg) => {
                res.status_code(StatusCode::BAD_REQUEST);
                res.render(Json(ErrorResponse { error: msg }));
            }
        }
    }
}

impl EndpointOutRegister for StorageVerifyResponses {
    fn register(components: &mut salvo::oapi::Components, operation: &mut salvo::oapi::Operation) {
        operation.responses.insert(
            String::from("200"),
            salvo::oapi::Response::new("Per-asset durability verdicts").add_content(
                "application/json",
                salvo::oapi::Content::new(StorageVerifyResponse::to_schema(components)),
            ),
        );
        operation.responses.insert(
            String::from("400"),
            salvo::oapi::Response::new("Structurally invalid request"),
        );
    }
}

/// Batch-confirm that assets' blobs are stored, indexed, and retrievable on this server.
#[endpoint(operation_id = "storage_verify", tags("storage"), security(("bearer" = [])))]
pub async fn storage_verify(
    _req: &mut Request,
    _depot: &mut Depot,
    _body: JsonBody<StorageVerifyRequest>,
) -> StorageVerifyResponses {
    todo!("S-C3: storage-verification endpoint — see SLICES.md")
}
