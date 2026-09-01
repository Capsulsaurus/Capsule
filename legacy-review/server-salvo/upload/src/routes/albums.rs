//! `POST /v1/albums` — album provisioning (slice `S-C25`).
//!
//! The one endpoint that lets a client tell the server an album exists. A container album's
//! id is derived from the account master key ([Organization — The Default Album]), so the
//! client already knows it; provisioning binds that UUID to the authenticated caller's owner
//! group so [invariant 6] ("album exists; the caller has write capability on it") can pass for
//! a real, client-named album. Without it, `capsule push` had nowhere to land: the entire
//! `/v1/albums` tree was `{album_id}/ops`, and nothing anywhere created an album row.
//!
//! # Shape
//!
//! ```text
//! POST /v1/albums            Authorization: Bearer <access token>
//! { "album_id": "0198f3c2-9c4a-7b3d-8f21-4d7c9a1b2e35" }
//!
//! 201 { "album_id": "…", "created": true  }   the row was created and bound
//! 200 { "album_id": "…", "created": false }   already yours — nothing written
//! 400 error.album.invalid_id                  not a canonical hyphenated UUID
//! 403 error.album.not_available                the id cannot be bound to this account
//! ```
//!
//! **Idempotent by contract**, not by accident: the same id arrives from every device the
//! user owns and again after a passphrase recovery, so re-registering must succeed. `created`
//! reports which happened for logging and for the status code; a client never has to branch
//! on it.
//!
//! **`403` is deliberately uninformative.** One code, one fixed message, whatever the reason —
//! the endpoint must not become an existence oracle over other accounts' derived album ids.
//!
//! **No name is accepted.** The body is strict (`deny_unknown_fields`), and its only field is
//! the id: sending `name` or `description` is a `400`, not a silently-ignored extra. The
//! plaintext `albums.name` / `albums.description` columns are written empty by the service
//! layer; the server is not entitled to album titles (slice `S-C26` retires the columns).
//!
//! [Organization — The Default Album]: https://docs/design/organization/#the-default-album
//! [invariant 6]: https://docs/design/threat-model/validation/#server-side-validation-invariants

use auth::models::errors::ApiError;
use capsule_i18n::error_codes;
use salvo::oapi::extract::JsonBody;
use salvo::prelude::*;
use service::album::{Mutation, ProvisionError, ProvisionOutcome};

use crate::models::requests::ProvisionAlbumRequest;
use crate::models::responses::ProvisionAlbumResponse;
use crate::state::OpsState;

/// Every answer `POST /v1/albums` can give. Rejections carry a stable `error.*` code; clients
/// switch on the code, never on the bare status.
pub(super) enum ProvisionResponses {
    /// The album row was created and bound to the caller's owner group.
    Created(ProvisionAlbumResponse),
    /// The album already existed and is already writable by the caller — nothing written.
    AlreadyProvisioned(ProvisionAlbumResponse),
    /// No/invalid bearer token.
    Unauthorized(String),
    /// The submitted id is not a canonical lowercase hyphenated UUID.
    InvalidId,
    /// The id cannot be bound to this account. One message for every reason.
    NotAvailable,
    /// Server fault.
    Internal,
}

capsule_wire::salvo_responses! {
    ProvisionResponses {
        Created(body) => 201, json(body),
            doc("The album was created and bound to the caller");
        AlreadyProvisioned(body) => 200, json(body), doc(
            "The album id was already provisioned to this account; nothing was written"
        );
        Unauthorized(detail) => 401, json(ApiError::new(detail)),
            doc("Missing or invalid access token");
        InvalidId {} => 400, json(ApiError::with_code(
            "album_id must be a canonical lowercase hyphenated UUID",
            error_codes::ALBUM_INVALID_ID,
        )), doc("album_id is not a canonical hyphenated UUID");
        // Fixed message: it must read identically whether the id is bound to another
        // account or is unavailable for any other reason.
        NotAvailable {} => 403, json(ApiError::with_code(
            "This album id is not available to this account",
            error_codes::ALBUM_NOT_AVAILABLE,
        )), doc("The album id is not available to this account");
        Internal {} => 500, json(ApiError::new("Internal server error")),
            undocumented();
    }
}

/// Register the caller's derived album id with the server.
///
/// Idempotent: re-registering an id the caller already owns succeeds and writes nothing.
/// Accepts no album name — album titles live in the encrypted sidecar, not on the server.
#[endpoint(
    operation_id = "provision_album",
    tags("albums"),
    security(("bearer" = []))
)]
pub async fn provision_album(
    req: &mut Request,
    depot: &mut Depot,
    body: JsonBody<ProvisionAlbumRequest>,
) -> ProvisionResponses {
    let state = depot
        .obtain::<OpsState>()
        .expect("OpsState is injected by middleware");

    let user_id = match auth::utils::headers::validate_user_from_headers(
        req.headers(),
        &state.config.jwt_eddsa_decoding_key,
    ) {
        Ok(id) => id,
        Err(e) => return ProvisionResponses::Unauthorized(e.to_string()),
    };

    let album_id = body.into_inner().album_id;
    match Mutation::provision_album(&state.conn, &user_id, &album_id).await {
        Ok(ProvisionOutcome::Created) => ProvisionResponses::Created(ProvisionAlbumResponse {
            album_id,
            created: true,
        }),
        Ok(ProvisionOutcome::AlreadyProvisioned) => {
            ProvisionResponses::AlreadyProvisioned(ProvisionAlbumResponse {
                album_id,
                created: false,
            })
        }
        Err(ProvisionError::InvalidAlbumId) => ProvisionResponses::InvalidId,
        Err(ProvisionError::NotAvailable) => ProvisionResponses::NotAvailable,
        Err(ProvisionError::Db(e)) => {
            tracing::error!("album provisioning db error: {e}");
            ProvisionResponses::Internal
        }
    }
}
