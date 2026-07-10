use salvo::prelude::*;

use crate::envelope::EnvelopeGate;
use crate::state::AppState;

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
        .push(gated)
}

#[handler]
async fn status() -> &'static str {
    "Upload service is running"
}
