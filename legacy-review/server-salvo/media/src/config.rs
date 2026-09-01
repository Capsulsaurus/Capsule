use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use service::attestation::{AttestationKeyring, parse_key_history};
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
    /// This server's identity (`SERVER_DOMAIN`) — the home-server id a share link is matched
    /// against. A share whose `home_server` differs is not served here; the serve path returns a
    /// `{ home_server }` pointer instead (slice `S-C4`; Security Contract — Home-server-only).
    pub server_id: String,
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
    /// The server attestation keyring (slice `S-C15`): signs `StorageAttestation`s on the
    /// `signed: true` verify path and backs the `.well-known` key publication. The same
    /// keyring the upload server signs receipts with (built from the same env seed).
    pub attestation: Arc<AttestationKeyring>,
    /// This server's classical Ed25519 **operational** public key — the identity that signs
    /// server-to-server requests and federation capability tokens, published in
    /// `.well-known/capsule/server-info` (slice `S-C18`) so a peer can verify a capability it
    /// was handed. Distinct from the hybrid attestation key above (different lifetime, its own
    /// document, its own append-only history). `None` when the deployment's signing key could
    /// not be read — the field is then omitted from the published record rather than
    /// published empty.
    pub operational_public_key: Option<[u8; 32]>,
    /// The min-supported-client deprecation announcements this deployment is publishing
    /// (slice `S-C18`). Empty = nothing is being deprecated, the honest default.
    pub deprecations: Vec<DeprecationAnnouncement>,
    /// The minimum notice, in days, given before a deprecation cutoff (policy default 90).
    pub deprecation_announcement_days: i64,
}

/// One min-supported-client deprecation announcement, as published at
/// `.well-known/capsule/deprecation` and echoed in `server-info`'s active windows.
///
/// The policy ([Threat Model — Min-Supported-Client Deprecation Policy]) requires the
/// announcement to name the **cutoff date** and the **minimum `protocol_version`** that will
/// remain accepted, published at least the announcement window ahead of the cutoff. Dates are
/// `YYYY-MM-DD` (ordered lexicographically = chronologically), matching `protocol_version`.
///
/// [Threat Model — Min-Supported-Client Deprecation Policy]:
///     ../../../../../capsule-docs/src/content/docs/design/threat-model/schema-rules.md
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeprecationAnnouncement {
    /// The date the dropped version leaves the accepted window (`YYYY-MM-DD`).
    pub cutoff: String,
    /// The lowest `protocol_version` that stays accepted after the cutoff.
    pub min_protocol_version: String,
    /// When the cutoff was announced (`YYYY-MM-DD`); the policy's notice period is measured
    /// from here.
    pub announced_at: String,
    /// The semver build the `X-Capsule-Min-Client-Build` response header advertises for this
    /// cutoff, when the deployment pins one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_client_build: Option<String>,
}

/// The policy's default announcement window in days.
pub(crate) const DEFAULT_DEPRECATION_ANNOUNCEMENT_DAYS: i64 = 90;

/// Default drop-session creations allowed per window per key (invariant 31).
pub(crate) const DEFAULT_DROP_RATE_LIMIT_MAX: u32 = 60;
/// Default drop-session rate-limit window in seconds.
pub(crate) const DEFAULT_DROP_RATE_LIMIT_WINDOW_SECS: u64 = 60;

impl From<&environment::ServerConfig> for MediaServerConfig {
    fn from(config: &environment::ServerConfig) -> Self {
        Self {
            server_id: config.domain.clone(),
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
            attestation: Arc::new(AttestationKeyring::new(
                config.domain.clone(),
                &config.attestation_key_seed,
                parse_key_history(config.attestation_key_history.as_deref()),
            )),
            operational_public_key: load_operational_public_key(),
            // No deprecation is announced until an operator configures one; publishing an
            // empty announcement set is the truthful default (nothing is being dropped).
            deprecations: Vec::new(),
            deprecation_announcement_days: DEFAULT_DEPRECATION_ANNOUNCEMENT_DAYS,
        }
    }
}

/// Recover the **public** half of this deployment's operational Ed25519 signing key, for
/// publication in `.well-known/capsule/server-info`.
///
/// `environment::ServerConfig` keeps the key only as `jsonwebtoken`'s opaque `EncodingKey` /
/// `DecodingKey`, neither of which exposes the raw public bytes, so the public half is derived
/// here from the same `JWT_ED25519_DER` the environment loader parsed. The private half is
/// dropped immediately — only the 32 public bytes are retained. (The clean home for this is a
/// public-key field on `ServerConfig` itself; this stays local to the media server until that
/// crate grows one.)
///
/// A missing or unparseable key is **not** fatal: `server-info` simply omits `signing_key`
/// rather than publishing a value a peer could TOFU-pin.
fn load_operational_public_key() -> Option<[u8; 32]> {
    use base64::Engine as _;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    let der = base64::engine::general_purpose::STANDARD
        .decode(std::env::var("JWT_ED25519_DER").ok()?)
        .ok()?;
    let public = Ed25519KeyPair::from_pkcs8_maybe_unchecked(&der)
        .ok()?
        .public_key()
        .as_ref()
        .to_vec();
    let key = <[u8; 32]>::try_from(public.as_slice()).ok();
    if key.is_none() {
        tracing::warn!("operational signing key is not 32 bytes; server-info omits signing_key");
    }
    key
}
