use salvo::prelude::*;

use crate::state::AppState;

mod assets;
mod drops;
mod exports;
mod share;
mod verify;

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

/// Storage-verification router (mounted at /storage). Skeleton — slice `S-C3`.
pub fn get_storage_router(state: AppState) -> Router {
    Router::new()
        .hoop(affix_state::inject(state))
        .push(Router::with_path("verify").post(verify::storage_verify))
}

/// Guest drop-session router (mounted at /u; link-capability auth). Skeleton — `S-C5`.
/// Drop chunks reuse the upload protocol's `PATCH` mechanics under the session this
/// opens.
pub fn get_drop_link_router(state: AppState) -> Router {
    Router::new()
        .hoop(affix_state::inject(state))
        .push(Router::with_path("<opaque_id>/drop").post(drops::create_drop_session))
}

/// Owner-facing drop inbox router (mounted at /drops; session auth). Skeleton — `S-C5`.
pub fn get_drops_router(state: AppState) -> Router {
    Router::new()
        .hoop(affix_state::inject(state))
        .get(drops::list_drop_inbox)
        .push(
            Router::with_path("<drop_id>")
                .delete(drops::discard_drop)
                .push(Router::with_path("adopt").post(drops::adopt_drop)),
        )
}
