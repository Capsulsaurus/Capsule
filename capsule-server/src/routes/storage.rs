//! `POST /v1/storage/verify` — the key-free durability verdict (slice `S-C3`).
//!
//! The question this answers is asked immediately before a destructive action: a client is
//! about to release the only local copy of a photo and wants to know whether the server really
//! holds what it thinks it holds. [`crate::verify`] computes the verdict; this is its wire
//! shape and its refusals.
//!
//! # Two tightenings on the retired surface, both deliberate
//!
//! **Owner-scoped.** The Salvo endpoint answered about any `asset_id` a caller sent. This one
//! answers only about the caller's own; somebody else's asset is indistinguishable from one
//! that never existed.
//!
//! **An empty declaration is a `400`, not a vacuous `durable: true`.** `durable` is a
//! conjunction over the hashes the client declared, so declaring none makes it trivially true —
//! and this verdict gates deletion. A client that sends an empty list has a bug, and the worst
//! possible way to tell it so is to answer "yes, safe to delete".
//!
//! # `S-C28` audit
//!
//! | Salvo status | Verdict |
//! | --- | --- |
//! | `200` | kept. A non-durable asset is still `200` — the verdict carries it, and a refusal would make "not durable" and "could not check" the same thing to a client that switches on status |
//! | `400 error.storage.invalid_request` | kept, and now also covers the empty declaration above |
//! | `401` | kept, and now the framework's |
//! | `429 error.storage.deep_rate_limited` | **deleted as unreachable.** It priced `deep`, and `deep` is not on this surface — see `S-C41` |
//! | `500` | kept, with `error.storage.unavailable` |

use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::auth::AccessToken;
use crate::blob::ContentAddress;
use crate::store::{AssetId, OwnerId};
use crate::verify::{AssetQuery, MAX_ASSETS_PER_REQUEST, MAX_BLOBS_PER_ASSET, VerifyContext};

/// The storage surface: asking whether the server still holds what it said it would.
#[derive(Tag)]
#[tag(
    name = "storage",
    description = "Confirming durability before a client destroys its local copy."
)]
pub struct StorageTag;

// ===========================================================================================
// Wire types
// ===========================================================================================

/// One asset to verify, with the exact copies the client is relying on.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct AssetVerifyRequest {
    /// The asset.
    pub asset_id: String,
    /// Every content address the client would be trusting the server with. The verdict is a
    /// conjunction over exactly these, so a client asks about what it is about to delete.
    pub blob_hashes: Vec<String>,
}

/// The `POST /v1/storage/verify` body.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct StorageVerifyRequest {
    /// The assets to verify.
    pub assets: Vec<AssetVerifyRequest>,
}

/// One declared blob's verdict.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct BlobVerdictResponse {
    /// The address, as the client declared it.
    pub hash: String,
    /// The role the asset holds it under — `unknown` for a hash the asset does not hold.
    pub role: String,
    /// The bytes are present at that address.
    pub stored: bool,
    /// A live asset of the caller's references the address.
    pub indexed: bool,
    /// Nothing is withholding it.
    pub retrievable: bool,
}

/// One asset's verdict.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StorageVerdictResponse {
    /// The asset the client asked about.
    pub asset_id: String,
    /// Every declared blob is stored ∧ indexed ∧ retrievable. **This is the field that gates a
    /// deletion**, so it is false whenever the server cannot say otherwise.
    pub durable: bool,
    /// One entry per declared hash, in declaration order and never shortened.
    pub blobs: Vec<BlobVerdictResponse>,
    /// The server's own clock at verification, RFC 3339. Never the client's.
    pub checked_at: String,
}

/// The `POST /v1/storage/verify` response.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct StorageVerifyResponse {
    /// One verdict per requested asset, in request order.
    pub verdicts: Vec<StorageVerdictResponse>,
}

// ===========================================================================================
// Rejections
// ===========================================================================================

/// Why no verdict was returned.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum VerifyRejection {
    /// The request could not be read as a set of assets and addresses.
    #[error("{detail}")]
    #[problem(status = 400, title = "Invalid request")]
    Invalid {
        /// What was wrong, in English. The client localizes `code`, not this.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer, so nothing was decided.
    ///
    /// Deliberately **not** a `durable: false` verdict. A client told "not durable" keeps its
    /// copy and is safe; the danger is the other direction, and conflating an outage with a
    /// real finding would train a user to ignore the state that matters.
    #[error("the verdict could not be computed")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl VerifyRejection {
    /// The request was not a well-formed question.
    fn invalid(detail: impl Into<String>) -> Self {
        Self::Invalid {
            detail: detail.into(),
            code: error_codes::STORAGE_INVALID_REQUEST,
        }
    }

    /// A collaborator could not answer.
    fn unavailable() -> Self {
        Self::Unavailable {
            code: error_codes::STORAGE_UNAVAILABLE,
        }
    }
}

