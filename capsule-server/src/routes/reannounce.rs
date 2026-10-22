//! `POST /v1/sync/reannounce` — a client re-announces the signed manifests it holds (#781).
//!
//! After an index rebuild moved a library to a new sync epoch, a client resyncs from zero and
//! sends, for every asset it holds that the feed no longer lists, the chain head's provenance
//! record: manifests only, never bytes. [`crate::sync::reannounce`] decides each one; this is
//! the wire shape.
//!
//! # One answer per manifest, in request order
//!
//! A batch is not all-or-nothing. Each manifest is answered on its own merits — `restored`,
//! `already_indexed`, `missing_blobs` naming what the store lacks, or `refused` with a stable
//! `error.sync.reannounce_*` code — because they are independent assets and one asset a client
//! may not restore says nothing about the next. What refuses the whole request is a request that
//! is not a well-formed question (`400`), and a collaborator that could not answer (`500`):
//! answering the rest of a batch after an outage would leave the client unable to tell which
//! answers were decided.
//!
//! # Statuses
//!
//! | Status | When |
//! | --- | --- |
//! | `200` | every manifest was answered |
//! | `400` (`error.sync.reannounce_invalid`) | no manifests, more than [`MAX_REANNOUNCE_BATCH`], or one that is not base64 |
//! | `401` / `403` | the bearer scheme's |
//! | `500` (`error.sync.unavailable`) | the index, the store, the authority or the ledger could not answer |

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::attestation::AttestationContext;
use crate::auth::AccessToken;
use crate::quota::QuotaContext;
use crate::routes::sync::SyncTag;
use crate::store::UserId;
use crate::sync::reannounce::{self, Answer, MAX_REANNOUNCE_BATCH, Reannounce};
use crate::upload::UploadContext;

// ===========================================================================================
// Wire types
// ===========================================================================================

/// The `POST /v1/sync/reannounce` body.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct ReannounceRequest {
    /// Each asset's chain-head provenance record, base64 of the exact bytes the client holds:
    /// the same bytes the feed's `manifest_cbor` carries and an upload's `provenance` blob is.
    pub manifests: Vec<String>,
}

/// What re-announcing one manifest did.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReannounceOutcome {
    /// The index lacked the row and now holds it.
    Restored,
    /// The index already holds the row. Nothing was written.
    AlreadyIndexed,
    /// The store lacks blobs the row needs, listed in `missing`. Nothing was written: the client
    /// re-uploads them through the transfer protocol.
    MissingBlobs,
    /// Refused, with `code`. Nothing was written.
    Refused,
}

/// One manifest's answer.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReannounceResult {
    /// The asset the manifest names. Absent only when it did not decode far enough to name one.
    pub asset_id: Option<String>,
    /// What happened.
    pub outcome: ReannounceOutcome,
    /// The position a restored row was published at.
    pub sync_seq: Option<u64>,
    /// The content addresses the store does not hold, for `missing_blobs`. Empty otherwise.
    #[serde(default)]
    pub missing: Vec<String>,
    /// The stable `error.sync.reannounce_*` code, for `refused`.
    pub code: Option<String>,
}

/// The `POST /v1/sync/reannounce` response.
#[derive(Schema, Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ReannounceResponse {
    /// One answer per manifest, in request order and never shortened.
    pub results: Vec<ReannounceResult>,
}

// ===========================================================================================
// Rejections
// ===========================================================================================

/// Why no manifest was answered.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum ReannounceRejection {
    /// The request is not a set of manifests this route reads.
    #[error("{detail}")]
    #[problem(status = 400, title = "Invalid request")]
    Invalid {
        /// What was wrong, in English. The client localizes `code`, not this.
        detail: String,
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer, so nothing more was decided.
    #[error("the re-announce could not be decided")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

impl ReannounceRejection {
    fn invalid(detail: impl Into<String>) -> Self {
        Self::Invalid {
            detail: detail.into(),
            code: error_codes::SYNC_REANNOUNCE_INVALID,
        }
    }
}

// ===========================================================================================
// The operation
// ===========================================================================================

/// Restore index rows from the signed manifests a client holds, after a rebuild lost them.
///
/// Idempotent per manifest: a row restored once is `already_indexed` the second time, and
/// nothing is written again.
#[kynos::post(
    "/v1/sync/reannounce",
    operation_id = "reannounce_manifests",
    tag = SyncTag
)]
pub async fn reannounce_manifests(
    Inject(upload): Inject<UploadContext>,
    Inject(attestation): Inject<AttestationContext>,
    Inject(quota): Inject<QuotaContext>,
    Auth(credential): Auth<AccessToken>,
    Json(request): Json<ReannounceRequest>,
) -> Result<Json<ReannounceResponse>, ReannounceRejection> {
    let manifests = parse(&request)?;
    let caller = UserId::new(credential.user.as_str());
    let context = Reannounce {
        upload: &upload,
        attestation: &attestation,
        quotas: quota.quotas(),
    };

    let mut results = Vec::with_capacity(manifests.len());
    for bytes in &manifests {
        let answer = reannounce::reannounce(context, &caller, bytes)
            .await
            .map_err(|error| {
                tracing::error!(%error, %caller, "a re-announce could not be decided");
                ReannounceRejection::Unavailable {
                    code: error_codes::SYNC_UNAVAILABLE,
                }
            })?;
        results.push(render(answer));
    }
    tracing::info!(
        %caller,
        manifests = results.len(),
        restored = results
            .iter()
            .filter(|result| result.outcome == ReannounceOutcome::Restored)
            .count(),
        "answered a re-announce"
    );
    Ok(Json(ReannounceResponse { results }))
}

/// The manifests' bytes, after the request-level checks.
fn parse(request: &ReannounceRequest) -> Result<Vec<Vec<u8>>, ReannounceRejection> {
    if request.manifests.is_empty() {
        return Err(ReannounceRejection::invalid(
            "no manifests were re-announced",
        ));
    }
    if request.manifests.len() > MAX_REANNOUNCE_BATCH {
        return Err(ReannounceRejection::invalid(format!(
            "at most {MAX_REANNOUNCE_BATCH} manifests may be re-announced in one request"
        )));
    }
    request
        .manifests
        .iter()
        .map(|encoded| {
            BASE64.decode(encoded).map_err(|error| {
                ReannounceRejection::invalid(format!("a manifest is not base64: {error}"))
            })
        })
        .collect()
}

/// One answer, as the wire spells it.
fn render(answer: Answer) -> ReannounceResult {
    let empty = |outcome, asset_id: Option<String>| ReannounceResult {
        asset_id,
        outcome,
        sync_seq: None,
        missing: Vec::new(),
        code: None,
    };
    match answer {
        Answer::Restored { asset, sync_seq } => ReannounceResult {
            sync_seq,
            ..empty(ReannounceOutcome::Restored, Some(asset.as_str().to_owned()))
        },
        Answer::AlreadyIndexed { asset } => empty(
            ReannounceOutcome::AlreadyIndexed,
            Some(asset.as_str().to_owned()),
        ),
        Answer::MissingBlobs { asset, missing } => ReannounceResult {
            missing: missing
                .iter()
                .map(|address| address.as_str().to_owned())
                .collect(),
            ..empty(
                ReannounceOutcome::MissingBlobs,
                Some(asset.as_str().to_owned()),
            )
        },
        Answer::Refused { asset, refusal } => ReannounceResult {
            code: Some(refusal.code().to_owned()),
            ..empty(
                ReannounceOutcome::Refused,
                asset.map(|asset| asset.as_str().to_owned()),
            )
        },
    }
}
