use salvo::prelude::*;

use crate::drop_state::DropState;
use crate::share_state::ShareState;
use crate::state::AppState;

mod blob;
mod drops;
mod receipts;
mod share;
mod verify;
mod well_known;

/// Key-free ranged blob-serving router (mounted at `/blob`; slice `S-C10`). `GET /blob/{hash}`
/// serves opaque ciphertext by content address with HTTP `Range` at the ciphertext stride;
/// access-token auth is enforced per handler.
pub fn get_blob_router(state: AppState) -> Router {
    blob_tree().hoop(affix_state::inject(state))
}

fn blob_tree() -> Router {
    Router::new().push(Router::with_path("{hash}").get(blob::get_blob))
}

/// Public share-link serve router (mounted at `/s`; slice `S-C4`). All three endpoints are
/// key-free and unauthenticated; the serve engine enforces rate limits, the fail-closed
/// revocation cache, the mandatory privacy strip, and the home-server gate.
pub fn get_share_router(state: ShareState) -> Router {
    Router::new().hoop(affix_state::inject(state)).push(
        Router::with_path("{opaque_id}")
            .get(share::get_share_metadata)
            .push(Router::with_path("wrapped-secret").get(share::get_wrapped_secret))
            .push(
                Router::with_path("blob")
                    .push(Router::with_path("{hash}").get(share::get_share_blob)),
            ),
    )
}

/// Storage-verification router (mounted at /storage). Slice `S-C3` (+ signed attestation,
/// slice `S-C15`).
pub fn get_storage_router(state: AppState) -> Router {
    storage_tree().hoop(affix_state::inject(state))
}

fn storage_tree() -> Router {
    Router::new().push(Router::with_path("verify").post(verify::storage_verify))
}

/// Durable custody-receipt router (mounted at /assets; slice `S-C15`).
pub fn get_receipts_router(state: AppState) -> Router {
    receipts_tree().hoop(affix_state::inject(state))
}

fn receipts_tree() -> Router {
    Router::new().push(Router::with_path("{asset_id}/receipts").get(receipts::get_asset_receipts))
}

/// The media crate's route trees that belong in the generated REST client's OpenAPI schema
/// (slice `S-D8`), pre-nested under the same sub-paths the live server mounts them at
/// ([`crate::get_router`] and friends in [`crate`]), with no injected state — for the
/// deterministic schema dump.
///
/// Deliberately narrowed to a clean, generatable subset: the share / drop / well-known routers
/// are bare `#[handler]`s that salvo-oapi does not describe, so they are absent from the schema
/// by construction anyway. (The plaintext-era per-id asset-serve tree that used to be carved
/// out here was deleted with slice `S-C17`.)
///
/// The blob (`/blob/{hash}`), storage-verify (`/storage/verify`), and custody-receipt
/// (`/assets/{asset_id}/receipts`) reads stay in — plain, typed request/response surfaces.
pub fn schema_router() -> Router {
    Router::new()
        .push(Router::with_path("blob").push(blob_tree()))
        .push(Router::with_path("storage").push(storage_tree()))
        .push(Router::with_path("assets").push(receipts_tree()))
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
