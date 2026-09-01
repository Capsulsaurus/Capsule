//! Shared read/write access to the signed device-directory store (slice `S-C9`).
//!
//! A user publishes a master-signed [`DeviceDirectory`] listing their devices' signing
//! keys; peers and sync consumers fetch it to learn which keys to trust and to resolve a
//! manifest's `created_by_device`. The server stores and serves the signed bytes
//! **opaquely** — it never re-models the document. Its one semantic job is the anti-rollback
//! guard (threat-model invariant 23): a publish is accepted only if its `directory_version`
//! **strictly advances** the version already stored for that user, so the server cannot walk
//! a directory back to re-list a revoked device or hide a freshly-added one.
//!
//! - The **write** side ([`Mutation::publish`]) projects `directory_version` out of the
//!   signed CBOR and performs a single guarded upsert whose `ON CONFLICT … WHERE` clause
//!   enforces strict monotonicity under the row lock, storing the received bytes verbatim.
//! - The **read** side ([`Query::fetch`]) returns the exact signed bytes last published.
//!
//! [`DeviceDirectory`]: capsule_core::crypto::keys::DeviceDirectory

mod mutation;
mod query;

pub use mutation::Mutation;
pub use query::Query;
use thiserror::Error;

/// A device-directory publish/fetch failure. The HTTP surface maps each variant to a stable
/// `error.directory.*` catalog code and status; nothing here is user-facing text.
#[derive(Debug, Error)]
pub enum DirectoryError {
    /// The submitted body is not a decodable signed `DeviceDirectory` (bad CBOR, or a
    /// `directory_version` outside the representable range). Maps to `400`.
    #[error("device directory document is malformed: {0}")]
    Malformed(String),
    /// Invariant 23: the submitted `directory_version` does not strictly advance the version
    /// currently stored for the user. Maps to `409`. `stored` is the current high-water mark
    /// (best-effort, for diagnostics); `submitted` is the rejected version.
    #[error("device directory version {submitted} does not advance the stored version {stored}")]
    VersionConflict { stored: i64, submitted: i64 },
    /// A database failure. Maps to `500`.
    #[error("device directory store error: {0}")]
    Db(#[from] sea_orm::DbErr),
}
