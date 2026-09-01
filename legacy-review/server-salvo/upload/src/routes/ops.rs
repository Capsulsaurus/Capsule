//! `POST /albums/{album_id}/ops` — the generic lifecycle-write surface (slice `S-C16`).
//!
//! One endpoint for every non-upload lifecycle write (`delete`, `metadata-update`,
//! `derivative-add`/`derivative-replace` over stored blobs, `trash-restore`). The body is the
//! signed manifest bundle (opaque canonical-CBOR manifest + the encrypted metadata blob when
//! the action carries one). The [`OpService`](crate::service::ops::OpService) runs the key-free
//! structural battery (invariants 16–18, 25) ahead of any write, then appends the provenance
//! record + mints the per-album `sync_seq` + records the replay row in one transaction.
//!
//! Rejections carry a stable `error.*` code and write nothing; a byte-identical resubmission
//! returns the stored prior response (at-most-once).

use auth::utils::headers::validate_user_from_headers;
use salvo::oapi::extract::PathParam;
use salvo::prelude::*;
use salvo::writing::Text;

use crate::error::UploadError;
use crate::models::requests::OpRequest;
use crate::state::OpsState;

/// Apply one lifecycle write to `{album_id}`.
#[endpoint(
    operation_id = "album_lifecycle_op",
    tags("lifecycle"),
    security(("bearer" = []))
)]
pub async fn post_op(
    req: &mut Request,
    depot: &mut Depot,
    res: &mut Response,
    album_id: PathParam<String>,
) {
    let state = depot
        .obtain::<OpsState>()
        .expect("OpsState is injected by middleware");
    let album_id = album_id.into_inner();

    // Parse the strict bundle ourselves so an unknown/malformed field is our own
    // `400 error.upload.malformed_request`, not the extractor's untyped 400.
    let request = match req.parse_json::<OpRequest>().await {
        Ok(r) => r,
        Err(e) => {
            UploadError::InvalidUpload(format!("malformed request body: {e}"))
                .write(req, depot, res)
                .await;
            return;
        }
    };

    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => {
                res.status_code(StatusCode::UNAUTHORIZED);
                res.render(Text::Plain(e.to_string()));
                return;
            }
        };

    match state.ops_service.apply(&album_id, &user_id, &request).await {
        Ok(result) => {
            let status = StatusCode::from_u16(result.status).unwrap_or(StatusCode::OK);
            res.status_code(status);
            // Render the stored bytes verbatim so a replay is byte-identical (no re-serialize).
            let body = String::from_utf8(result.body).unwrap_or_default();
            res.render(Text::Json(body));
        }
        Err(e) => e.write(req, depot, res).await,
    }
}
