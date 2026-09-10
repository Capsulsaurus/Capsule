use thiserror::Error;

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("library already exists at {0}")]
    AlreadyExists(std::path::PathBuf),

    #[error("directory is not empty")]
    DirectoryNotEmpty,

    #[error("corrupt or missing version file: {0}")]
    CorruptVersion(String),

    #[error("library is locked by PID {pid} on {hostname} (locked at {locked_at})")]
    Locked {
        pid: u32,
        hostname: String,
        locked_at: i64,
    },

    #[error("version mismatch: found {found}, expected {expected}")]
    VersionMismatch { found: u8, expected: u8 },

    /// The catalog (`index/library.sqlite`) was stamped by a newer build than this one.
    ///
    /// A refusal, not a downgrade: the catalog is left byte-for-byte untouched and the lock
    /// is released, because an older binary cannot know what invariants the newer schema
    /// added. The recovery is to update Capsule (SSoT: Versioning — Client Catalog
    /// Migration). Typed here rather than flattened into [`Db`](Self::Db) so a client can
    /// tell the user *which* two versions disagree (slice `S-D23`).
    #[error(
        "catalog schema v{found} is newer than this build supports (v{supported}); \
         update Capsule to open this library"
    )]
    CatalogTooNew { found: u32, supported: u32 },

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("CBOR error: {0}")]
    Cbor(String),
}
