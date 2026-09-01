//! Persistent session storage for the CLI (slice `S-D5`).
//!
//! The `capsule-sdk` owns the session/token store and the refresh engine (slice
//! `S-D7`); the CLI owns only the **at-rest bytes** — exactly the boundary
//! [`capsule_sdk::auth::PersistedSession`] documents. `capsule auth login` exports
//! that snapshot here; `capsule sync` reloads it and hands it back to
//! [`capsule_sdk::auth::AuthClient::resume`]; `capsule auth logout` clears it.
//!
//! On Linux the CLI has no OS keychain to lean on, so the token pair is written to
//! a file with **owner-only permissions** (`0600` on Unix), scoped to the config
//! directory. Tokens are secret material: this module never logs them, and the
//! serialized form carries no field a log line would want.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use capsule_sdk::auth::PersistedSession;
use secrecy::ExposeSecret;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Everything the session store can fail with.
#[derive(Debug, Error)]
pub enum SessionError {
    /// Reading, writing, or removing the session file failed.
    #[error("session store I/O error at {path}: {source}")]
    Io {
        /// The offending path.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The session file exists but did not deserialize.
    #[error("malformed session file at {path}: {source}")]
    Decode {
        /// The offending path.
        path: String,
        /// The underlying (de)serialization error.
        source: serde_json::Error,
    },
}

/// The on-disk shape. Deliberately minimal — the SDK reconstructs the live session
/// (and its refresh engine) from these three fields via `AuthClient::resume`.
#[derive(Serialize, Deserialize)]
struct StoredSession {
    access_token: String,
    refresh_token: String,
    access_expires_at_unix: i64,
}

/// A file-backed store for the SDK's [`PersistedSession`].
pub struct SessionStore {
    path: PathBuf,
}

impl SessionStore {
    /// A store backed by `path` (typically `<config_dir>/session.json`).
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The backing file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Load the persisted session, or `None` when the user is not logged in.
    pub fn load(&self) -> Result<Option<PersistedSession>, SessionError> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(source) => return Err(self.io_error(source)),
        };
        let stored: StoredSession =
            serde_json::from_slice(&bytes).map_err(|source| SessionError::Decode {
                path: self.path.display().to_string(),
                source,
            })?;
        Ok(Some(PersistedSession {
            access_token: stored.access_token.into(),
            refresh_token: stored.refresh_token.into(),
            access_expires_at_unix: stored.access_expires_at_unix,
        }))
    }

    /// Persist a session snapshot with owner-only file permissions.
    pub fn save(&self, session: &PersistedSession) -> Result<(), SessionError> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| self.io_error(source))?;
        }
        let stored = StoredSession {
            access_token: session.access_token.expose_secret().to_string(),
            refresh_token: session.refresh_token.expose_secret().to_string(),
            access_expires_at_unix: session.access_expires_at_unix,
        };
        let json = serde_json::to_vec_pretty(&stored).map_err(|source| SessionError::Decode {
            path: self.path.display().to_string(),
            source,
        })?;
        self.write_owner_only(&json)
            .map_err(|source| self.io_error(source))
    }

    /// Remove the persisted session. Idempotent: a missing file is success.
    pub fn clear(&self) -> Result<(), SessionError> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(self.io_error(source)),
        }
    }

    /// Write `bytes` to the session file, forcing `0600` on Unix so the tokens are
    /// never world- or group-readable.
    fn write_owner_only(&self, bytes: &[u8]) -> std::io::Result<()> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let mut file = options.open(&self.path)?;
        // `mode` above only applies when the file is newly created; force the
        // permissions on a pre-existing file too.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(bytes)?;
        file.flush()
    }

    fn io_error(&self, source: std::io::Error) -> SessionError {
        SessionError::Io {
            path: self.path.display().to_string(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "capsule-cli-session-{tag}-{}.json",
            nanoid::nanoid!()
        ))
    }

    /// A saved session round-trips through the file store byte-for-byte.
    #[test]
    fn save_then_load_round_trips_the_token_pair() {
        let path = temp_path("roundtrip");
        let store = SessionStore::new(&path);
        assert!(store.load().unwrap().is_none(), "empty store loads None");

        let session = PersistedSession {
            access_token: "access-xyz".into(),
            refresh_token: "refresh-abc".into(),
            access_expires_at_unix: 1_900_000_000,
        };
        store.save(&session).unwrap();

        let loaded = store.load().unwrap().expect("session present after save");
        assert_eq!(loaded.access_token.expose_secret(), "access-xyz");
        assert_eq!(loaded.refresh_token.expose_secret(), "refresh-abc");
        assert_eq!(loaded.access_expires_at_unix, 1_900_000_000);

        store.clear().unwrap();
        assert!(store.load().unwrap().is_none(), "cleared store loads None");
        // Clearing again is idempotent.
        store.clear().unwrap();
    }

    /// The session file is written owner-only (`0600`) so the tokens are not
    /// readable by other users on a shared host.
    #[cfg(unix)]
    #[test]
    fn session_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = temp_path("perms");
        let store = SessionStore::new(&path);
        store
            .save(&PersistedSession {
                access_token: "a".into(),
                refresh_token: "r".into(),
                access_expires_at_unix: 1,
            })
            .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "session file must be owner-only");
        store.clear().unwrap();
    }

    /// A corrupt session file surfaces a typed decode error, not a silent empty.
    #[test]
    fn malformed_file_is_a_decode_error() {
        let path = temp_path("malformed");
        std::fs::write(&path, b"not json").unwrap();
        let store = SessionStore::new(&path);
        assert!(matches!(store.load(), Err(SessionError::Decode { .. })));
        let _ = std::fs::remove_file(&path);
    }
}
