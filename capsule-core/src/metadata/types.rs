use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssetType {
    /// Photos
    Photo,
    /// Videos
    Video,
    /// Sidecars (related media files)
    Sidecar,
}

impl AssetType {
    /// Detects asset type based on file extension. None if not recognized.
    /// Does not check it is a file or directory. Assumes it is a file.
    pub fn from_file_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?;
        // TODO: Detect from mimetype instead of extension
        match ext.to_lowercase().as_str() {
            "jpg" | "jpeg" | "png" | "gif" => Some(AssetType::Photo),
            "mp4" | "mov" | "avi" => Some(AssetType::Video),
            _ => None,
        }
    }
}
