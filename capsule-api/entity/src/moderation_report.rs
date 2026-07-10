//! The federated-report admin queue (slice `S-C8`).
//!
//! One row per **accepted** federated moderation report — signed by a known peer and within
//! that peer's report rate budget (invariant 24). A report carries the alleged asset's
//! **content hash and album pointer only, never plaintext or decryption material**: an admin
//! who already holds album access can locate and view the asset to act; an admin without it
//! sees only opaque identifiers, exactly as the E2EE model requires. SSoT:
//! [Moderation — Federated Reporting](https://docs/design/moderation/#federated-reporting).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "moderation_reports")]
pub struct Model {
    /// UUIDv7 report id (time-ordered).
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: String,
    /// The peer server that submitted the report (attributable — a false reporter is
    /// identifiable and blockable).
    #[sea_orm(indexed)]
    pub reporting_server: String,
    /// The reported account on this (home) server.
    #[sea_orm(indexed)]
    pub reported_user: String,
    /// The alleged asset's content hash (64-char lowercase hex) — never plaintext.
    #[sea_orm(column_type = "String(StringLen::N(64))")]
    pub content_hash: String,
    /// An album pointer locating the asset for an admin with album access.
    pub album_pointer: Option<String>,
    /// Free-form admin-facing reason, when the peer supplied one.
    pub reason: Option<String>,
    /// When the report was accepted into the queue.
    #[sea_orm(column_type = "TimestampWithTimeZone")]
    pub received_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
