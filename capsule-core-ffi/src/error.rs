//! The single error type that crosses the UniFFI boundary.

/// An error surfaced by the catalog or sidecar APIs.
///
/// UniFFI maps this to a Swift `enum CatalogError: Error` with associated
/// `message` values, so Swift call sites can `try`/`catch` it directly.
#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum CatalogError {
    /// The underlying SQLite catalog returned an error.
    #[error("database error: {message}")]
    Database { message: String },

    /// A CBOR sidecar payload could not be encoded or decoded, or contained an
    /// invalid enum value.
    #[error("sidecar error: {message}")]
    Sidecar { message: String },

    /// A caller-supplied argument was invalid.
    #[error("invalid argument: {message}")]
    InvalidArgument { message: String },

    /// A view gated by [Local Gallery — SR1] was read without a live fresh-local-auth
    /// grant. Acquire one with [`Catalog::unlock_view`](crate::Catalog::unlock_view) and
    /// retry; the grant covers a short grace window (default 5 minutes, per-view).
    ///
    /// [Local Gallery — SR1]: https://docs/design/local-gallery/#security-requirements
    #[error("view is locked: fresh local authentication is required")]
    ViewLocked,
}

impl From<capsule_core::library::GatedQueryError> for CatalogError {
    fn from(e: capsule_core::library::GatedQueryError) -> Self {
        use capsule_core::library::{GateError, GatedQueryError};
        match e {
            // Every gate refusal is a refusal: `GateError::Auth` cannot actually reach a
            // *query* (the gated read is a pure state check that never challenges the
            // platform), and folding it in here keeps the mapping total without a
            // panicking arm.
            GatedQueryError::Gate(GateError::Locked | GateError::Auth(_)) => Self::ViewLocked,
            GatedQueryError::Db(message) => Self::Database { message },
        }
    }
}

impl From<rusqlite::Error> for CatalogError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database {
            message: e.to_string(),
        }
    }
}
