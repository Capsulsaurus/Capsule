use salvo::prelude::*;

use crate::envelope::EnvelopeGate;
use crate::state::{AppState, OpsState};

mod ops;
mod quota;
mod receipt;
mod tus;

pub(super) fn get_router(state: AppState, protocol_min: String, protocol_max: String) -> Router {
    // The protocol handshake (invariant 1) gates every session/write route — but not the
    // unauthenticated health check. It advertises the accepted range on every response.
    let gated = Router::new()
        .hoop(EnvelopeGate::new(protocol_min, protocol_max))
        .push(Router::with_path("sessions").get(tus::list_sessions))
        .push(Router::new().post(tus::create_upload))
        .push(
            Router::with_path("{id}")
                .head(tus::head_upload)
                .patch(tus::patch_upload)
                .delete(tus::delete_upload),
        );

    Router::new()
        .hoop(affix_state::inject(state))
        .push(Router::with_path("status").get(status))
        // The quota snapshot (S-C6) is a plain authenticated read; it does not ride the
        // envelope protocol handshake the write routes require.
        .push(Router::with_path("quota").get(quota::get_quota))
        // The custody-receipt fetch (S-C15) is likewise a plain authenticated read, outside
        // the envelope handshake — a client with no live session can still recover its receipt.
        .push(Router::with_path("{id}/receipt").get(receipt::get_receipt))
        .push(gated)
}

/// The generic lifecycle-write surface, mounted at the API root under `albums/` (slice
/// `S-C16`) so the transport path is `POST /albums/{album_id}/ops` per the authorization
/// contract — not under `/upload`. The same fail-closed protocol handshake (invariant 1) the
/// upload write routes require gates it.
pub(crate) fn get_ops_router(
    state: OpsState,
    protocol_min: String,
    protocol_max: String,
) -> Router {
    Router::new()
        .hoop(affix_state::inject(state))
        .hoop(EnvelopeGate::new(protocol_min, protocol_max))
        .push(Router::with_path("{album_id}/ops").post(ops::post_op))
}

#[handler]
async fn status() -> &'static str {
    "Upload service is running"
}
