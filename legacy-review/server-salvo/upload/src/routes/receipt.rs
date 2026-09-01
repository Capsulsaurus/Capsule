//! `GET /upload/{id}/receipt` — the session-window custody-receipt fetch (slice `S-C15`).
//!
//! Pairs with the lost-ACK recovery flow: a client that finalized but lost the ACK re-fetches
//! its [`CustodyReceipt`](service::attestation::CustodyReceipt) here. The receipt is signed
//! *inside* the finalization transaction, so it exists exactly once the session reached
//! `Completed`; a request before then is `409 error.upload.receipt_not_available` (invariant
//! 33). The response carries the full signed receipt as canonical CBOR (base64) — the form
//! the client verifies under the pinned attestation key — plus its decoded scalar fields.

use auth::utils::headers::validate_user_from_headers;
use capsule_i18n::error_codes;
use entity::{asset, custody_receipt};
use salvo::oapi::extract::PathParam;
use salvo::prelude::*;
use sea_orm::EntityTrait;
use serde::Serialize;

use crate::models::session::UploadSessionStatus;
use crate::state::AppState;

/// The decoded receipt view returned alongside the signed CBOR.
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct ReceiptResponse {
    /// Strictly monotonic per-server sequence number.
    pub receipt_seq: i64,
    /// This server's canonical origin.
    pub server_id: String,
    /// The attestation key fingerprint that signed (hex).
    pub server_key_id: String,
    /// SHA-256 of the previous receipt (hex), absent for the first receipt.
    pub prior_receipt_hash: Option<String>,
    /// The upload session id.
    pub upload_id: String,
    /// The asset id.
    pub asset_id: String,
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
    /// The full signed receipt as canonical CBOR (base64) — verify under the attestation key.
    pub receipt_cbor: String,
}

impl From<custody_receipt::Model> for ReceiptResponse {
    fn from(m: custody_receipt::Model) -> Self {
        use base64::Engine as _;
        Self {
            receipt_seq: m.receipt_seq,
            server_id: m.server_id,
            server_key_id: m.server_key_id,
            prior_receipt_hash: m.prior_receipt_hash,
            upload_id: m.upload_id,
            asset_id: m.asset_id,
            blob_role: m.blob_role,
            ciphertext_hash: m.ciphertext_hash,
            size: m.size,
            envelope_hash: m.envelope_hash,
            received_at: m.received_at.to_rfc3339(),
            receipt_cbor: base64::engine::general_purpose::STANDARD.encode(&m.receipt_cbor),
        }
    }
}

/// A structured error body carrying the stable `error.*` code clients localize.
#[derive(Serialize)]
struct ErrorResponse {
    code: &'static str,
    error: String,
}

/// Possible responses for the receipt fetch.
pub(super) enum ReceiptResponses {
    /// The signed receipt.
    Ok(Box<ReceiptResponse>),
    /// The session has not reached `Completed`, so no receipt is signed yet (409).
    NotAvailable,
    /// No such upload session and no receipt.
    NotFound,
    /// The caller is neither the uploader nor the owner.
    Forbidden,
    /// Missing / invalid bearer token.
    Unauthorized(String),
    /// A server-side failure.
    Internal(String),
}

capsule_wire::salvo_responses! {
    ReceiptResponses {
        Ok(data) => 200, json(*data),
            doc("The signed custody receipt", schema = ReceiptResponse);
        NotAvailable {} => 409, json(ErrorResponse {
            code: error_codes::UPLOAD_RECEIPT_NOT_AVAILABLE,
            error: "the upload has not finished; its receipt is not available yet"
                .to_string(),
        }), doc("Receipt not available (session not yet Completed)");
        NotFound {} => 404, text("upload session not found"),
            doc("No such upload session");
        Forbidden {} => 403, text("forbidden"), undocumented();
        Unauthorized(msg) => 401, text(msg), undocumented();
        Internal(msg) => 500, text(msg), undocumented();
    }
}

/// Fetch the custody receipt for an upload session.
#[endpoint(operation_id = "get_upload_receipt", tags("upload"), security(("bearer" = [])))]
pub async fn get_receipt(
    req: &mut Request,
    depot: &mut Depot,
    id: PathParam<String>,
) -> ReceiptResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    let upload_id = id.into_inner();

    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return ReceiptResponses::Unauthorized(e.to_string()),
        };

    // The receipt is permanent (exempt from session GC), so fetch it first: it may outlive
    // the session it was issued from.
    let receipt =
        match service::attestation::Query::receipt_by_upload(&state.conn, &upload_id).await {
            Ok(r) => r,
            Err(e) => return ReceiptResponses::Internal(e.to_string()),
        };

    if let Some(receipt) = receipt {
        // Authorize: the uploading user or the asset's owner group.
        let owner_id = match asset::Entity::find_by_id(&receipt.asset_id)
            .one(&state.conn)
            .await
        {
            Ok(a) => a.map(|a| a.owner_id),
            Err(e) => return ReceiptResponses::Internal(e.to_string()),
        };
        if receipt.uploaded_by_user != user_id && owner_id.as_deref() != Some(user_id.as_str()) {
            return ReceiptResponses::Forbidden;
        }
        return ReceiptResponses::Ok(Box::new(receipt.into()));
    }

    // No receipt yet: distinguish "session not Completed" (409) from "no such session" (404).
    match state.upload_service.get_session(&upload_id).await {
        Ok(Some(session)) => {
            if session.upload_user_id != user_id && session.owner_id != user_id {
                return ReceiptResponses::Forbidden;
            }
            if session.status == UploadSessionStatus::Completed {
                // Completed but no receipt is an inconsistency; the client retries.
                tracing::warn!(upload_id, "session Completed but no receipt row found");
            }
            ReceiptResponses::NotAvailable
        }
        Ok(None) => ReceiptResponses::NotFound,
        Err(e) => ReceiptResponses::Internal(e.to_string()),
    }
}
