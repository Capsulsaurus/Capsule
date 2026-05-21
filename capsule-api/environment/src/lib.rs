use dotenvy::dotenv;
use std::{env, num::ParseIntError, path::PathBuf};
use thiserror::Error;
use tracing::level_filters::LevelFilter;
use wrapper::SecretKeyWrapper;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use jsonwebtoken::{DecodingKey, EncodingKey};

#[cfg(feature = "auth")]
use crate::constants::{ACCESS_TOKEN_EXPIRY, REFRESH_TOKEN_EXPIRY, TOTP_ISSUER};
#[cfg(feature = "upload")]
use crate::constants::{MAX_CACHE_SIZE, MAX_FILE_SIZE};
use crate::jwt::convert_ed25519_der_to_jwt_keys;

pub mod constants;
mod jwt;
pub mod wrapper;

#[derive(Debug, Error)]
pub enum EnvironmentError {
    /// MissingVariable(key)
    #[error("Environment variable not found: \"{0}\"")]
    MissingVariable(String),
    /// ParseError(key, error)
    #[error("Failed to parse environment variable \"{0}\": {1}")]
    ParseError(String, String),
}

#[derive(Debug, Clone)]
pub struct DatabaseConfig {
    pub url: String,
}

#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Server host (e.g. "0.0.0.0")
    pub host: String,
    /// Server port (e.g. 3000)
    pub port: u16,
    /// Public domain (e.g. "api.capsule.com")
    pub domain: String,

    #[cfg(feature = "auth")]
    /// EdDSA encoding key
    pub jwt_eddsa_encoding_key: SecretKeyWrapper<EncodingKey>,
    // Decoding key is used by all servers so it is not feature gated
    /// EdDSA decoding key
    pub jwt_eddsa_decoding_key: SecretKeyWrapper<DecodingKey>,
    #[cfg(feature = "auth")]
    /// JWT refresh token duration in seconds
    pub jwt_refresh_token_duration_seconds: u64,
    #[cfg(feature = "auth")]
    /// JWT access token duration in seconds
    pub jwt_access_token_duration_seconds: u64,
    #[cfg(feature = "auth")]
    /// TOTP issuer string shown in authenticator apps
    pub totp_issuer: String,

    #[cfg(any(feature = "upload", feature = "media", feature = "sync"))]
    /// Upload directory
    pub upload_dir: PathBuf,
    #[cfg(feature = "upload")]
    /// Maximum file size in bytes
    pub max_file_size: usize,
    #[cfg(feature = "upload")]
    /// Maximum cache size in bytes
    pub max_cache_size: usize,
    #[cfg(feature = "upload")]
    /// Sled database directory
    pub sled_db_dir: PathBuf,

    #[cfg(any(feature = "auth", feature = "upload"))]
    /// Valkey URL (e.g. "redis://127.0.0.1:6379")
    pub valkey_url: String,

    #[cfg(any(feature = "auth", feature = "upload"))]
    /// Allowed CORS origins. Use `["*"]` to allow all origins (development only).
    pub allowed_origins: Vec<String>,
}
// TODO: Separate out these configs into environment variables struct ^^

#[derive(Debug, Clone)]
pub struct Environment {
    pub database: DatabaseConfig,
    pub server: ServerConfig,
    pub log_level: LevelFilter,
}

