//! The `.well-known/capsule/*` publication documents (slice `S-C18`).
//!
//! Every well-known path Capsule serves is censused in [Authentication — The
//! `.well-known/capsule/*` Registry]; this module owns the **wire shape** of the three
//! server-scoped records the media server publishes beside `attestation-keys`, and builds each
//! one purely from configuration (plus, for `revoked-jti`, the durable revocation table). The
//! routes ([`crate::routes`]) are a thin rendering layer over these builders, so every shape is
//! unit-testable without a socket.
//!
//! - [`ServerInfo`] — public server-scoped facts: the API base URL, the auth + federation
//!   endpoints, the server's operational signing key, the supported `protocol_version` range,
//!   and the `min_protocol_version` cutoffs of any **active** deprecation window. It **never**
//!   carries a user list: a peer that can enumerate a server's users is an abuse and privacy
//!   surface the identity model forbids outright, so this record is built only from
//!   configuration — it has no database access at all, which is what makes the absence
//!   structural rather than a review promise.
//! - [`DeprecationDocument`] — the min-supported-client announcements: each names a cutoff date
//!   and the minimum `protocol_version` that stays accepted, published at least the
//!   announcement window ahead of the cutoff ([Threat Model — Min-Supported-Client Deprecation
//!   Policy]).
//! - [`RevokedJtiDocument`] — the federation capability revocation list, bounded to at most
//!   24 h of revocations. Publishing it is what makes peers' 15-minute fail-closed staleness
//!   rule enforceable ([Federation — Token Lifecycle]): a verifier caches this list and, past
//!   the bound with no successful refresh, rejects every `jti` it can no longer confirm.
//!
//! `moved/{user}` is post-v1 with [Account Portability] and is deliberately absent.
//!
//! [Authentication — The `.well-known/capsule/*` Registry]:
//!     ../../../../../capsule-docs/src/content/docs/design/authentication.md
//! [Account Portability]: ../../../../../capsule-docs/src/content/docs/design/authentication.md
//! [Federation — Token Lifecycle]: ../../../../../capsule-docs/src/content/docs/design/federation.md
//! [Threat Model — Min-Supported-Client Deprecation Policy]:
//!     ../../../../../capsule-docs/src/content/docs/design/threat-model/schema-rules.md

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use jiff::Timestamp;
use jiff::tz::TimeZone;
use serde::{Deserialize, Serialize};

use crate::config::{DeprecationAnnouncement, MediaServerConfig};

/// The algorithm identifier published for the classical Ed25519 **operational** key — the
/// identity that signs server-to-server requests and federation capability tokens (the
/// operational-signature carve-out; it is deliberately *not* the hybrid attestation key, which
/// has its own document and its own append-only history).
pub(crate) const OPERATIONAL_KEY_ALGORITHM: &str = "ed25519";

/// The gRPC service path the sync feed is mounted at, at the server root (a peer pulls the same
/// feed a client does — federation introduces no new data protocol).
const SYNC_SERVICE_PATH: &str = "capsule.sync.v1.SyncService";

// ─── server-info ──────────────────────────────────────────────────────────────

/// The server's published operational signing key. No key history: nothing the operational key
/// signs outlives it ([Federation — Server Identity and Key Rotation]), so a rotation simply
/// replaces this record — unlike the attestation key, whose retired entries are never dropped.
///
/// [Federation — Server Identity and Key Rotation]:
///     ../../../../../capsule-docs/src/content/docs/design/federation.md
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PublishedSigningKey {
    /// The signature algorithm identifier (`ed25519`).
    pub algorithm: String,
    /// The 32-byte Ed25519 public key, base64.
    pub public: String,
}

/// The closed `[Min, Max]` protocol window this server accepts, the same window every response
/// advertises in `X-Capsule-Protocol-Min` / `-Max`. Dates are `YYYY-MM-DD`, ordered
/// lexicographically = chronologically.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProtocolWindow {
    /// The lowest protocol version accepted.
    pub min: String,
    /// The highest protocol version accepted.
    pub max: String,
}

/// Where a client authenticates. Absolute URLs so a discovery record is self-contained.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AuthEndpoints {
    /// Password (+ TOTP) login.
    pub login: String,
    /// Account registration.
    pub register: String,
    /// Access-token refresh from a session token.
    pub refresh: String,
    /// The passkey ceremony base (`register/start|finish`, `login/start|finish`).
    pub passkey: String,
}

