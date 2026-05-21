use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod presets;
pub mod types;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VideoFile {
    pub path: PathBuf,
    pub metadata: VideoMetadata,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VideoMetadata {
    // TODO: Add fields
}
