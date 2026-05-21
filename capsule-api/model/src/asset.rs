use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetType {
    #[serde(rename = "ph")]
    Photo,
    #[serde(rename = "vi")]
    Video,
    #[serde(rename = "mp")]
    MotionPhoto,
    #[serde(rename = "sc")]
    Sidecar,
}

impl From<entity::asset::AssetType> for AssetType {
    fn from(t: entity::asset::AssetType) -> Self {
        match t {
            entity::asset::AssetType::Photo => AssetType::Photo,
            entity::asset::AssetType::Video => AssetType::Video,
            entity::asset::AssetType::MotionPhoto => AssetType::MotionPhoto,
            entity::asset::AssetType::Sidecar => AssetType::Sidecar,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CreateAsset {
    pub owner_id: String,
    pub album_id: Option<String>,
    pub width: i32,
    pub height: i32,
    pub asset_type: AssetType,
    pub original_filename: String,
    pub file_size: i64,
    pub file_hash: String,
    pub content_type: String,
    pub date: Option<DateTime<Utc>>,
    pub uploaded: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UpdateAsset {
    pub album_id: Option<Option<String>>,
    pub width: Option<i32>,
    pub height: Option<i32>,
    pub asset_type: Option<AssetType>,
    pub original_filename: Option<String>,
    pub file_size: Option<i64>,
    pub file_hash: Option<i64>,
    pub content_type: Option<String>,
    pub date: Option<Option<DateTime<Utc>>>,
    pub uploaded: Option<bool>,
    pub deleted_at: Option<Option<DateTime<Utc>>>,
}
