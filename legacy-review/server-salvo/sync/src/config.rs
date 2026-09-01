use std::path::PathBuf;

use environment::ServerConfig;

/// Lowest protocol version the sync feed negotiates (`x-capsule-protocol`). Mirrors the
/// upload server's window; the negotiation rules are owned by the api-surfaces design doc.
pub const DEFAULT_PROTOCOL_MIN: &str = "2026-01-01";
/// Highest protocol version the sync feed negotiates.
pub const DEFAULT_PROTOCOL_MAX: &str = "2026-12-31";
/// Page size used when the client asks for `0`.
pub const DEFAULT_PAGE_SIZE: u32 = 256;
/// Hard clamp on a client-requested page size.
pub const MAX_PAGE_SIZE: u32 = 1024;

#[derive(Clone)]
pub struct SyncServerConfig {
    pub upload_dir: PathBuf,
    pub jwt_eddsa_decoding_key: jsonwebtoken::DecodingKey,
    /// Accepted protocol window `[min, max]` (`YYYY-MM-DD`).
    pub protocol_min: String,
    pub protocol_max: String,
    /// Server-only HMAC key for the opaque sync cursor (invariant 22).
    pub cursor_mac_key: [u8; 32],
    /// Default page size when the client requests `0`.
    pub default_page_size: u32,
    /// Hard clamp on a client-requested page size.
    pub max_page_size: u32,
    /// CORS-allowed browser origins for the gRPC-web feed carriage (slice `S-D6`). Empty
    /// (the default when unset) allows any origin, mirroring the other browser-facing
    /// routers' permissive default.
    pub allowed_origins: Vec<String>,
}

impl From<&ServerConfig> for SyncServerConfig {
    fn from(config: &ServerConfig) -> Self {
        Self {
            upload_dir: config.upload_dir.clone(),
            jwt_eddsa_decoding_key: (*config.jwt_eddsa_decoding_key).clone(),
            protocol_min: DEFAULT_PROTOCOL_MIN.to_string(),
            protocol_max: DEFAULT_PROTOCOL_MAX.to_string(),
            cursor_mac_key: *config.sync_cursor_mac_key,
            default_page_size: DEFAULT_PAGE_SIZE,
            max_page_size: MAX_PAGE_SIZE,
            allowed_origins: config.allowed_origins.clone(),
        }
    }
}
