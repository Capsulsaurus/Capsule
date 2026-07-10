//! `GET /.well-known/capsule/attestation-keys` — the attestation-key publication (slice
//! `S-C15`).
//!
//! Publishes this server's attestation keys with an **append-only key history** so a receipt
//! signed years ago still verifies: a receipt's `server_key_id` selects its verification key
//! from this document (SSoT: Federation — Server Identity and Key Rotation). Public and
//! unauthenticated — clients pin it (TOFU) on first contact.

use salvo::prelude::*;

use crate::state::AppState;

/// Serve the server's attestation-key history as JSON.
#[handler]
pub async fn attestation_keys(depot: &mut Depot, res: &mut Response) {
    let state = depot
        .obtain::<AppState>()
        .expect("AppState is injected by middleware");
    res.render(Json(state.config.attestation.well_known()));
}