/// Where a **peer** pulls. Federation introduces no new data protocol: a peer fetches the same
/// content-addressed primitives a client does — the sync feed and `GET /blob/{hash}` — so the
/// federation surface is exactly these two existing endpoints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct FederationEndpoints {
    /// The gRPC sync-feed service a peer pulls pages from (mounted at the server root, because
    /// gRPC addresses a method by its fully-qualified path).
    pub sync_feed: String,
    /// The content-addressed ciphertext fetch base; a blob is `{blob}/{hash}`.
    pub blob: String,
}

/// `GET /.well-known/capsule/server-info` — public, unauthenticated, server-scoped facts only.
///
/// **Never a user list.** User lookup is authenticated (session token, federation capability, or
/// the opt-in anonymous WebFinger record); this document is the discovery root and nothing more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ServerInfo {
    /// This server's canonical origin (the handle suffix in `user@server.tld`).
    pub server_id: String,
    /// The versioned REST API base every endpoint below is relative to.
    pub api_base_url: String,
    /// The auth endpoints a client bootstraps a session through.
    pub auth: AuthEndpoints,
    /// The endpoints a federated peer pulls from.
    pub federation: FederationEndpoints,
    /// This server's operational signing key — peers verify capability tokens under it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signing_key: Option<PublishedSigningKey>,
    /// The `protocol_version` window this server accepts.
    pub protocol_version: ProtocolWindow,
    /// The `min_protocol_version` cutoffs of every **active** deprecation window (an
    /// announcement whose cutoff has passed is no longer a window — it is simply the current
    /// `protocol_version.min`). Empty when nothing is being deprecated.
    pub deprecations: Vec<DeprecationAnnouncement>,
}

impl ServerInfo {
    /// Build the record from configuration alone as of `now`. No database handle is taken, by
    /// construction: this document cannot leak user state it cannot read.
    #[must_use]
    pub(crate) fn build(config: &MediaServerConfig, now: Timestamp) -> Self {
        let origin = origin_of(&config.server_id);
        let api_base_url = format!("{origin}/v1");
        Self {
            server_id: config.server_id.clone(),
            auth: AuthEndpoints {
                login: format!("{api_base_url}/auth/login"),
                register: format!("{api_base_url}/auth/register"),
                refresh: format!("{api_base_url}/auth/refresh"),
                passkey: format!("{api_base_url}/auth/passkey"),
            },
            federation: FederationEndpoints {
                sync_feed: format!("{origin}/{SYNC_SERVICE_PATH}"),
                blob: format!("{api_base_url}/blob"),
            },
            signing_key: config
                .operational_public_key
                .map(|key| PublishedSigningKey {
                    algorithm: OPERATIONAL_KEY_ALGORITHM.to_string(),
                    public: BASE64.encode(key),
                }),
            protocol_version: ProtocolWindow {
                min: config.protocol_min.clone(),
                max: config.protocol_max.clone(),
            },
            deprecations: active_deprecations(config, now),
            api_base_url,
        }
    }
}

// ─── deprecation ──────────────────────────────────────────────────────────────

/// `GET /.well-known/capsule/deprecation` — the min-supported-client announcements.
///
/// Dropping a `protocol_version` from the accepted window is a breaking change, so it is
/// announced here at least `announcement_window_days` ahead of the cutoff. The surface is never
/// retroactive: albums pinned to a dropped version stay readable forever, only writes are
/// refused past the cutoff.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct DeprecationDocument {
    /// This server's canonical origin.
    pub server_id: String,
    /// The minimum notice this deployment gives before a cutoff (default 90 days).
    pub announcement_window_days: i64,
    /// Every announcement whose cutoff has not yet passed, earliest cutoff first.
    pub announcements: Vec<DeprecationAnnouncement>,
}

impl DeprecationDocument {
    /// Build the record from configuration alone as of `now`.
    #[must_use]
    pub(crate) fn build(config: &MediaServerConfig, now: Timestamp) -> Self {
        Self {
            server_id: config.server_id.clone(),
            announcement_window_days: config.deprecation_announcement_days,
            announcements: active_deprecations(config, now),
        }
    }
}

// ─── revoked-jti ──────────────────────────────────────────────────────────────

