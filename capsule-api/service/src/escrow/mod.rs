//! Shared read/write access to the master-key escrow store (slice `S-C12`).
//!
//! A signed-in user stores the passphrase-wrapped account master key (`capsule_core::backup`
//! wrap format) so a holder of the ≥128-bit recovery secret can reconstruct the key
//! hierarchy after every device is lost. The server keeps the blob **opaque** — it never
//! interprets, re-models, or decrypts the bytes; its only checks are ownership (the caller
//! stores and fetches only their own escrow) and a coarse size sanity bound. The entropy
//! floor on the recovery secret is a client-side rule enforced in core, not re-validated here.
//!
//! **Single active escrow.** There is one escrow per user, keyed by `user_id`:
//! - The **write** side ([`Mutation::store`]) is a guarded upsert that overwrites the row
//!   in place, so a store-or-replace deletes the prior ciphertext in the same statement —
//!   after a replace the old blob is gone and unwraps nothing (the guided re-wrap contract:
//!   the lost secret must reach nothing).
//! - The **read** side ([`Query::fetch`]) returns the exact bytes last stored for the caller.

mod mutation;
mod query;

pub use mutation::Mutation;
pub use query::Query;
use thiserror::Error;

/// A master-key escrow store/fetch failure. The HTTP surface maps each variant to a stable
/// `error.escrow.*` catalog code and status; nothing here is user-facing text.
#[derive(Debug, Error)]
pub enum EscrowError {
    /// The submitted escrow blob failed the coarse size sanity bound — empty, or larger than
    /// any legitimate wrapped 32-byte master key. Maps to `400`. The server never inspects
    /// the wrap format beyond this; the recovery-secret entropy floor is a client-side rule.
    #[error("escrow blob failed the size sanity bound: {0}")]
    Malformed(String),
    /// A database failure. Maps to `500`.
    #[error("escrow store error: {0}")]
    Db(#[from] sea_orm::DbErr),
}
