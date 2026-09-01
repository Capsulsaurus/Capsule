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
