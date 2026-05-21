use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Asset {
    /// Asset ID
    pub id: Uuid,
    /// Album ID
    pub album_id: Option<String>,
    /// Owner ID
    pub owner_id: String,
    /// File extension (e.g., "png", "mp4", "json")
    /// Do NOT prepend with a dot (`.`)
    /// String is case-sensitive
    pub ext: String,
}

impl Asset {
    pub fn new(album_id: Option<String>, owner_id: String, ext: String) -> Self {
        Self {
            id: Uuid::now_v7(),
            album_id,
            owner_id,
            ext,
        }
    }
}
