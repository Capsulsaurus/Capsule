use salvo::prelude::*;

use crate::state::AppState;

mod assets;
mod exports;
mod share;

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
