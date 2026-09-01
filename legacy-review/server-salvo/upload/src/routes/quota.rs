use auth::utils::headers::validate_user_from_headers;
use salvo::prelude::*;
use service::quota::{self, UNLIMITED};

use crate::models::responses::{QuotaResponse, QuotaResponses};
use crate::state::AppState;

/// Report the authenticated uploader's storage-quota snapshot (slice `S-C6`).
///
/// Returns `used` (bytes charged to the caller), the configured `soft_limit` / `hard_limit`
/// (`null` when the deployment runs unlimited), and the classified `state`. Read-only; the
/// hard enforcement point is `POST /upload` session creation, not this endpoint.
#[endpoint(operation_id = "get_quota", tags("upload"), security(("bearer" = [])))]
pub async fn get_quota(req: &mut Request, dep: &mut Depot) -> QuotaResponses {
    let state = dep
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    let user_id =
        match validate_user_from_headers(req.headers(), &state.config.jwt_eddsa_decoding_key) {
            Ok(id) => id,
            Err(e) => return QuotaResponses::Unauthorized(e.to_string()),
        };

    let limits = state.config.quota_limits();
    match quota::Query::current_status(&state.conn, &user_id, &limits).await {
        Ok(status) => QuotaResponses::Success(QuotaResponse {
            used: status.used,
            soft_limit: (status.soft_limit != UNLIMITED).then_some(status.soft_limit),
            hard_limit: (status.hard_limit != UNLIMITED).then_some(status.hard_limit),
            state: status.state.as_str().to_string(),
        }),
        Err(e) => QuotaResponses::InternalServerError(eyre::eyre!(e).into()),
    }
}
