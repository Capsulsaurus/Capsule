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

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("CBOR error: {0}")]
    Cbor(String),
}
