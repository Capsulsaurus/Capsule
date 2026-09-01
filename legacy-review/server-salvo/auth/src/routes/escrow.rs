//! Master-key escrow store/fetch/replace — the recovery-escrow server surface (slice
//! `S-C12`; SSoT: [Backup — Master-Key Escrow] and the [guided re-wrap] contract).
//!
//! A signed-in user **stores** the passphrase-wrapped account master key
//! (`capsule_core::backup` wrap) as an opaque blob; the same owner **fetches** it back to
//! bootstrap the passphrase restore path on a fresh device. The server stores and serves the
//! bytes **verbatim** — it never interprets, re-models, or decrypts the wrap format. Its only
//! guards are ownership (you store and fetch only your own escrow) and a coarse size sanity
//! bound. The ≥128-bit recovery-secret entropy floor is a client-side rule enforced in core
//! and is deliberately not re-validated server-side.
//!
//! **Single active escrow.** Store and replace are the same operation: a guarded upsert keyed
//! by the caller's account id overwrites the row in place, so a replace deletes the prior
//! ciphertext in the same transaction — after it, the old blob is gone and unwraps nothing.
//! There is no version/monotonicity: rotation is a plain replace, per the guided re-wrap
//! contract (an O(1) escrow-blob replacement with no data re-encryption).
//!
//! Paths live under `backup/escrow`.
//!
//! [Backup — Master-Key Escrow]: https://docs/design/backup-recovery/#master-key-escrow
//! [guided re-wrap]: https://docs/design/backup-recovery/#on-repeated-failure-guided-re-wrap

use capsule_i18n::error_codes;
use salvo::prelude::*;
use service::escrow::{EscrowError, Mutation, Query};

use crate::errors::ClaimValidationError;
use crate::models::errors::ApiError;
use crate::state::AppState;
use crate::utils::headers::get_token_from_headers;

/// Upper bound on a stored escrow blob. A wrapped 32-byte master key plus its in-band
/// Argon2id parameters, salt, and nonce is a couple hundred bytes; a larger body is refused
/// before buffering as a coarse abuse/sanity guard (the server never inspects the contents).
const MAX_ESCROW_BYTES: usize = 4 * 1024;

/// Resolve the caller's account id from the bearer access token, or the auth error to
/// surface. Escrow is strictly owner-scoped: the resolved id is the only account a caller can
/// store to or fetch from — there is no target-user parameter.
fn authenticate(req: &Request, state: &AppState) -> Result<String, ClaimValidationError> {
    let token = get_token_from_headers(req.headers())?;
    use secrecy::ExposeSecret;
    let claims = state.auth_service.get_claims(token.expose_secret())?;
    claims.validate_access_token()?;
    Ok(claims.sub)
}

/// Store/replace responses. Every rejection carries a stable `error.*` code (clients switch on
/// the code, not the bare status).
pub(super) enum StoreEscrowResponses {
    /// Stored (single active escrow: any prior blob was replaced in the same transaction).
    NoContent,
    /// No/invalid bearer token.
    Unauthorized(ClaimValidationError),
    /// Body failed the coarse size sanity bound (`400 error.escrow.malformed`).
    Malformed(String),
    /// Server fault.
    Internal,
}

capsule_wire::salvo_responses! {
    StoreEscrowResponses {
        NoContent {} => 204, empty(),
            doc("Escrow stored (any prior blob replaced in place)");
        Unauthorized(e) => _, delegate(e), undocumented();
        Malformed(detail) => 400, json(ApiError::with_code(
            format!("Malformed escrow blob: {detail}"),
            error_codes::ESCROW_MALFORMED,
        )), doc("Escrow blob failed the size sanity bound");
        Internal {} => 500, json(ApiError::new("Internal server error")), undocumented();
    }
    delegated {
        401 => "Missing or invalid access token",
    }
}

/// Fetch responses. The success body is the exact opaque escrow bytes last stored.
pub(super) enum FetchEscrowResponses {
    /// The verbatim wrapped-master-key bytes (`application/octet-stream`).
    Ok(Vec<u8>),
    /// No/invalid bearer token.
    Unauthorized(ClaimValidationError),
    /// The caller has never stored an escrow.
    NotFound,
    /// Server fault.
    Internal,
}

capsule_wire::salvo_responses! {
    FetchEscrowResponses {
        Ok(bytes) => 200,
            header("Content-Type", "application/octet-stream")
            custom { |res|
                if let Err(e) = res.write_body(bytes) {
                    tracing::error!("failed to write escrow body: {e}");
                    res.status_code(StatusCode::INTERNAL_SERVER_ERROR);
                }
            },
            doc(
                "The wrapped master-key escrow, returned verbatim as opaque bytes (application/octet-stream)"
            );
        Unauthorized(e) => _, delegate(e), undocumented();
        NotFound {} => 404, json(ApiError::new("No escrow stored for this user")),
            doc("No escrow stored for this user");
        Internal {} => 500, json(ApiError::new("Internal server error")), undocumented();
    }
    delegated {
        401 => "Missing or invalid access token",
    }
}

/// Store or replace the caller's wrapped master-key escrow. The body is the opaque
/// `capsule_core::backup` wrap; the server keeps it opaque and overwrites any prior escrow in
/// the same transaction (single active escrow), so a replace leaves the old ciphertext
/// unretrievable.
#[endpoint(operation_id = "store_backup_escrow", tags("auth"), security(("bearer" = [])))]
pub async fn store_backup_escrow(req: &mut Request, depot: &mut Depot) -> StoreEscrowResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    let user_id = match authenticate(req, state) {
        Ok(id) => id,
        Err(e) => return StoreEscrowResponses::Unauthorized(e),
    };

    let body = match req.payload_with_max_size(MAX_ESCROW_BYTES).await {
        Ok(b) => b.to_vec(),
        Err(e) => {
            return StoreEscrowResponses::Malformed(format!("unreadable or oversized body: {e}"));
        }
    };

    match Mutation::store(&state.conn, &user_id, body).await {
        Ok(()) => StoreEscrowResponses::NoContent,
        Err(EscrowError::Malformed(detail)) => StoreEscrowResponses::Malformed(detail),
        Err(EscrowError::Db(e)) => {
            tracing::error!("backup escrow store db error: {e}");
            StoreEscrowResponses::Internal
        }
    }
}

/// Fetch the caller's own wrapped master-key escrow verbatim so a client can unwrap it with
/// the recovery passphrase (the `capsule_core::backup` restore path) on a fresh device.
#[endpoint(operation_id = "fetch_backup_escrow", tags("auth"), security(("bearer" = [])))]
pub async fn fetch_backup_escrow(req: &mut Request, depot: &mut Depot) -> FetchEscrowResponses {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");

    let user_id = match authenticate(req, state) {
        Ok(id) => id,
        Err(e) => return FetchEscrowResponses::Unauthorized(e),
    };

    match Query::fetch(&state.conn, &user_id).await {
        Ok(Some(bytes)) => FetchEscrowResponses::Ok(bytes),
        Ok(None) => FetchEscrowResponses::NotFound,
        Err(e) => {
            tracing::error!("backup escrow fetch db error: {e}");
            FetchEscrowResponses::Internal
        }
    }
}
