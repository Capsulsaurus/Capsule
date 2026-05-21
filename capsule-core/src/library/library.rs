use std::path::PathBuf;

use crate::db::DatabaseDriver;
use crate::library::error::LibraryError;
use crate::library::lock;
use crate::sidecar::LibraryConfigCbor;
use crate::sidecar::io::write_library_config;

/// An open, locked Capsule library.
///
/// Dropping a `Library` automatically releases the lock. For a clean close that
/// also updates `last_opened_at`, call [`Library::close`] explicitly.
#[allow(dead_code)]
pub struct Library {
    pub root: PathBuf,
    pub db: DatabaseDriver,
    config: LibraryConfigCbor,
}

impl Library {
    pub(crate) fn new(root: PathBuf, db: DatabaseDriver, config: LibraryConfigCbor) -> Self {
        Self { root, db, config }
    }

    pub fn config(&self) -> &LibraryConfigCbor {
        &self.config
    }

    /// Update `last_opened_at`, flush config, release lock, and consume `self`.
    /// After this returns `Ok`, the lock has been released and the Library is gone.
    pub fn close(mut self) -> Result<(), LibraryError> {
        self.config.last_opened_at = now_secs();
        let config_path = self.root.join(".library/config.cbor");
        write_library_config(&config_path, &self.config)
            .map_err(|e| LibraryError::Cbor(e.to_string()))?;
        let root = self.root.clone();
        std::mem::forget(self); // Prevent Drop from double-releasing
        lock::release(&root)?;
        Ok(())
    }
}

impl Drop for Library {
    fn drop(&mut self) {
        let _ = lock::release(&self.root);
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
