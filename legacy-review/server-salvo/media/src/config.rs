use std::path::PathBuf;

/// Media server configuration
#[derive(Debug, Clone)]
pub struct MediaServerConfig {
    /// Upload directory
    pub upload_dir: PathBuf,
    /// JWT decoding key for authentication
    pub jwt_eddsa_decoding_key: jsonwebtoken::DecodingKey,
}

impl From<&environment::ServerConfig> for MediaServerConfig {
    fn from(config: &environment::ServerConfig) -> Self {
        Self {
            upload_dir: config.upload_dir.clone(),
            jwt_eddsa_decoding_key: (*config.jwt_eddsa_decoding_key).clone(),
        }
    }
}
