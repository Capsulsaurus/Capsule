use std::path::Path;
use std::{fs, io, ops::Deref};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::utils::hash::get_file_hash;

#[derive(Clone, Serialize, Deserialize)]
pub struct HashData(String);

impl std::fmt::Debug for HashData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<String> for HashData {
    fn from(value: String) -> Self {
        HashData(value)
    }
}

impl Deref for HashData {
    type Target = String;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    /// BLAKE3 hash (64-char lowercase hex)
    pub hash_blake3: HashData,
    /// File size in bytes
    pub size: u64,
    // /// Media type if available
    // pub media_type: Option<String>,
    /// Original file name
    pub original_filename: String,
    /// Creation timestamp
    pub created_timestamp: DateTime<Utc>,
    /// Last modified timestamp
    pub modified_timestamp: DateTime<Utc>,

    /// Import timestamp
    pub import_timestamp: DateTime<Utc>,
    // pub size: u64,
    // pub modified: u64,
}

impl FileMetadata {
    /// Generate file metadata from a path
    pub async fn from_file_path(path: &Path) -> io::Result<FileMetadata> {
        let metadata = fs::metadata(path)?;

        // Get file hash
        let hash = get_file_hash(path)?;

        // Get file size
        let size = metadata.len();

        // Extract filename from path
        let original_filename = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Get timestamps
        let created_timestamp = metadata
            .created()
            .map(DateTime::<Utc>::from)
            .unwrap_or_else(|_| Utc::now());

        let modified_timestamp = metadata
            .modified()
            .map(DateTime::<Utc>::from)
            .unwrap_or_else(|_| Utc::now());

        let import_timestamp = Utc::now();

        // Detect media (MIME) type
        // let media_type = ...;

        Ok(FileMetadata {
            hash_blake3: hash.into(),
            size,
            // media_type,
            original_filename,
            created_timestamp,
            modified_timestamp,
            import_timestamp,
        })
    }
}