/// `GET /.well-known/capsule/revoked-jti` — the federation capability revocation list.
///
/// Bounded to at most 24 h of revocations by construction: a capability's `exp` is never more
/// than 24 h after its `iat`, and a row is pruned once its `exp` passes (an expired token is
/// rejected unconditionally anyway). A peer caches this document and honors it for at most
/// 15 minutes of staleness, failing **closed** past that bound.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RevokedJtiDocument {
    /// The issuing server's canonical origin — a revocation list is only meaningful for the
    /// issuer whose tokens it revokes.
    pub server_id: String,
    /// The issuer's clock when the list was rendered (RFC 3339); the peer's staleness bound is
    /// measured against its own successful fetch, this is the issuer's provenance stamp.
    pub issued_at: String,
    /// The publication bound in hours (24) — the list holds no revocation whose token could
    /// still be live beyond it.
    pub window_hours: i64,
    /// The revoked, not-yet-expired capability ids, ordered.
    pub revoked_jti: Vec<String>,
}

impl RevokedJtiDocument {
    /// Assemble the document over an already-loaded `jti` set (the durable read lives in
    /// [`service::federation::Revocations::published_jtis`]).
    #[must_use]
    pub(crate) fn new(server_id: &str, now: Timestamp, revoked_jti: Vec<String>) -> Self {
        Self {
            server_id: server_id.to_string(),
            issued_at: now.to_string(),
            window_hours: service::federation::Revocations::PUBLICATION_WINDOW_HOURS,
            revoked_jti,
        }
    }
}

// ─── shared helpers ───────────────────────────────────────────────────────────

/// The announcements still in force as of `now`, earliest cutoff first. A `YYYY-MM-DD` cutoff
/// orders lexicographically = chronologically, so the comparison needs no date parsing and a
/// malformed configured value can never panic a public read path.
fn active_deprecations(config: &MediaServerConfig, now: Timestamp) -> Vec<DeprecationAnnouncement> {
    let today = now.to_zoned(TimeZone::UTC).date().to_string();
    let mut active: Vec<DeprecationAnnouncement> = config
        .deprecations
        .iter()
        .filter(|a| a.cutoff.as_str() >= today.as_str())
        .cloned()
        .collect();
    active.sort_by(|a, b| a.cutoff.cmp(&b.cutoff));
    active
}

