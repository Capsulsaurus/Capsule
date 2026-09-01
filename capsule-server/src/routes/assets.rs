//! `GET /v1/assets/{asset_id}/receipts` — the whole custody chain for one asset (slice `S-C58`).
//!
//! The **durable** counterpart to `GET /v1/upload/{id}/receipt`. That one answers "what did you
//! admit taking, for this transfer", and its answer disappears from the caller's reach when the
//! upload session is collected; this one answers "what have you admitted taking, for this
//! photo", over the asset's whole life — every blob of the original bundle, and every later
//! manifest that amended it.
//!
//! Which is what makes it the operation a *dispute* is settled from. `S-C52` decided the server
//! keeps the provenance chain rather than collapsing it; this is where the keeping becomes
//! visible. A client comparing its local chain against this list can tell a server that lost a
//! blob from one that never had it, because a receipt is a signature over what the server said
//! it accepted, at a sequence number it cannot go back and renumber.
//!
//! # The CBOR is the receipt; the fields are a convenience
//!
//! Each entry carries `receipt_cbor`, base64 of the exact canonical bytes the attestation
//! signature covers, and the decoded scalars beside it. The scalars are for a client that wants
//! to render a list without decoding thirty receipts; **verification reads the CBOR**, and a
//! client that trusted the projection would be trusting this server's JSON encoder rather than
//! its signature.
//!
//! # Owner only
//!
//! Not the uploader. The receipts name their uploading account, so admitting one would be
//! possible — and it is exactly wrong for the case that makes it possible: a guest depositing
//! into somebody's inbox through a drop link (`S-C5`) gives the asset up, and letting them read
//! its later chain would tell them when the owner amended, culled or restored it. The authority
//! is the same one `S-C39` settled for blob reads: an account reads its own assets.
//!
//! # `S-C28` audit
//!
//! | Status | Verdict |
//! | --- | --- |
//! | `200` | the chain, in `receipt_seq` order. An empty list is a normal answer for an asset whose bundle is still in flight |
//! | `404 error.storage.asset_not_found` | no such asset, **or** not this caller's. One answer for both: an asset id is not a capability, and a guess must not reveal whether it named something |
//! | `401` / `403` | the framework's, through `Auth` |
//! | `500 error.storage.unavailable` | the index or the receipt log could not answer |

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::receipts::CustodyReceipt;
use capsule_i18n::error_codes;
use kynos::prelude::*;
use kynos::security::auth::Auth;
use serde::{Deserialize, Serialize};

use crate::attestation::AttestationContext;
use crate::auth::AccessToken;
use crate::routes::storage::StorageTag;
use crate::store::{AssetId, UserId};
use crate::verify::VerifyContext;

/// Which asset's chain.
#[derive(Schema, PathParams, Debug)]
pub struct AssetPath {
    /// The asset id.
    pub asset_id: String,
}

/// One custody receipt, decoded, beside the bytes that were signed.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct AssetReceipt {
    /// Strictly monotonic per server. The chain position this receipt cannot be moved from.
    pub receipt_seq: u64,
    /// This server's canonical origin — what binds the receipt to one server.
    pub server_id: String,
    /// The attestation key fingerprint that signed, hex. Survives rotation, which is why the
    /// key is named rather than assumed.
    pub server_key_id: String,
    /// SHA-256 of the previous receipt in the server's log, hex. Absent for the first receipt
    /// this server ever issued.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prior_receipt_hash: Option<String>,
    /// The upload session that produced custody.
    pub upload_id: String,
    /// `original`, `derivative`, `metadata` or `provenance`.
    pub blob_role: String,
    /// The server-recomputed ciphertext content address, hex.
    pub ciphertext_hash: String,
    /// Ciphertext size in bytes.
    pub size: u64,
    /// SHA-256 of the asset's signed manifest, hex — present on the `provenance` receipt and
    /// absent on every other, because the manifest commits to the rest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub envelope_hash: Option<String>,
    /// The server's trusted clock at the finalization commit, RFC 3339.
    pub received_at: String,
    /// The full signed receipt as canonical CBOR, base64.
    ///
    /// **This is the receipt.** Verify the hybrid signature over these bytes under the key
    /// `server_key_id` names, from `/.well-known/capsule/attestation-keys`; everything above is
    /// a reading of them.
    pub receipt_cbor: String,
}

