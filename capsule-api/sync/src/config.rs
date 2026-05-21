use environment::ServerConfig;
use std::path::PathBuf;

#[derive(Clone)]
pub struct SyncServerConfig {
    pub upload_dir: PathBuf,
    pub jwt_eddsa_decoding_key: jsonwebtoken::DecodingKey,
}

impl From<&ServerConfig> for SyncServerConfig {
    fn from(config: &ServerConfig) -> Self {
        Self {
            upload_dir: config.upload_dir.clone(),
            jwt_eddsa_decoding_key: (*config.jwt_eddsa_decoding_key).clone(),
        }
    }
}