/// The server's public origin. `server_id` is the deployment's public domain; federation and
/// client traffic are TLS-only, so the scheme is always `https` unless the operator already
/// configured a full origin.
fn origin_of(server_id: &str) -> String {
    if server_id.starts_with("http://") || server_id.starts_with("https://") {
        server_id.trim_end_matches('/').to_string()
    } else {
        format!("https://{}", server_id.trim_end_matches('/'))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> MediaServerConfig {
        MediaServerConfig {
            server_id: "capsule.example".to_string(),
            upload_dir: std::path::PathBuf::from("/tmp/does-not-exist"),
            jwt_eddsa_decoding_key: jsonwebtoken::DecodingKey::from_secret(b"k"),
            valkey_url: String::new(),
            max_file_size: 1,
            protocol_min: "2026-01-01".to_string(),
            protocol_max: "2026-12-31".to_string(),
            allowed_content_types: Vec::new(),
            timestamp_drift_days: 30,
            quota_limits: service::quota::QuotaLimits::unlimited(),
            drop_rate_limit_max: 1,
            drop_rate_limit_window_secs: 1,
            attestation: std::sync::Arc::new(service::attestation::AttestationKeyring::new(
                "capsule.example".to_string(),
                &[3u8; 64],
                Vec::new(),
            )),
            operational_public_key: Some([9u8; 32]),
            deprecations: Vec::new(),
            deprecation_announcement_days: 90,
        }
    }

    fn now() -> Timestamp {
        "2026-08-21T00:00:00Z".parse().expect("fixed clock")
    }

    /// Every URL the discovery record publishes is absolute, `https`, and rooted at the
    /// configured origin — a peer that has only the handle suffix can reach the server.
    #[test]
    fn server_info_urls_are_absolute_under_the_configured_origin() {
        let info = ServerInfo::build(&config(), now());
        assert_eq!(info.api_base_url, "https://capsule.example/v1");
        assert_eq!(info.auth.login, "https://capsule.example/v1/auth/login");
        assert_eq!(
            info.auth.register,
            "https://capsule.example/v1/auth/register"
        );
        assert_eq!(info.auth.refresh, "https://capsule.example/v1/auth/refresh");
        assert_eq!(info.auth.passkey, "https://capsule.example/v1/auth/passkey");
        assert_eq!(info.federation.blob, "https://capsule.example/v1/blob");
        assert_eq!(
            info.federation.sync_feed,
            "https://capsule.example/capsule.sync.v1.SyncService"
        );
    }

    /// The published window is exactly the window the server enforces, and the signing key is
    /// the configured operational key.
    #[test]
    fn server_info_publishes_the_enforced_window_and_signing_key() {
        let cfg = config();
        let info = ServerInfo::build(&cfg, now());
        assert_eq!(info.protocol_version.min, cfg.protocol_min);
        assert_eq!(info.protocol_version.max, cfg.protocol_max);
        let key = info.signing_key.expect("operational key published");
        assert_eq!(key.algorithm, OPERATIONAL_KEY_ALGORITHM);
        assert_eq!(key.public, BASE64.encode([9u8; 32]));
    }

    /// A deployment that never configured an operational key publishes no `signing_key` field
    /// at all, rather than a null or an empty string a peer might TOFU-pin.
    #[test]
    fn an_unconfigured_signing_key_is_omitted_not_empty() {
        let mut cfg = config();
        cfg.operational_public_key = None;
        let json = serde_json::to_value(ServerInfo::build(&cfg, now())).expect("serialize");
        assert!(
            json.get("signing_key").is_none(),
            "an absent key must be absent, not null"
        );
    }

    /// Only announcements whose cutoff is still ahead are an *active* window; a past cutoff has
    /// already moved into `protocol_version.min` and is dropped from both documents.
    #[test]
    fn expired_announcements_leave_the_active_window() {
        let mut cfg = config();
        cfg.deprecations = vec![
            DeprecationAnnouncement {
                cutoff: "2027-01-01".to_string(),
                min_protocol_version: "2026-06-01".to_string(),
                announced_at: "2026-08-01".to_string(),
                min_client_build: Some("2.0.0".to_string()),
            },
            DeprecationAnnouncement {
                cutoff: "2026-01-01".to_string(),
                min_protocol_version: "2025-06-01".to_string(),
                announced_at: "2025-08-01".to_string(),
                min_client_build: None,
            },
        ];
        let info = ServerInfo::build(&cfg, now());
        assert_eq!(
            info.deprecations.len(),
            1,
            "only the future cutoff is active"
        );
        assert_eq!(info.deprecations[0].cutoff, "2027-01-01");

        let doc = DeprecationDocument::build(&cfg, now());
        assert_eq!(
            doc.announcements, info.deprecations,
            "one shared record set"
        );
        assert_eq!(doc.announcement_window_days, 90);
    }

    /// Announcements are ordered by cutoff so the nearest deadline reads first.
    #[test]
    fn announcements_are_ordered_by_cutoff() {
        let mut cfg = config();
        cfg.deprecations = vec![
            DeprecationAnnouncement {
                cutoff: "2028-01-01".to_string(),
                min_protocol_version: "2027-06-01".to_string(),
                announced_at: "2027-08-01".to_string(),
                min_client_build: None,
            },
            DeprecationAnnouncement {
                cutoff: "2027-01-01".to_string(),
                min_protocol_version: "2026-06-01".to_string(),
                announced_at: "2026-08-01".to_string(),
                min_client_build: None,
            },
        ];
        let cutoffs: Vec<String> = DeprecationDocument::build(&cfg, now())
            .announcements
            .into_iter()
            .map(|a| a.cutoff)
            .collect();
        assert_eq!(cutoffs, vec!["2027-01-01", "2028-01-01"]);
    }

    /// Each record survives the wire round trip it is published over.
    #[test]
    fn every_record_round_trips_through_json() {
        let cfg = config();
        let info = ServerInfo::build(&cfg, now());
        let back: ServerInfo =
            serde_json::from_str(&serde_json::to_string(&info).expect("ser")).expect("de");
        assert_eq!(back, info);

        let dep = DeprecationDocument::build(&cfg, now());
        let back: DeprecationDocument =
            serde_json::from_str(&serde_json::to_string(&dep).expect("ser")).expect("de");
        assert_eq!(back, dep);

        let rev = RevokedJtiDocument::new("capsule.example", now(), vec!["a".to_string()]);
        let back: RevokedJtiDocument =
            serde_json::from_str(&serde_json::to_string(&rev).expect("ser")).expect("de");
        assert_eq!(back, rev);
        assert_eq!(rev.window_hours, 24, "the publication bound is 24 hours");
    }
}
