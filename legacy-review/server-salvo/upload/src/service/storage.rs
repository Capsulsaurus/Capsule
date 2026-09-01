use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;

use tokio::fs;

use crate::config::UploadServerConfig;
use crate::error::UploadError;

/// Service responsible for the physical storage of in-flight uploads on disk.
///
/// Each session owns exactly one append-only file, `{upload_id}.bin`; accepted
/// chunks are appended in order. Sequential offsets are a hard protocol rule, so
/// append-only storage is correct by construction and there is no per-chunk
/// staging or assembly step (upload-protocol design doc, §Append-Only Storage).
#[derive(Clone)]
pub struct StorageService {
    upload_dir: PathBuf,
}

impl StorageService {
    pub(crate) fn new(config: UploadServerConfig) -> Self {
        Self {
            upload_dir: config.upload_dir,
        }
    }

    /// Construct over just the upload directory — the media drop server (S-C5) reuses this
    /// content-addressed store without the full upload-server config.
    pub fn with_upload_dir(upload_dir: PathBuf) -> Self {
        Self { upload_dir }
    }

    /// The session's single upload file (the "incoming" staging area).
    pub fn get_upload_path(&self, upload_id: &str) -> PathBuf {
        self.upload_dir.join(format!("{upload_id}.bin"))
    }

    /// The content-addressed blob store directory (shared addressing — the reader half is
    /// storage verification, slice `S-C3`).
    fn blobs_dir(&self) -> PathBuf {
        service::blob_store::blobs_dir(&self.upload_dir)
    }

    /// The content-addressed path a finalized blob is committed to.
    fn get_blob_path(&self, hash: &str) -> PathBuf {
        service::blob_store::blob_path(&self.upload_dir, hash)
    }

    /// Atomically commit a verified upload into the content-addressed blob store by renaming
    /// `incoming/{upload_id}.bin` to `blobs/{hash}.bin`. Only finalization (after the hash is
    /// verified) calls this. Idempotent on a byte-identical merge — the content address makes
    /// an overwrite harmless.
    pub async fn commit_blob(&self, upload_id: &str, hash: &str) -> Result<(), UploadError> {
        let src = self.get_upload_path(upload_id);
        let dst = self.get_blob_path(hash);
        fs::create_dir_all(self.blobs_dir()).await?;
        fs::rename(&src, &dst).await?;
        Ok(())
    }

    /// Read a committed blob's bytes by its content address. Used to inline the small
    /// encrypted metadata blob onto its sync feed entry (S-C2).
    pub async fn read_committed_blob(&self, hash: &str) -> Result<Vec<u8>, UploadError> {
        Ok(fs::read(self.get_blob_path(hash)).await?)
    }

    /// Write `bytes` directly into the content-addressed blob store at `hash` (via a temp
    /// file + atomic rename). Used by drop adoption (S-C5) to durably store the small
    /// in-memory metadata blob the adopter submits; the content address makes an overwrite of
    /// an identical existing blob harmless.
    pub async fn write_blob(&self, hash: &str, bytes: &[u8]) -> Result<(), UploadError> {
        fs::create_dir_all(self.blobs_dir()).await?;
        let dst = self.get_blob_path(hash);
        let tmp = self.blobs_dir().join(format!("{hash}.tmp"));
        fs::write(&tmp, bytes).await?;
        fs::rename(&tmp, &dst).await?;
        Ok(())
    }

    /// Remove a committed blob by its content address. Used to GC a partial bundle when the
    /// finalization transaction fails after the rename.
    pub async fn remove_blob(&self, hash: &str) -> Result<(), UploadError> {
        let path = self.get_blob_path(hash);
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Appends a chunk at `offset`, returning the file's new length.
    ///
    /// The file's current length is cross-checked against the expected offset
    /// before writing: the file is the on-disk truth and the session counter its
    /// cache, so a divergence is a server-side inconsistency
    /// (`error.upload.storage_inconsistent`), never silently absorbed. The chunk
    /// is flushed to disk before returning — an acknowledged byte is a byte on
    /// disk.
    pub async fn append_at(
        &self,
        upload_id: &str,
        offset: u64,
        data: bytes::Bytes,
    ) -> Result<u64, UploadError> {
        let path = self.get_upload_path(upload_id);

        tokio::task::spawn_blocking(move || {
            let mut file = OpenOptions::new().create(true).append(true).open(&path)?;

            let on_disk = file.metadata()?.len();
            if on_disk != offset {
                return Err(UploadError::StorageInconsistent {
                    expected: offset,
                    on_disk,
                });
            }

            file.write_all(&data)?;
            file.sync_data()?;
            Ok::<u64, UploadError>(on_disk + data.len() as u64)
        })
        .await
        .map_err(|e| UploadError::Unknown(e.to_string()))?
    }

    /// Removes the session's upload file, if present. Used on cancellation,
    /// discard, and failed finalization.
    pub async fn remove(&self, upload_id: &str) -> Result<(), UploadError> {
        let path = self.get_upload_path(upload_id);
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// The on-disk length of the session's upload file, or `None` if it does not exist.
    /// The file length is the on-disk truth against which the session counter is a cache.
    pub async fn file_len(&self, upload_id: &str) -> Result<Option<u64>, UploadError> {
        let path = self.get_upload_path(upload_id);
        match fs::metadata(&path).await {
            Ok(m) => Ok(Some(m.len())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Enumerate the upload ids of every `incoming/{id}.bin` file on disk. Used by the
    /// startup scrub to reconcile the blob store against the session store.
    pub(crate) async fn list_upload_ids(&self) -> Result<Vec<String>, UploadError> {
        let dir = self.upload_dir.clone();
        let mut ids = Vec::new();
        let mut entries = match fs::read_dir(&dir).await {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(ids),
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(id) = name.strip_suffix(".bin") {
                ids.push(id.to_string());
            }
        }
        Ok(ids)
    }
}
