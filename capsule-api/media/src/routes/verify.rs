//! Storage verification — `POST /storage/verify` (slice `S-C3` in the repo-root
//! `SLICES.md`; SSoT: <https://docs/design/import/storage-verification/>).
//!
//! The key-free durability query: for each asset, confirm every declared blob is
//! **stored** (present in `blobs/` at its content address), **indexed** (a committed
//! `uploaded = true` row references it), and **retrievable** (refcount > 0, not mid-GC,
//! not quarantined). The verdict gates every destructive local cleanup on clients (the
//! verify-before-destroy rule; the client-side predicate is
//! `capsule_core::library::release_is_safe`). The request is a read and writes no state.
//!
//! The signed `StorageAttestation` form of this verdict (`signed: true` + nonce) is owned
//! by slice `S-C15`; this endpoint is the unsigned engine it will wrap.

use auth::utils::headers::validate_user_from_headers;
use capsule_i18n::error_codes;
use model::errors::InternalServerError;
use salvo::oapi::extract::JsonBody;
use salvo::prelude::*;
use serde::{Deserialize, Serialize};

use crate::service::verify::{AssetQuery, AssetVerdict, VerifyError};
use crate::state::AppState;

/// A SHA-256 content address is exactly 64 lowercase-hex characters.
const CONTENT_HASH_LEN: usize = 64;

/// One asset to verify: the exact blob hashes the client is relying on.
#[derive(Debug, Deserialize, ToSchema)]
pub(super) struct AssetVerifyRequest {
    /// The asset id.
    pub asset_id: String,
    /// Content addresses (hex) of every blob the client relies on.
    pub blob_hashes: Vec<String>,
}

/// The `POST /storage/verify` request body.
#[derive(Debug, Deserialize, ToSchema)]
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
    /// `original | metadata | derivative | provenance` — or `unknown` for a hash the server
    /// does not associate with the asset.
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

/// A structured error body carrying the stable `error.*` code clients localize.
#[derive(Serialize)]
struct ErrorResponse {
    code: &'static str,
    error: String,
}

/// Possible responses for storage verification.
pub(super) enum StorageVerifyResponses {
    /// Verdicts computed (a non-durable asset is still a 200 — the verdict carries it).
    Ok(StorageVerifyResponse),
    /// Structurally invalid request (unknown asset id shape, malformed hash).
    BadRequest(String),
    /// Missing / invalid bearer token.
    Unauthorized(String),
    /// The caller exceeded its per-user deep-scan budget.
    RateLimited,
    /// A server-side failure computing the verdict.
    Internal(InternalServerError),
}

impl From<Vec<AssetVerdict>> for StorageVerifyResponse {
    fn from(verdicts: Vec<AssetVerdict>) -> Self {
        Self {
            verdicts: verdicts
                .into_iter()
                .map(|v| StorageVerdictResponse {
                    asset_id: v.asset_id,
                    durable: v.durable,
                    blobs: v
                        .blobs
                        .into_iter()
                        .map(|b| BlobVerdictResponse {
                            hash: b.hash,
                            role: b.role.as_str().to_string(),
                            stored: b.stored,
                            indexed: b.indexed,
                            retrievable: b.retrievable,
                        })
                        .collect(),
                    checked_at: v.checked_at.to_string(),
                })
                .collect(),
        }
    }
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
                res.render(Json(ErrorResponse {
                    code: error_codes::STORAGE_INVALID_REQUEST,
                    error: msg,
                }));
            }
            Self::Unauthorized(msg) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Text::Plain(msg));
            }
            Self::RateLimited => {
                res.status_code(StatusCode::TOO_MANY_REQUESTS);
                res.render(Json(ErrorResponse {
                    code: error_codes::STORAGE_DEEP_RATE_LIMITED,
                    error: "deep-scan rate limit exceeded".to_string(),
                }));
            }
            Self::Internal(e) => {
                e.write(req, depot, res).await;
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
        operation.responses.insert(
            String::from("401"),
            salvo::oapi::Response::new("Missing or invalid bearer token"),
        );
        operation.responses.insert(
            String::from("429"),
            salvo::oapi::Response::new("Deep-scan rate limit exceeded"),
        );
    }
}

/// A content address is exactly 64 lowercase-hex characters.
fn is_valid_content_hash(hash: &str) -> bool {
    hash.len() == CONTENT_HASH_LEN
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Structurally validate + convert the wire request into the service query shape.
fn parse_request(body: StorageVerifyRequest) -> Result<(Vec<AssetQuery>, bool), String> {
    let mut assets = Vec::with_capacity(body.assets.len());
    for asset in body.assets {
        if asset.asset_id.trim().is_empty() {
            return Err("asset_id must be non-empty".to_string());
        }
        for hash in &asset.blob_hashes {
            if !is_valid_content_hash(hash) {
                return Err(format!("malformed content hash: {hash:?}"));
            }
        }
        assets.push(AssetQuery {
            asset_id: asset.asset_id,
            blob_hashes: asset.blob_hashes,
        });
    }
    Ok((assets, body.deep))
}

/// Batch-confirm that assets' blobs are stored, indexed, and retrievable on this server.
#[endpoint(operation_id = "storage_verify", tags("storage"), security(("bearer" = [])))]
pub async fn storage_verify(
    req: &mut Request,
    depot: &mut Depot,
    body: JsonBody<StorageVerifyRequest>,
) -> StorageVerifyResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return StorageVerifyResponses::Unauthorized(e.to_string()),
        };

    let (assets, deep) = match parse_request(body.into_inner()) {
        Ok(parsed) => parsed,
        Err(msg) => return StorageVerifyResponses::BadRequest(msg),
    };

    match state
        .verify
        .verify(&state.conn, &user_id, &assets, deep)
        .await
    {
        Ok(verdicts) => StorageVerifyResponses::Ok(verdicts.into()),
        Err(VerifyError::DeepRateLimited) => StorageVerifyResponses::RateLimited,
        Err(e @ (VerifyError::Db(_) | VerifyError::Hash(_))) => {
            StorageVerifyResponses::Internal(InternalServerError::from(e))
        }
    }
}
