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
}

impl From<rusqlite::Error> for CatalogError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database {
            message: e.to_string(),
        }
    }
}