impl From<CustodyReceipt> for AssetReceipt {
    fn from(receipt: CustodyReceipt) -> Self {
        // Encoded from the whole receipt before anything is read out of it, so the bytes served
        // are the bytes held rather than a re-serialization of a struct this route projected.
        let receipt_cbor = BASE64.encode(receipt.to_canonical_cbor());
        let core = receipt.core;
        Self {
            receipt_seq: core.receipt_seq,
            server_id: core.server_id,
            server_key_id: core.server_key_id.to_hex(),
            prior_receipt_hash: core.prior_receipt_hash.map(|hash| hash.to_hex()),
            upload_id: core.upload_id,
            blob_role: core.blob_role,
            ciphertext_hash: core.ciphertext_hash.to_hex(),
            size: core.size,
            envelope_hash: core.envelope_hash.map(|hash| hash.to_hex()),
            received_at: core.received_at,
            receipt_cbor,
        }
    }
}

/// The chain.
#[derive(Schema, Serialize, Deserialize, Debug, Clone)]
pub struct AssetReceiptsResponse {
    /// The asset the chain belongs to, echoed so a client batching requests can tell the
    /// answers apart.
    pub asset_id: String,
    /// Every receipt covering the asset, in `receipt_seq` order.
    pub receipts: Vec<AssetReceipt>,
}

/// Why a chain was not returned.
#[derive(Debug, thiserror::Error, ApiError)]
pub enum AssetReceiptsRejection {
    /// No such asset, or not this caller's. One answer for both; see the module docs.
    #[error("no such asset")]
    #[problem(status = 404, title = "Not found")]
    NotFound {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },

    /// A collaborator could not answer.
    #[error("the custody receipts could not be read")]
    #[problem(status = 500, title = "Internal server error")]
    Unavailable {
        /// The stable catalog code.
        #[problem(extension)]
        code: &'static str,
    },
}

/// Fetch every custody receipt covering one asset.
#[kynos::get(
    "/v1/assets/{asset_id}/receipts",
    operation_id = "get_asset_receipts",
    tag = StorageTag
)]
pub async fn get_asset_receipts(
    Inject(verify): Inject<VerifyContext>,
    Inject(attestation): Inject<AttestationContext>,
    Auth(credential): Auth<AccessToken>,
    Path(path): Path<AssetPath>,
) -> Result<Json<AssetReceiptsResponse>, AssetReceiptsRejection> {
    let asset = AssetId::new(path.asset_id);
    let caller = UserId::new(credential.user.as_str());

    // The ownership read comes first and decides everything, exactly as `serve::resolve` does
    // (`S-C39`): a caller who is not the owner must not be able to tell a tombstoned asset from
    // one that never existed by reading the status line.
    let row = verify
        .index()
        .read(&asset)
        .await
        .map_err(|error| {
            tracing::error!(%error, asset_id = %asset, "the asset index could not answer");
            AssetReceiptsRejection::Unavailable {
                code: error_codes::STORAGE_UNAVAILABLE,
            }
        })?
        .ok_or_else(AssetReceiptsRejection::not_found)?;

    if row.owner_id.as_str() != caller.as_str() {
        tracing::info!(asset_id = %asset, "a receipt chain was refused: not this caller's asset");
        return Err(AssetReceiptsRejection::not_found());
    }

    let receipts = attestation
        .receipts()
        .for_asset(&asset)
        .await
        .map_err(|error| {
            tracing::error!(%error, asset_id = %asset, "the receipt log could not answer");
            AssetReceiptsRejection::Unavailable {
                code: error_codes::STORAGE_UNAVAILABLE,
            }
        })?;

    tracing::debug!(asset_id = %asset, count = receipts.len(), "serving a custody chain");
    Ok(Json(AssetReceiptsResponse {
        asset_id: asset.as_str().to_owned(),
        receipts: receipts.into_iter().map(AssetReceipt::from).collect(),
    }))
}

impl AssetReceiptsRejection {
    /// No such asset, or not this caller's.
    fn not_found() -> Self {
        Self::NotFound {
            code: error_codes::STORAGE_ASSET_NOT_FOUND,
        }
    }
}