// ===========================================================================================
// The operation
// ===========================================================================================

/// Confirm that the server holds the copies a client is about to stop holding.
///
/// A pure read: it writes no blob, no index row and no verdict. Soundness against a racing
/// collection comes from the standing GC grace window rather than from a per-request lease,
/// which is why nothing here takes one.
#[kynos::post(
    "/v1/storage/verify",
    operation_id = "verify_storage",
    tag = StorageTag
)]
pub async fn verify_storage(
    Inject(verify): Inject<VerifyContext>,
    Auth(credential): Auth<AccessToken>,
    Json(request): Json<StorageVerifyRequest>,
) -> Result<Json<StorageVerifyResponse>, VerifyRejection> {
    let queries = parse(&request)?;
    // The caller files under itself, exactly as the feed does. There is no on-behalf read here:
    // a verdict about another account's asset is disclosure about another account.
    let owner = OwnerId::new(credential.user.as_str());

    let verdicts = crate::verify::verify(&verify, &owner, &queries)
        .await
        .map_err(|error| {
            tracing::error!(%error, %owner, "a storage verdict could not be computed");
            VerifyRejection::unavailable()
        })?;

    Ok(Json(StorageVerifyResponse {
        verdicts: verdicts
            .into_iter()
            .map(|verdict| StorageVerdictResponse {
                asset_id: verdict.asset_id.to_string(),
                durable: verdict.durable,
                blobs: verdict
                    .blobs
                    .into_iter()
                    .map(|blob| BlobVerdictResponse {
                        hash: blob.hash.as_str().to_owned(),
                        // `unknown` rather than an absent field: a client reading a verdict
                        // position by position must see the same shape for every entry.
                        role: blob.role.map_or("unknown", |role| role.as_str()).to_owned(),
                        stored: blob.stored,
                        indexed: blob.indexed,
                        retrievable: blob.retrievable,
                    })
                    .collect(),
                checked_at: verdict.checked_at.to_string(),
            })
            .collect(),
    }))
}

/// Structurally validate the request and convert it into the engine's query shape.
///
/// Every refusal here is about the *question*, never about the answer: a malformed address
/// cannot be verified, and neither can an empty declaration. Anything that is a well-formed
/// question — including one about an asset that does not exist — gets a verdict.
fn parse(request: &StorageVerifyRequest) -> Result<Vec<AssetQuery>, VerifyRejection> {
    if request.assets.is_empty() {
        return Err(VerifyRejection::invalid("no assets were declared"));
    }
    if request.assets.len() > MAX_ASSETS_PER_REQUEST {
        return Err(VerifyRejection::invalid(format!(
            "at most {MAX_ASSETS_PER_REQUEST} assets may be verified in one request"
        )));
    }

    let mut queries = Vec::with_capacity(request.assets.len());
    for asset in &request.assets {
        if asset.asset_id.trim().is_empty() {
            return Err(VerifyRejection::invalid("an asset id was empty"));
        }
        // A conjunction over nothing is `true`, and this verdict gates a deletion.
        if asset.blob_hashes.is_empty() {
            return Err(VerifyRejection::invalid(format!(
                "asset {} declared no blob hashes, and a verdict over none would be vacuously durable",
                asset.asset_id
            )));
        }
        if asset.blob_hashes.len() > MAX_BLOBS_PER_ASSET {
            return Err(VerifyRejection::invalid(format!(
                "at most {MAX_BLOBS_PER_ASSET} blob hashes may be declared for one asset"
            )));
        }

        let mut blob_hashes = Vec::with_capacity(asset.blob_hashes.len());
        for hash in &asset.blob_hashes {
            let address = ContentAddress::parse(hash).map_err(|error| {
                VerifyRejection::invalid(format!("malformed blob hash: {error}"))
            })?;
            blob_hashes.push(address);
        }
        queries.push(AssetQuery {
            asset_id: AssetId::new(&asset.asset_id),
            blob_hashes,
        });
    }
    Ok(queries)
}
