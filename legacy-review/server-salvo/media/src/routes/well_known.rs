//! The `.well-known/capsule/*` registry (slices `S-C15`, `S-C18`).
//!
//! Every record here is **public and unauthenticated** — clients pin them (TOFU) on first
//! contact and peers poll them. The shapes live in [`crate::service::well_known`]; these
//! handlers are a thin rendering layer over the builders there, which is what keeps every
//! record unit-testable without a socket.
//!
//! | Path                 | Handler                | What it publishes                          |
//! | -------------------- | ---------------------- | ------------------------------------------ |
//! | `attestation-keys`   | [`attestation_keys`]   | storage-attestation keys + key history      |
//! | `server-info`        | [`server_info`]        | server-scoped discovery facts, never users  |
//! | `revoked-jti`        | [`revoked_jti`]        | the federation capability revocation list   |
//! | `deprecation`        | [`deprecation`]        | min-supported-client announcements          |
//!
//! `moved/{user}` — the one record that names a user — is post-v1 with Account Portability and
//! is deliberately not served.
//!
//! SSoT: [Authentication — The `.well-known/capsule/*` Registry].
//!
//! [Authentication — The `.well-known/capsule/*` Registry]:
//!     ../../../../../capsule-docs/src/content/docs/design/authentication.md

use jiff::Timestamp;
use salvo::prelude::*;
use service::federation::Revocations;

use crate::service::well_known::{DeprecationDocument, RevokedJtiDocument, ServerInfo};
use crate::state::AppState;

/// Serve the server's attestation-key history as JSON.
///
/// Publishes this server's attestation keys with an **append-only key history** so a receipt
/// signed years ago still verifies: a receipt's `server_key_id` selects its verification key
/// from this document (SSoT: Federation — Server Identity and Key Rotation).
#[handler]
pub async fn attestation_keys(depot: &mut Depot, res: &mut Response) {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    res.render(Json(state.config.attestation.well_known()));
}

/// Serve the public server-scoped discovery record.
///
/// Built from configuration only — no database handle is taken — so the "**never** a user list"
/// constraint is structural: this handler cannot read user state at all.
#[handler]
pub async fn server_info(depot: &mut Depot, res: &mut Response) {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    res.render(Json(ServerInfo::build(&state.config, Timestamp::now())));
}

/// Serve the federation capability revocation list.
///
/// Peers cache this document with a maximum staleness of 15 minutes and **fail closed** past
/// that bound; publishing it is what makes that rule enforceable rather than aspirational.
#[handler]
pub async fn revoked_jti(depot: &mut Depot, res: &mut Response) {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    let now = Timestamp::now();
    match Revocations::published_jtis(&state.conn, now).await {
        Ok(jtis) => {
            tracing::debug!(count = jtis.len(), "published revocation list");
            res.render(Json(RevokedJtiDocument::new(
                &state.config.server_id,
                now,
                jtis,
            )));
        }
        // A peer must never read a *partial* revocation list as authoritative: an empty or
        // truncated list reads as "nothing is revoked". Failing the fetch instead leaves the
        // peer on its cached copy, which its own staleness bound then fails closed.
        Err(e) => {
            tracing::error!("failed to read the revocation list for publication: {e}");
            res.render(StatusError::internal_server_error());
        }
    }
}

/// Serve the min-supported-client deprecation announcements.
#[handler]
pub async fn deprecation(depot: &mut Depot, res: &mut Response) {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    res.render(Json(DeprecationDocument::build(
        &state.config,
        Timestamp::now(),
    )));
}
