//! Server moderation hooks (slice `S-C8`).
//!
//! Capsule is end-to-end encrypted, so the server **cannot** scan content it holds — no
//! server-side content or CSAM scanner exists or ever will. Moderation operates entirely on
//! what *is* available: user reports, account-level signals, and federated peer reputation
//! (SSoT: the [Moderation design doc][doc]). This module owns the four operational hooks:
//!
//! 1. **Federated reporting** ([`report`]) — signed, rate-limited intake of a peer's
//!    moderation report into the admin queue (threat-model invariant 24). A report carries
//!    the alleged asset's content hash + album pointer only, never plaintext or key material.
//! 2. **Blocklists** ([`blocklist`]) — a server-level blocklist (a blocked peer's federated
//!    requests are refused) and a per-user block (removes the blocked user from the blocker's
//!    shared albums; deliberately *not* a server-wide federation block).
//! 3. **Takedown** ([`takedown`]) — marks an asset unservable (`served = false`, `410 Gone`
//!    to peers) and appends a user-visible moderation provenance record; the blob is never
//!    deleted.
//! 4. **Suspension** ([`suspension`]) — an admin/billing account flag; enforcement refuses
//!    upload-session creation with [`MODERATION_ACCOUNT_SUSPENDED`], wired into slice `S-C1`'s
//!    create path.
//!
//! Every action that touches user data appends a [`moderation_event`](entity::moderation_event)
//! — the "no silent operations" rule.
//!
//! [doc]: ../../../capsule-docs/src/content/docs/design/moderation.md
//! [`MODERATION_ACCOUNT_SUSPENDED`]: capsule_i18n::error_codes::MODERATION_ACCOUNT_SUSPENDED

pub mod blocklist;
pub mod report;
pub mod suspension;
pub mod takedown;

pub use blocklist::Blocklist;
use capsule_i18n::error_codes;
pub use report::{Report, ReportCore, SignedReport};
use sea_orm::DbErr;
pub use suspension::Suspension;
pub use takedown::{AuditLog, ModerationEventKind, Takedown};
use thiserror::Error;

/// Default per-`(reporting_server, reported_user)` report budget within the window.
pub const DEFAULT_REPORT_RATE_MAX: u64 = 20;

/// Default federated-report rate-limit window: one hour.
pub const DEFAULT_REPORT_RATE_WINDOW: jiff::SignedDuration = jiff::SignedDuration::from_hours(1);

/// Tunable moderation policy limits. Self-hosted deployments accept the defaults.
#[derive(Debug, Clone, Copy)]
pub struct ModerationLimits {
    /// Max accepted reports per `(reporting_server, reported_user)` per window.
    pub report_rate_max: u64,
    /// The report rate-limit window.
    pub report_rate_window: jiff::SignedDuration,
}

impl Default for ModerationLimits {
    fn default() -> Self {
        Self {
            report_rate_max: DEFAULT_REPORT_RATE_MAX,
            report_rate_window: DEFAULT_REPORT_RATE_WINDOW,
        }
    }
}

/// A moderation failure. Each client/peer-visible variant maps to a stable `error.moderation.*`
/// catalog code; the HTTP/gRPC surface renders status + code.
#[derive(Debug, Error)]
pub enum ModerationError {
    /// A federated request (pull or report) from a blocklisted peer server.
    #[error("federated request refused: peer server {server} is blocklisted")]
    ServerBlocked {
        /// The blocked peer.
        server: String,
    },
    /// A federated report whose signature did not verify against a known peer key — an
    /// unsigned, tampered, or unknown-peer report. Dropped before the admin queue
    /// (invariant 24).
    #[error("federated report refused: signature invalid or reporting peer unknown")]
    ReportUnsigned,
    /// A structurally malformed report (undecodable core, bad content-hash shape).
    #[error("federated report malformed: {0}")]
    ReportMalformed(String),
    /// The per-`(reporting_server, reported_user)` report budget is exhausted — backpressure
    /// rather than amplification (invariant 24).
    #[error("federated report rate limit exceeded for peer {server} against user {user}")]
    ReportRateLimited {
        /// The reporting peer.
        server: String,
        /// The reported user.
        user: String,
    },
    /// The asset a takedown/lift named does not exist on this server.
    #[error("asset not found: {asset_id}")]
    AssetNotFound {
        /// The missing asset id.
        asset_id: String,
    },
    /// A database failure.
    #[error(transparent)]
    Db(#[from] DbErr),
}

impl ModerationError {
    /// The stable `error.moderation.*` catalog code, when one applies. `None` for internal
    /// (`Db`) or not-found faults that render as a bare status.
    #[must_use]
    pub fn code(&self) -> Option<&'static str> {
        match self {
            ModerationError::ServerBlocked { .. } => Some(error_codes::MODERATION_SERVER_BLOCKED),
            ModerationError::ReportUnsigned | ModerationError::ReportMalformed(_) => {
                Some(error_codes::MODERATION_REPORT_UNSIGNED)
            }
            ModerationError::ReportRateLimited { .. } => {
                Some(error_codes::MODERATION_REPORT_RATE_LIMITED)
            }
            ModerationError::AssetNotFound { .. } | ModerationError::Db(_) => None,
        }
    }
}
