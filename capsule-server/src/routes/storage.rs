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
//! | `429 error.storage.deep_rate_limited` | **reinstated for the signed verdict only.** A throttled `deep` still answers `200` with `deep: rate_limited`; a throttled `attestation_nonce` request is refused whole, because a client that asked for evidence must not silently receive none |
//! | `500` | kept, with `error.storage.unavailable` |

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::storage_attestation::{ATTESTATION_NONCE_LEN, AttestationNonce};
use capsule_i18n::error_codes;
use kynos::prelude::*;
use serde::{Deserialize, Serialize};

use crate::attestation::{AttestationContext, sign_storage_attestation};
use crate::auth::AccessToken;
use crate::blob::ContentAddress;
use crate::counter::{CounterKey, Verdict, unix_seconds};
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
    /// Also re-read and re-hash the bytes (`S-C41`).
    ///
    /// Absent or `false` is the structural check: ask the index and the store whether the bytes
    /// are there. `true` additionally re-hashes them, which is the only way to catch silent
    /// corruption — `stored` is a question about the filesystem, and a corrupt blob is still
    /// stored.
    ///
    /// **Rate-limited per account**, because a deep scan reads and hashes every declared blob
    /// and an unbounded one is an I/O-amplification attack costing the caller one small JSON
    /// body. Past the budget the *structural* verdict still comes back and each blob's `deep`
    /// reads `rate_limited`: throwing away a good structural answer because the optional half
    /// was throttled would make the limiter cost more than it saves.
    #[serde(default)]
    pub deep: bool,
    /// Ask for the **signed** verdict, bound to this nonce: 32 bytes the client drew fresh for
    /// this request, as 64 lowercase hex characters.
    ///
    /// When present the response carries `attestation`, the same verdicts signed under the
    /// server attestation key with this nonce inside the signature, so the client can keep the
    /// answer as evidence and a replayed answer to an earlier request is refused.
    ///
    /// **Rate-limited per account from the same budget as `deep`.** Unlike `deep`, a throttled
    /// signed request is refused whole with `429`: a client that asked for evidence and silently
    /// got an unsigned answer would hold nothing it could later show anyone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation_nonce: Option<String>,
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
    /// What a deep scan found: `intact`, `corrupt`, or `rate_limited` (`S-C41`).
    ///
    /// **Absent when no deep scan ran**, and the absence is load-bearing: it is the difference
    /// between "we did not look at the bytes" and "we looked and they were fine", and a client
    /// deciding whether to release its only copy has to be able to tell those apart.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deep: Option<String>,
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
    /// The signed verdict, present exactly when the request carried `attestation_nonce`: base64
    /// of the canonical-CBOR `StorageAttestation` (`capsule_core::crypto::storage_attestation`).
    ///
    /// The signed bytes are the evidence; `verdicts` is a reading of the same answer for a client
    /// that does not need to keep it. A client verifies the signature under the pinned
    /// attestation key, checks the nonce is the one it sent and that the verdicts answer the
    /// assets and hashes it asked about, then keeps the bytes as they arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation: Option<String>,
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

    /// A signed verdict was asked for and the account's budget is spent.
    ///
    /// Shares the deep scan's budget and code: signing is priced like a deep scan, and the two
    /// are one allowance of expensive verification per account. `retry_after` repeats the
    /// `Retry-After` instant in the body as Unix seconds.
    #[error("too many signed or deep storage checks")]
    #[problem(status = 429, title = "Too many requests")]
    RateLimited {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
        /// When the caller may retry, as Unix seconds.
        #[problem(extension)]
        retry_after: u64,
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
    Inject(counters): Inject<crate::counter::CounterContext>,
    Inject(attestation): Inject<AttestationContext>,
    Auth(credential): Auth<AccessToken>,
    Json(request): Json<StorageVerifyRequest>,
) -> Result<Json<StorageVerifyResponse>, VerifyRejection> {
    let queries = parse(&request)?;
    let nonce = request
        .attestation_nonce
        .as_deref()
        .map(parse_nonce)
        .transpose()?;
    // The caller files under itself, exactly as the feed does. There is no on-behalf read here:
    // a verdict about another account's asset is disclosure about another account.
    let owner = OwnerId::new(credential.user.as_str());

    // The limiter is half the feature, not a refinement of it (`S-C41`): a deep scan reads and
    // re-hashes every declared blob, so an unbounded one lets one authenticated account make
    // the server walk its whole store, repeatedly, for the price of a small JSON body. A signed
    // verdict is priced the same way (transfer/durability.md, Signed Storage Attestation): each
    // one is a hybrid signature the server computes on demand.
    //
    // Charged once per request that asks for either, from one budget. A structural check is
    // cheap and unlimited, and spending the budget on one would throttle the operation clients
    // actually run.
    let limited_until = if request.deep || nonce.is_some() {
        match counters
            .hit(
                &CounterKey::DeepVerify(crate::store::UserId::new(credential.user.as_str())),
                crate::counter::budgets::DEEP_VERIFY,
            )
            .await
            .map_err(|error| {
                // Fail closed, like every other limiter: one that an attacker turns off by
                // loading the counter store is not a limiter.
                tracing::error!(%error, %owner, "the deep-verify limiter could not be reached");
                VerifyRejection::unavailable()
            })? {
            Verdict::Admitted { .. } => None,
            Verdict::Limited { retry_after } => Some(retry_after),
        }
    } else {
        None
    };

    if nonce.is_some()
        && let Some(retry_after) = limited_until
    {
        // The whole request, unlike `deep`: a client that asked for evidence and got an
        // unsigned answer would hold nothing it could later show anyone, and would not know it.
        tracing::info!(%owner, %retry_after, "a signed verify was throttled");
        return Err(VerifyRejection::RateLimited {
            code: error_codes::STORAGE_DEEP_RATE_LIMITED,
            retry_after: unix_seconds(retry_after),
        });
    }

    let depth = match (request.deep, limited_until) {
        (false, _) => crate::verify::Depth::Structural,
        (true, None) => crate::verify::Depth::Deep,
        (true, Some(_)) => {
            // Deliberately **not** a `429` for the whole request. A deep scan is an addition to
            // a structural verdict; refusing the structural half because the optional half was
            // throttled would make the limiter cost a client more than it saves them, and a
            // client that stopped verifying at all is the outcome nobody wants.
            tracing::info!(%owner, "a deep verify was throttled to a structural one");
            crate::verify::Depth::RateLimited
        }
    };

    let verdicts = crate::verify::verify(&verify, &owner, &queries, depth)
        .await
        .map_err(|error| {
            tracing::error!(%error, %owner, "a storage verdict could not be computed");
            VerifyRejection::unavailable()
        })?;

    let signed = match nonce {
        None => None,
        Some(nonce) => {
            let attested = crate::verify::attested(&verdicts).map_err(|error| {
                tracing::error!(%error, %owner, "a storage verdict could not be attested");
                VerifyRejection::unavailable()
            })?;
            // Every verdict of one request carries the one instant the engine read its clock at.
            let checked_at = verdicts
                .first()
                .map_or_else(|| verify.clock().now(), |verdict| verdict.checked_at);
            let signed = sign_storage_attestation(
                attestation.signer(),
                owner.as_str(),
                nonce,
                checked_at.to_string(),
                attested,
            );
            tracing::info!(%owner, assets = verdicts.len(), "issued a storage attestation");
            Some(BASE64.encode(signed.to_canonical_cbor()))
        }
    };

    Ok(Json(StorageVerifyResponse {
        attestation: signed,
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
                        deep: blob.deep.map(|deep| deep.as_str().to_owned()),
                    })
                    .collect(),
                checked_at: verdict.checked_at.to_string(),
            })
            .collect(),
    }))
}

/// Read an attestation nonce: exactly 32 bytes as 64 lowercase hex characters.
///
/// Lowercase only, as a content address is: two spellings of one nonce would be two requests a
/// client could not tell apart from the signed bytes.
fn parse_nonce(nonce: &str) -> Result<AttestationNonce, VerifyRejection> {
    let well_formed = nonce.len() == 2 * ATTESTATION_NONCE_LEN
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !well_formed {
        return Err(VerifyRejection::invalid(format!(
            "an attestation nonce is {ATTESTATION_NONCE_LEN} bytes as lowercase hex"
        )));
    }
    let hash = capsule_core::crypto::hash::Hash32::from_hex(nonce)
        .map_err(|_| VerifyRejection::invalid("an attestation nonce is not hex"))?;
    Ok(hash.0)
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
