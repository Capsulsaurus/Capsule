//! A provisioned web-upload link (slice `S-C5`).
//!
//! The server-held half of an upload link (the [Web Upload design doc]'s Provision step).
//! `opaque_id` is the random ≥128-bit URL-path token resolved on every drop-session
//! creation; `owner_id` is the provisioning user whose quota a drop debits (invariant 29).
//! The optional per-link caps plus the running `bytes_used`/`files_used` counters bound a
//! leaked link to wasted quota and inbox space (invariant 26); `passphrase_verifier` carries
//! the optional Argon2id abuse-gate verifier; `revoked_at` is the fail-closed revocation flag.
//!
//! [Web Upload design doc]: ../../../../capsule-docs/src/content/docs/design/web-upload.md

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "upload_links")]
pub struct Model {
    /// Owner-held revocation handle (never URL-exposed).
    #[sea_orm(primary_key, auto_increment = false)]
    pub link_id: String,
    /// The random ≥128-bit opaque URL-path token (hex of 16 bytes = 32 chars); the lookup key.
    #[sea_orm(unique, column_type = "String(StringLen::N(32))")]
    pub opaque_id: String,
    /// The provisioning user whose quota a drop through this link debits.
    pub owner_id: String,
    /// Optional destination-album hint (advisory; adoption chooses the album).
    #[sea_orm(nullable)]
    pub album_hint: Option<String>,
    /// Pinned wire protocol version (`YYYY-MM-DD`).
    #[sea_orm(column_type = "String(StringLen::N(10))")]
    pub protocol_version: String,
    /// Pinned crypto suite id (from the primitives inventory).
    pub crypto_suite_id: i32,
    /// Cap: RFC 3339 expiry; `NULL` = no expiry (revocation still applies).
    #[sea_orm(column_type = "TimestampWithTimeZone", nullable)]
    pub expires_at: Option<DateTime<Utc>>,
    /// Cap: cumulative byte cap across all drops on this link.
    #[sea_orm(nullable)]
    pub max_total_bytes: Option<i64>,
    /// Cap: maximum number of files this link may deposit.
    #[sea_orm(nullable)]
    pub max_file_count: Option<i32>,
    /// Cap: maximum single-file (ciphertext) size.
    #[sea_orm(nullable)]
    pub max_file_size: Option<i64>,
    /// Cap: whether the link dies after its first successful drop.
    #[sea_orm(default_value = "false")]
    pub single_use: bool,
    /// Optional Argon2id abuse-gate verifier (JSON of the S-A6 `PassphraseVerifier`).
    #[sea_orm(column_type = "JsonBinary", nullable)]
    pub passphrase_verifier: Option<Json>,
    /// Fail-closed revocation instant; `NULL` = live.
    #[sea_orm(column_type = "TimestampWithTimeZone", nullable)]
    pub revoked_at: Option<DateTime<Utc>>,
    /// Running cumulative bytes deposited (against `max_total_bytes`).
    pub bytes_used: i64,
    /// Running cumulative file count (against `max_file_count`).
    pub files_used: i32,
    /// Row creation instant.
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub created_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