impl Environment {
    pub fn load() -> Result<Self, EnvironmentError> {
        dotenv().ok();

        let load_env = |key: &str| {
            env::var(key).map_err(|_| EnvironmentError::MissingVariable(key.to_string()))
        };
        let load_env_u16 = |key: &str| {
            load_env(key).and_then(|v| {
                v.parse::<u16>().map_err(|e: ParseIntError| {
                    EnvironmentError::ParseError(key.to_string(), e.to_string())
                })
            })
        };
        let load_env_u64 = |key: &str| {
            load_env(key).and_then(|v| {
                v.parse::<u64>().map_err(|e: ParseIntError| {
                    EnvironmentError::ParseError(key.to_string(), e.to_string())
                })
            })
        };
        let load_env_usize = |key: &str| {
            load_env(key).and_then(|v| {
                v.parse::<usize>().map_err(|e: ParseIntError| {
                    EnvironmentError::ParseError(key.to_string(), e.to_string())
                })
            })
        };

        let load_jwt_ed25519_keys = |key: &str| {
            load_env(key).and_then(|s| {
                let der = BASE64.decode(s).map_err(|e| {
                    EnvironmentError::ParseError(
                        key.to_string(),
                        format!("Unable to decode base64: {}", e),
                    )
                })?;

                convert_ed25519_der_to_jwt_keys(&der).map_err(|e| {
                    EnvironmentError::ParseError(
                        key.to_string(),
                        format!("Unable to convert DER to JWT keys: {}", e),
                    )
                })
            })
        };

        let (jwt_eddsa_encoding_key, jwt_eddsa_decoding_key) =
            load_jwt_ed25519_keys("JWT_ED25519_DER")?;

        let load_log_level = |key: &str| {
            load_env(key).and_then(|s| {
                s.parse::<LevelFilter>()
                    .map_err(|e| EnvironmentError::ParseError(key.to_string(), e.to_string()))
            })
        };

        Ok(Self {
            database: DatabaseConfig {
                url: load_env("DATABASE_URL")?,
            },
            server: ServerConfig {
                host: load_env("SERVER_HOST").unwrap_or("0.0.0.0".to_string()),
                port: load_env_u16("SERVER_PORT").unwrap_or(3000),
                domain: load_env("SERVER_DOMAIN").unwrap_or("localhost".to_string()),
                #[cfg(feature = "auth")]
                jwt_eddsa_encoding_key: SecretKeyWrapper::from(jwt_eddsa_encoding_key),
                jwt_eddsa_decoding_key: SecretKeyWrapper::from(jwt_eddsa_decoding_key.clone()),
                #[cfg(feature = "auth")]
                jwt_refresh_token_duration_seconds: load_env_u64(
                    "JWT_REFRESH_TOKEN_DURATION_SECONDS",
                )
                .unwrap_or(REFRESH_TOKEN_EXPIRY),
                #[cfg(feature = "auth")]
                jwt_access_token_duration_seconds: load_env_u64(
                    "JWT_ACCESS_TOKEN_DURATION_SECONDS",
                )
                .unwrap_or(ACCESS_TOKEN_EXPIRY),
                #[cfg(feature = "auth")]
                totp_issuer: load_env("TOTP_ISSUER").unwrap_or(TOTP_ISSUER.to_string()),
                #[cfg(any(feature = "upload", feature = "media", feature = "sync"))]
                upload_dir: load_env("UPLOAD_DIR")
                    .unwrap_or(String::from("./uploads"))
                    .into(),
                #[cfg(feature = "upload")]
                max_file_size: load_env_usize("MAX_FILE_SIZE").unwrap_or(MAX_FILE_SIZE),
                #[cfg(feature = "upload")]
                max_cache_size: load_env_usize("MAX_CACHE_SIZE").unwrap_or(MAX_CACHE_SIZE),
                #[cfg(feature = "upload")]
                sled_db_dir: load_env("SLED_DB_DIR")
                    .unwrap_or(String::from("./.metadata"))
                    .into(), // TODO: If this is still used
                #[cfg(any(feature = "auth", feature = "upload"))]
                valkey_url: load_env("VALKEY_URL")?,
                #[cfg(any(feature = "auth", feature = "upload"))]
                allowed_origins: load_env("ALLOWED_ORIGINS")
                    .map(|v| {
                        v.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect()
                    })
                    .unwrap_or_else(|_| {
                        if cfg!(debug_assertions) {
                            vec!["*".to_string()]
                        } else {
                            vec![]
                        }
                    }),
            },
            log_level: load_log_level("LOG_LEVEL").unwrap_or(if cfg!(debug_assertions) {
                LevelFilter::TRACE
            } else {
                LevelFilter::INFO
            }),
        })
    }
}
