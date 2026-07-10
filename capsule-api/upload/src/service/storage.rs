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
pub(crate) struct StorageService {
    config: UploadServerConfig,
}

impl StorageService {
    pub(crate) fn new(config: UploadServerConfig) -> Self {
        Self { config }
    }

    /// The session's single upload file.
    pub(crate) fn get_upload_path(&self, upload_id: &str) -> PathBuf {
        self.config.upload_dir.join(format!("{upload_id}.bin"))
    }

    /// Appends a chunk at `offset`, returning the file's new length.
    ///
    /// The file's current length is cross-checked against the expected offset
    /// before writing: the file is the on-disk truth and the session counter its
    /// cache, so a divergence is a server-side inconsistency
    /// (`error.upload.storage_inconsistent`), never silently absorbed. The chunk
    /// is flushed to disk before returning — an acknowledged byte is a byte on
    /// disk.
    pub(crate) async fn append_at(
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
    pub(crate) async fn remove(&self, upload_id: &str) -> Result<(), UploadError> {
        let path = self.get_upload_path(upload_id);
        match fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
