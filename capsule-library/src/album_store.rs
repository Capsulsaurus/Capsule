//! Persistence of the sealed album-key store at `{root}/.library/albums.cbor`.
//!
//! The store's format, sealing and escrow are `capsule_core::crypto::keys::AlbumStore`; this
//! module owns only the file. It is an extension trait so the call sites keep reading
//! `AlbumStore::load(..)` / `store.save(..)`.

use std::fs;
use std::path::{Path, PathBuf};

use capsule_core::crypto::keys::{ALBUM_STORE_FILE, AlbumStore, AlbumStoreError, MasterKey};

use crate::utils::paths::tmp_path;

/// Reading and durably replacing an [`AlbumStore`] on disk.
pub trait AlbumStoreFile: Sized {
    /// `{root}/.library/albums.cbor`.
    fn path(root: &Path) -> PathBuf;

    /// Load and unseal the store at `{root}/.library/albums.cbor`.
    ///
    /// `Ok(None)` means the file does not exist — a **pre-`S-A10` library**, whose album keys
    /// were session-scoped and were therefore never persisted at all. Such a library opens with
    /// zero albums; its assets are recoverable only through a backup artifact (see
    /// [`Workspace::import_backup`](crate::lifecycle::Workspace::import_backup)). This is not a
    /// regression — those assets were already undecryptable across a restart before the store
    /// existed — but it is a real, permanent limitation of libraries created before it.
    fn load(root: &Path, master: &MasterKey) -> Result<Option<Self>, AlbumStoreError>;

    /// Seal and durably replace the store. Temp-then-rename, so a crash mid-write leaves the
    /// previous good keystore intact rather than a truncated one.
    fn save(&self, root: &Path, master: &MasterKey) -> Result<(), AlbumStoreError>;
}

impl AlbumStoreFile for AlbumStore {
    fn path(root: &Path) -> PathBuf {
        root.join(".library").join(ALBUM_STORE_FILE)
    }

    #[tracing::instrument(skip_all, fields(root = %root.display()))]
    fn load(root: &Path, master: &MasterKey) -> Result<Option<Self>, AlbumStoreError> {
        let path = <Self as AlbumStoreFile>::path(root);
        if !path.exists() {
            tracing::debug!(path = %path.display(), "album store: absent");
            return Ok(None);
        }
        let sealed = fs::read(&path).map_err(|e| AlbumStoreError::Io(e.to_string()))?;
        AlbumStore::open_sealed(&sealed, master).map(Some)
    }

    #[tracing::instrument(skip_all, fields(root = %root.display(), albums = self.albums().len()))]
    fn save(&self, root: &Path, master: &MasterKey) -> Result<(), AlbumStoreError> {
        let sealed = self.seal(master)?;
        let path = <Self as AlbumStoreFile>::path(root);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|e| AlbumStoreError::Io(e.to_string()))?;
        }
        let tmp = tmp_path(&path);
        fs::write(&tmp, &sealed).map_err(|e| AlbumStoreError::Io(e.to_string()))?;
        fs::rename(&tmp, &path).map_err(|e| AlbumStoreError::Io(e.to_string()))?;
        tracing::info!(
            path = %path.display(),
            sealed_bytes = sealed.len(),
            "album store: persisted"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
