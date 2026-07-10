use std::path::PathBuf;
use std::sync::Arc;

use environment::ServerConfig;
use environment::wrapper::SecretKeyWrapper;
use jsonwebtoken::DecodingKey;
use service::attestation::{AttestationKeyring, parse_key_history};
use service::quota::{DEFAULT_PER_PEER_BUDGET_RATIO, QuotaLimits, UNLIMITED};

/// The closed `content_type` enum for the current protocol version (invariant 5).
/// Server-tunable, but frozen for a given `protocol_version`. Metadata/provenance/
/// backup blobs are opaque CBOR/ciphertext and declare `application/octet-stream`.
pub const DEFAULT_CONTENT_TYPES: &[&str] = &[
    "image/jpeg",
    "image/png",
    "image/heic",
    "image/heif",
    "image/webp",
    "image/avif",
    "image/gif",
    "image/tiff",
    "video/mp4",
    "video/quicktime",
    "video/webm",
    "application/octet-stream",
];

/// Lowest protocol date this server accepts (`X-Capsule-Protocol-Min`).
pub const DEFAULT_PROTOCOL_MIN: &str = "2026-01-01";
/// Highest protocol date this server accepts (`X-Capsule-Protocol-Max`).
pub const DEFAULT_PROTOCOL_MAX: &str = "2026-12-31";
/// Gross-drift sanity bound for the envelope timestamp, in days (invariant 8).
pub const DEFAULT_DRIFT_DAYS: i64 = 30;

/// Default soft-warning quota threshold in bytes. Unlimited by default — a self-hosted
/// deployment runs with no quota; a hosted service overrides per tier (Quota design doc).
pub const DEFAULT_QUOTA_SOFT_LIMIT: u64 = UNLIMITED;
/// Default hard quota threshold in bytes (unlimited by default).
pub const DEFAULT_QUOTA_HARD_LIMIT: u64 = UNLIMITED;
/// Default grace window, in days, before the Grace-expired state engages.
pub const DEFAULT_QUOTA_GRACE_DAYS: i64 = 14;

#[derive(Clone)]
pub struct UploadServerConfig {
    pub host: String,
    pub port: u16,
    pub domain: String,

    /// Upload directory
    pub upload_dir: PathBuf,
    /// Maximum file size in bytes
    pub max_file_size: usize,
    /// Maximum cache size in bytes
    pub max_cache_size: usize,
    /// Valkey URL
    pub valkey_url: String,
    /// JWT Decoding Key
    pub jwt_eddsa_decoding_key: SecretKeyWrapper<DecodingKey>,
    /// Allowed CORS origins. Use `["*"]` to allow all origins (development only).
    pub allowed_origins: Vec<String>,

    /// Lowest accepted protocol date (`YYYY-MM-DD`); the protocol handshake window.
    pub protocol_min: String,
    /// Highest accepted protocol date (`YYYY-MM-DD`).
    pub protocol_max: String,
    /// The closed `content_type` allow-list (invariant 5).
    pub allowed_content_types: Vec<String>,
    /// Gross-drift sanity bound in days for the envelope timestamp (invariant 8).
    pub timestamp_drift_days: i64,

    /// Soft-warning quota threshold in bytes ([`UNLIMITED`] disables warnings).
    pub quota_soft_limit: u64,
    /// Hard quota threshold in bytes ([`UNLIMITED`] disables enforcement; the self-hosted
    /// default).
    pub quota_hard_limit: u64,
    /// Grace window in days before a hard-exceeded account enters read-only (Grace-expired).
    pub quota_grace_days: i64,
    /// Per-`(receiving_user, source_peer)` federated caching budget as a fraction of the hard
    /// limit.
    pub quota_per_peer_budget_ratio: f64,

    /// The server attestation keyring (slice `S-C15`): the hybrid signing key that seals each
    /// finalized upload's `CustodyReceipt` inside the finalization transaction, plus the
    /// append-only key history. Shared behind an `Arc` — cloning the config is cheap.
    pub attestation: Arc<AttestationKeyring>,
}

impl UploadServerConfig {
    /// The deployment quota limits (Quota design doc) derived from this config.
    pub(crate) fn quota_limits(&self) -> QuotaLimits {
        QuotaLimits {
            soft_limit: self.quota_soft_limit,
            hard_limit: self.quota_hard_limit,
            grace_window: jiff::SignedDuration::from_hours(self.quota_grace_days * 24),
            per_peer_budget_ratio: self.quota_per_peer_budget_ratio,
        }
    }
}

impl From<&ServerConfig> for UploadServerConfig {
    fn from(config: &ServerConfig) -> Self {
        Self {
            host: config.host.clone(),
            port: config.port,
            domain: config.domain.clone(),
            upload_dir: config.upload_dir.clone(),
            max_file_size: config.max_file_size,
            max_cache_size: config.max_cache_size,
            valkey_url: config.valkey_url.clone(),
            jwt_eddsa_decoding_key: config.jwt_eddsa_decoding_key.clone(),
            allowed_origins: config.allowed_origins.clone(),
            protocol_min: DEFAULT_PROTOCOL_MIN.to_string(),
            protocol_max: DEFAULT_PROTOCOL_MAX.to_string(),
            allowed_content_types: DEFAULT_CONTENT_TYPES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
            timestamp_drift_days: DEFAULT_DRIFT_DAYS,
            quota_soft_limit: DEFAULT_QUOTA_SOFT_LIMIT,
            quota_hard_limit: DEFAULT_QUOTA_HARD_LIMIT,
            quota_grace_days: DEFAULT_QUOTA_GRACE_DAYS,
            quota_per_peer_budget_ratio: DEFAULT_PER_PEER_BUDGET_RATIO,
            attestation: Arc::new(AttestationKeyring::new(
                config.domain.clone(),
                &config.attestation_key_seed,
                parse_key_history(config.attestation_key_history.as_deref()),
            )),
        }
    }
}

/// Validate the configuration. Returns error if configuration is valid.
/// Returns a list of warnings if configuration is valid but has potential issues.
pub(crate) fn validate_config(config: &UploadServerConfig) -> Result<Vec<String>, String> {
    let mut warnings = vec![];
    if config.max_file_size >= config.max_cache_size {
        return Err(String::from(
            "max_file_size must be less than max_cache_size",
        ));
    }

    // Warn max_file_size allows < 10 concurrent files
    if config.max_cache_size / config.max_file_size < 10 {
        warnings.push(
            "Based on current max_cache_size, max_file_size allows < 10 concurrent files"
                .to_string(),
        );
    }

    // Warn if upload_dir is a non-empty directory
    if config.upload_dir.is_dir()
        && config
            .upload_dir
            .read_dir()
            .map_err(|e| format!("Unable to read upload directory: {e:?}"))?
            .count()
            > 0
    {
        warnings.push("upload_dir is non-empty. This may be from server restarts.".to_string());
    }

    Ok(warnings)
}
