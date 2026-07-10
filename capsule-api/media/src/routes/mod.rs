use salvo::prelude::*;

use crate::drop_state::DropState;
use crate::state::AppState;

mod assets;
mod drops;
mod exports;
mod receipts;
mod share;
mod verify;
mod well_known;

pub fn get_router(state: AppState) -> Router {
    Router::new()
        .hoop(affix_state::inject(state.clone()))
        // Asset media endpoints
        .push(
            Router::with_path("<asset_id>")
                .get(assets::get_original)
                .push(Router::with_path("thumbnail").get(assets::get_thumbnail))
                .push(Router::with_path("preview").get(assets::get_preview))
                .push(Router::with_path("download").get(assets::get_download))
                .push(Router::with_path("stream").get(assets::get_stream)),
        )
        // Batch operations
        .push(Router::with_path("batch-download").post(assets::batch_download))
}

/// Separate router for public share access (mounted at /s)
pub fn get_share_router(state: AppState) -> Router {
    Router::new()
        .hoop(affix_state::inject(state))
        .push(Router::with_path("<token>").get(share::get_shared_content))
}

/// Storage-verification router (mounted at /storage). Slice `S-C3` (+ signed attestation,
/// slice `S-C15`).
pub fn get_storage_router(state: AppState) -> Router {
    Router::new()
        .hoop(affix_state::inject(state))
        .push(Router::with_path("verify").post(verify::storage_verify))
}

/// Durable custody-receipt router (mounted at /assets; slice `S-C15`).
pub fn get_receipts_router(state: AppState) -> Router {
    Router::new()
        .hoop(affix_state::inject(state))
        .push(Router::with_path("{asset_id}/receipts").get(receipts::get_asset_receipts))
}

/// Attestation-key publication router (mounted at /.well-known/capsule; slice `S-C15`).
/// Public — clients pin the keys (TOFU) to verify receipts.
pub fn get_well_known_router(state: AppState) -> Router {
    Router::new()
        .hoop(affix_state::inject(state))
        .push(Router::with_path("attestation-keys").get(well_known::attestation_keys))
}

/// Guest drop-session router (mounted at /u; link-capability auth). `POST` opens a session and
/// `PATCH` appends a chunk, reusing the S-C1 upload chunk mechanics (slice `S-C5`).
pub fn get_drop_link_router(state: DropState) -> Router {
    Router::new().hoop(affix_state::inject(state)).push(
        Router::with_path("{opaque_id}/drop")
            .post(drops::create_drop_session)
            .push(Router::with_path("{drop_id}").patch(drops::append_drop_chunk)),
    )
}

/// Owner-facing drop inbox router (mounted at /drops; session auth). Slice `S-C5`.
pub fn get_drops_router(state: DropState) -> Router {
    Router::new()
        .hoop(affix_state::inject(state))
        .get(drops::list_drop_inbox)
        .push(
            Router::with_path("{drop_id}")
                .delete(drops::discard_drop)
                .push(Router::with_path("adopt").post(drops::adopt_drop)),
        )
}
