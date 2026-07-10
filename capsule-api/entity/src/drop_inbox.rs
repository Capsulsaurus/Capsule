//! A pending web-upload drop awaiting the owner's review (slice `S-C5`).
//!
//! A drop is written **only** here (never an album asset row): the guest's sealed ciphertext
//! is a content-addressed blob referenced by `ciphertext_hash`, with the unsigned
//! `DropDescriptor` carried opaquely in `descriptor`. Adoption (invariant 32) promotes the
//! blob to an `assets` row and deletes this row in one transaction; discard just deletes it.
//! Drops never appear on any album's sync feed (SSoT: the Web Upload design doc).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "drop_inbox")]
pub struct Model {
    /// The inbox row id (UUIDv7).
    #[sea_orm(primary_key, auto_increment = false)]
    pub drop_id: String,
    /// The provisioning (link-owner) user whose inbox this drop lands in.
    #[sea_orm(indexed)]
    pub owner_id: String,
    /// The upload link this drop arrived through (revocation handle).
    pub link_id: String,
    /// The content address of the staged drop blob (never an album asset).
    #[sea_orm(column_type = "String(StringLen::N(64))")]
    pub ciphertext_hash: String,
    /// Ciphertext size in bytes (the quota reservation freed on discard/adoption).
    pub size: i64,
    /// The guest-declared content type (closed enum for the link's protocol version).
    pub content_type: String,
    /// Guest-supplied, unverified; advisory only.
    #[sea_orm(nullable)]
    pub suggested_filename: Option<String>,
    /// The full unsigned `DropDescriptor` projection, carried opaquely.
    #[sea_orm(column_type = "JsonBinary")]
    pub descriptor: Json,
    /// Server-attested arrival instant (`received_at`).
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub received_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
