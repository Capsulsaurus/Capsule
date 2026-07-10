use std::path::PathBuf;

use service::quota::QuotaLimits;
use upload::transport::{
    DEFAULT_CONTENT_TYPES, DEFAULT_DRIFT_DAYS, DEFAULT_PROTOCOL_MAX, DEFAULT_PROTOCOL_MIN,
};

/// Media server configuration.
///
/// The web-upload drop server (slice `S-C5`) reuses the S-C1 chunk mechanics, so this config
/// carries the same protocol/content-type/quota knobs the upload server does — pinned to the
/// upload-server defaults so a drop's `content_type`/`protocol_version` window matches the
/// album upload path (invariant 27 is "the same set as invariant 5").
#[derive(Debug, Clone)]
pub struct MediaServerConfig {
    /// Upload directory (the content-addressed blob store lives under it).
    pub upload_dir: PathBuf,
    /// JWT decoding key for owner (session) authentication.
    pub jwt_eddsa_decoding_key: jsonwebtoken::DecodingKey,
    /// Valkey URL backing the drop chunk sessions.
    pub valkey_url: String,
    /// Maximum single ciphertext size in bytes (invariant 28 backstop).
    pub max_file_size: u64,
    /// Lowest accepted protocol date (`YYYY-MM-DD`).
    pub protocol_min: String,
    /// Highest accepted protocol date (`YYYY-MM-DD`).
    pub protocol_max: String,
    /// The closed `content_type` allow-list (invariant 27).
    pub allowed_content_types: Vec<String>,
    /// Gross-drift sanity bound in days for the adoption manifest timestamp (invariant 8).
    pub timestamp_drift_days: i64,
    /// Deployment quota limits (charged to the provisioning owner at drop creation).
    pub quota_limits: QuotaLimits,
    /// Drop-session creations allowed per window, per `{opaque-id}`/source IP (invariant 31).
    pub drop_rate_limit_max: u32,
    /// The drop-session rate-limit window, in seconds.
    pub drop_rate_limit_window_secs: u64,
}

/// Default drop-session creations allowed per window per key (invariant 31).
pub(crate) const DEFAULT_DROP_RATE_LIMIT_MAX: u32 = 60;
/// Default drop-session rate-limit window in seconds.
pub(crate) const DEFAULT_DROP_RATE_LIMIT_WINDOW_SECS: u64 = 60;

impl From<&environment::ServerConfig> for MediaServerConfig {
    fn from(config: &environment::ServerConfig) -> Self {
        Self {
            upload_dir: config.upload_dir.clone(),
            jwt_eddsa_decoding_key: (*config.jwt_eddsa_decoding_key).clone(),
            valkey_url: config.valkey_url.clone(),
            max_file_size: config.max_file_size as u64,
            protocol_min: DEFAULT_PROTOCOL_MIN.to_string(),
            protocol_max: DEFAULT_PROTOCOL_MAX.to_string(),
            allowed_content_types: DEFAULT_CONTENT_TYPES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            timestamp_drift_days: DEFAULT_DRIFT_DAYS,
            // Self-hosted default: no quota enforcement (a hosted deployment overrides per tier).
            quota_limits: QuotaLimits::unlimited(),
            drop_rate_limit_max: DEFAULT_DROP_RATE_LIMIT_MAX,
            drop_rate_limit_window_secs: DEFAULT_DROP_RATE_LIMIT_WINDOW_SECS,
        }
    }
}
