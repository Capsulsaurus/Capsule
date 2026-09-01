//! `GET /assets/{asset_id}/receipts` — the durable custody-receipt fetch (slice `S-C15`).
//!
//! Unlike the session-window fetch on the upload server, this serves the *permanent* receipt
//! log for an asset (exempt from session GC) to the uploader or the asset's owner. Every
//! receipt covering the asset is returned in chain order (`receipt_seq` ascending), each with
//! its full signed canonical-CBOR form (base64) so the client can verify the hybrid signature
//! under the published attestation key and append it beside its provenance chain.

use auth::utils::headers::validate_user_from_headers;
use entity::{asset, custody_receipt};
use salvo::oapi::extract::PathParam;
use salvo::prelude::*;
use sea_orm::EntityTrait;
use serde::Serialize;

use crate::state::AppState;

/// One custody receipt, decoded, plus its signed CBOR form.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct AssetReceipt {
    /// Strictly monotonic per-server sequence number.
    pub receipt_seq: i64,
    /// This server's canonical origin.
    pub server_id: String,
    /// The attestation key fingerprint that signed (hex).
    pub server_key_id: String,
    /// SHA-256 of the previous receipt (hex), absent for the first receipt.
    pub prior_receipt_hash: Option<String>,
    /// The upload session that produced custody.
    pub upload_id: String,
    /// `original | derivative | metadata | provenance`.
    pub blob_role: String,
    /// The server-recomputed ciphertext content address (hex).
    pub ciphertext_hash: String,
    /// Ciphertext size in bytes.
    pub size: i64,
    /// SHA-256 of the manifest envelope CBOR (hex), when present.
    pub envelope_hash: Option<String>,
    /// The server's trusted clock at the finalization commit (RFC 3339).
    pub received_at: String,
    /// The full signed receipt as canonical CBOR (base64).
    pub receipt_cbor: String,
}

impl From<custody_receipt::Model> for AssetReceipt {
    fn from(m: custody_receipt::Model) -> Self {
        use base64::Engine as _;
        Self {
            receipt_seq: m.receipt_seq,
            server_id: m.server_id,
            server_key_id: m.server_key_id,
            prior_receipt_hash: m.prior_receipt_hash,
            upload_id: m.upload_id,
            blob_role: m.blob_role,
            ciphertext_hash: m.ciphertext_hash,
            size: m.size,
            envelope_hash: m.envelope_hash,
            received_at: m.received_at.to_rfc3339(),
            receipt_cbor: base64::engine::general_purpose::STANDARD.encode(&m.receipt_cbor),
        }
    }
}

/// The `GET /assets/{asset_id}/receipts` response.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct AssetReceiptsResponse {
    /// The asset id.
    pub asset_id: String,
    /// Every receipt covering the asset, in chain order.
    pub receipts: Vec<AssetReceipt>,
}

/// Fetch the permanent custody-receipt log for an asset (uploader or owner).
#[endpoint(operation_id = "get_asset_receipts", tags("storage"), security(("bearer" = [])))]
pub async fn get_asset_receipts(
    req: &mut Request,
    depot: &mut Depot,
    asset_id: PathParam<String>,
) -> Result<Json<AssetReceiptsResponse>, StatusError> {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    let asset_id = asset_id.into_inner();

    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return Err(StatusError::unauthorized().detail(e.to_string())),
        };

    // Authorize against the asset: its owner group or the uploading user.
    let asset = asset::Entity::find_by_id(&asset_id)
        .one(&state.conn)
        .await
        .map_err(|e| StatusError::internal_server_error().detail(e.to_string()))?
        .ok_or_else(StatusError::not_found)?;
    if asset.owner_id != user_id && asset.upload_user_id != user_id {
        return Err(StatusError::forbidden());
    }

    let receipts = service::attestation::Query::receipts_by_asset(&state.conn, &asset_id)
        .await
        .map_err(|e| StatusError::internal_server_error().detail(e.to_string()))?
        .into_iter()
        .map(AssetReceipt::from)
        .collect();

    Ok(Json(AssetReceiptsResponse { asset_id, receipts }))
}
