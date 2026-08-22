use salvo::prelude::*;

use crate::envelope::EnvelopeGate;
use crate::state::{AppState, OpsState};

mod albums;
mod ops;
mod quota;
mod receipt;
mod tus;

pub(super) fn get_router(state: AppState, protocol_min: String, protocol_max: String) -> Router {
    // The route *shape* (and thus the OpenAPI schema) is single-sourced in [`route_tree`];
    // serving only adds the depot state injector on top. See [`crate::openapi_router`]
    // (slice `S-D8`).
    route_tree(protocol_min, protocol_max).hoop(affix_state::inject(state))
}

/// The upload route tree with no injected state — the single source of truth for both the
/// live router ([`get_router`]) and the deterministic OpenAPI schema dump
/// ([`crate::openapi_router`], slice `S-D8`). The `EnvelopeGate` handshake hoop is a
/// pure-string concern and stays; only the depot state injection (a serving concern that
/// carries no schema information) is layered on by [`get_router`].
pub(super) fn route_tree(protocol_min: String, protocol_max: String) -> Router {
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
        .push(Router::with_path("status").get(status))
        // The quota snapshot (S-C6) is a plain authenticated read; it does not ride the
        // envelope protocol handshake the write routes require.
        .push(Router::with_path("quota").get(quota::get_quota))
        // The custody-receipt fetch (S-C15) is likewise a plain authenticated read, outside
        // the envelope handshake — a client with no live session can still recover its receipt.
        .push(Router::with_path("{id}/receipt").get(receipt::get_receipt))
        .push(gated)
}

/// The `/albums` route tree, mounted at the API root (slice `S-C16` + `S-C25`) so the
/// transport paths are `POST /albums` and `POST /albums/{album_id}/ops` per the authorization
/// contract — not under `/upload`.
pub(crate) fn get_ops_router(
    state: OpsState,
    protocol_min: String,
    protocol_max: String,
) -> Router {
    ops_route_tree(protocol_min, protocol_max).hoop(affix_state::inject(state))
}

/// The `/albums` route tree with no injected state, for the OpenAPI schema dump
/// (slice `S-D8`). Shares its route shape with [`get_ops_router`].
///
/// Two surfaces with deliberately different gating:
///
/// - **`POST /albums`** (provisioning, `S-C25`) is a plain authenticated write outside the
///   envelope handshake. It carries no signed manifest and moves no bytes — it is how a
///   client gets an album that invariant 6 can *pass*, so gating it on the same handshake as
///   the writes it unblocks would buy nothing.
/// - **`POST /albums/{album_id}/ops`** (lifecycle writes, `S-C16`) sits behind the same
///   fail-closed protocol handshake (invariant 1) the upload write routes require, scoped to
///   its own sub-router exactly as the upload tree scopes its `gated` branch.
pub(crate) fn ops_route_tree(protocol_min: String, protocol_max: String) -> Router {
    Router::new()
        .push(Router::new().post(albums::provision_album))
        .push(
            Router::new()
                .hoop(EnvelopeGate::new(protocol_min, protocol_max))
                .push(Router::with_path("{album_id}/ops").post(ops::post_op)),
        )
}

#[handler]
async fn status() -> &'static str {
    "Upload service is running"
}
