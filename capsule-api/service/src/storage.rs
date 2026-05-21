use capsule_core::models::asset::Asset;

use std::path::PathBuf;

#[derive(Clone)]
pub struct StorageConfig {
    pub upload_dir: PathBuf,
}

#[derive(Clone)]
pub struct StorageService {
    config: StorageConfig,
}

impl StorageService {
    pub fn new(config: StorageConfig) -> Self {
        Self { config }
    }

    pub fn get_upload_path(&self, asset: Asset) -> PathBuf {
        self.get_upload_path_by_ids(
            &asset.id,
            &asset.owner_id,
            asset.album_id.as_deref(),
            &asset.ext,
        )
    }

    pub fn get_upload_path_by_ids(
        &self,
        asset_id: &uuid::Uuid,
        owner_id: &str,
        album_id: Option<&str>,
        ext: &str,
    ) -> PathBuf {
        let asset_id_encoded = data_encoding::BASE32_NOPAD.encode(asset_id.as_bytes());

        let mut dir = self.config.upload_dir.join(owner_id);
        if let Some(album_id) = album_id {
            dir = dir.join(album_id);
        }

        dir = dir.join(format!("{asset_id_encoded}.{ext}"));

        dir
    }

    pub fn get_upload_dir(&self, upload_id: &str) -> PathBuf {
        self.config.upload_dir.join(upload_id)
    }
}
