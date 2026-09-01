//! Durable federation state — peer identity and the capability revocation list (slice `S-E2`).
//!
//! The **runtime** federation logic (capability issue/verify, per-peer compartmentalization,
//! the soft-fail rejected-hash cache, the fail-closed revocation-list staleness check) lives in
//! `capsule-api-sync::federation`; this module owns only the two pieces that must survive a
//! restart and be queried from Postgres:
//!
//! 1. [`Peers`] — peer identity. **Reconciliation with S-C8:** federation reuses the single
//!    [`federation_peers`](entity::federation_peer) table S-C8 registered — there is **no**
//!    second peer-identity store. A row maps a peer's canonical origin to its published Ed25519
//!    operational key. S-C8's report intake verifies a report's signature against this key; S-E2
//!    grounds a pulling peer's identity in the same table (first contact = no row = probationary
//!    tier, and a remote issuer's capability is verified against the key on file here). The home
//!    server signs its **own** capabilities with its configured operational key and identifies
//!    the requesting peer by the token's `sub`.
//! 2. [`Revocations`] — the durable [`federation_revoked_jti`](entity::federation_revoked_jti)
//!    list. The issuing server publishes the active rows as its `/.well-known/capsule/revoked-jti`
//!    document and consults them when verifying its own tokens. Rows are pruned once `exp`
//!    passes, so the list stays bounded by at most 24 hours of revocations.
//!
//! SSoT: the [Federation design doc](https://docs/design/federation/).

mod peers;
mod revocation;

pub use peers::Peers;
pub use revocation::Revocations;
