use crate::config::UploadServerConfig;
use crate::error::UploadError;
#[cfg(target_os = "linux")]
use std::os::unix::io::{AsRawFd, RawFd};
use std::{fs::File, path::PathBuf};
use tokio::fs;

/// Service responsible for managing the physical storage of upload files and chunks on disk.
#[derive(Clone)]
pub struct StorageService {
    config: UploadServerConfig,
}

// Struct for FICLONERANGE ioctl
// Struct for FICLONERANGE ioctl
#[repr(C)]
#[cfg(target_os = "linux")]
struct FileCloneRange {
    src_fd: i64,
    src_offset: u64,
    src_length: u64,
    dest_offset: u64,
}

impl StorageService {
    pub fn new(config: UploadServerConfig) -> Self {
        Self { config }
    }

    /// Generates the filesystem path for a specific chunk of an upload.
    ///
    /// Chunks are stored with the naming convention: `{upload_id}_{chunk_index}.part`
    pub fn get_chunk_path(&self, upload_id: &str, chunk_index: u64) -> PathBuf {
        self.config
            .upload_dir
            .join(format!("{}_{}.part", upload_id, chunk_index))
    }

    /// Combines all chunk files for an upload into a single destination file.
    ///
    /// This method sequentially reads each chunk (0 to num_chunks-1) and writes it to the
    /// destination file defined by `get_upload_path`. After successful combination, the original chunk files are deleted.
    ///
    /// # Arguments
    /// * `upload_id` - The unique identifier of the upload session
    /// * `num_chunks` - The total number of chunks to combine
    pub async fn combine_chunks(
        &self,
        upload_id: &str,
        num_chunks: u64,
    ) -> Result<PathBuf, UploadError> {
        let final_path = self.get_upload_path(upload_id);

        let final_path_clone = final_path.clone();
        let upload_id_str = upload_id.to_string();
        let storage = self.clone();

        tokio::task::spawn_blocking(move || {
            let mut dest = std::fs::File::create(&final_path_clone)
                .map_err(|e| UploadError::Unknown(e.to_string()))?;

            let mut current_offset = 0;

            for i in 0..num_chunks {
                let chunk_path = storage.get_chunk_path(&upload_id_str, i);
                let mut source = std::fs::File::open(&chunk_path)
                    .map_err(|e| UploadError::Unknown(e.to_string()))?;

                let written = Self::append_chunk(&mut dest, &mut source, current_offset, false)?;
                current_offset += written;
            }
            Ok::<(), UploadError>(())
        })
        .await
        .map_err(|e| UploadError::Unknown(e.to_string()))??;

        // Clean up chunks after successful merge
        self.delete_chunks(upload_id).await?;

        Ok(final_path)
    }

    /// Appends a source file to a destination file, attempting reflink first if available.
    ///
    /// # Arguments
    /// * `dest` - Destination file (opened for writing)
    /// * `source` - Source file (opened for reading)
    /// * `current_offset` - Current offset in destination file where source should be appended
    /// * `force_copy` - If true, skips reflink optimization and forces standard copy. useful for testing.
    fn append_chunk(
        dest: &mut File,
        source: &mut File,
        current_offset: u64,
        force_copy: bool,
    ) -> Result<u64, UploadError> {
        let metadata = source
            .metadata()
            .map_err(|e| UploadError::Unknown(e.to_string()))?;
        let size = metadata.len();

        let mut reflink_success = false;

        if !force_copy {
            // Try reflink first. This is a Linux-specific optimization.
            #[cfg(target_os = "linux")]
            {
                let dest_fd = AsRawFd::as_raw_fd(dest);
                let src_fd = AsRawFd::as_raw_fd(source);
                reflink_success = unsafe { attempt_reflink(dest_fd, src_fd, size, current_offset) };
            }
        }

        if !reflink_success {
            // Fallback to copy
            use std::io::{Seek, SeekFrom};
            dest.seek(SeekFrom::Start(current_offset))
                .map_err(|e| UploadError::Unknown(e.to_string()))?;
            std::io::copy(source, dest).map_err(|e| UploadError::Unknown(e.to_string()))?;
        }

        Ok(size)
    }

    /// Counts how many sequential chunks exist on disk for a given upload_id.
    ///
    /// Checks for chunks starting from index 0 and increments until a gap is found.
    pub async fn count_chunks(&self, upload_id: &str) -> Result<u64, UploadError> {
        let mut count = 0;
        loop {
            let path = self.get_chunk_path(upload_id, count);
            if fs::metadata(&path).await.is_err() {
                break;
            }
            count += 1;
        }
        Ok(count)
    }

    /// Delete all chunks for an upload. Used for cleanup on cancellation or after combining.
    ///
    /// Returns the number of chunks successfully deleted.
    pub async fn delete_chunks(&self, upload_id: &str) -> Result<u64, UploadError> {
        let mut deleted = 0;
        loop {
            let chunk_path = self.get_chunk_path(upload_id, deleted);
            if fs::metadata(&chunk_path).await.is_ok() {
                if let Err(e) = fs::remove_file(&chunk_path).await {
                    tracing::warn!("Failed to delete chunk {}: {}", chunk_path.display(), e);
                }
                deleted += 1;
            } else {
                break;
            }
        }
        Ok(deleted)
    }

    /// Gets the path for the final combined upload file (.bin).
    pub fn get_upload_path(&self, upload_id: &str) -> PathBuf {
        self.config.upload_dir.join(format!("{}.bin", upload_id))
    }
}

#[cfg(target_os = "linux")]
unsafe fn attempt_reflink(dest_fd: RawFd, src_fd: RawFd, len: u64, dest_offset: u64) -> bool {
    // FICLONERANGE might not be in the libc crate depending on version/OS
    // but on Linux it's standard.
    // If it's missing, this will fail to compile, and we'll fix it.
    let args = FileCloneRange {
        src_fd: src_fd as i64,
        src_offset: 0,
        src_length: len,
        dest_offset,
    };

    let ret = unsafe { libc::ioctl(dest_fd, libc::FICLONERANGE, &args) };
    ret == 0
}
