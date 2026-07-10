//! The server-level blocklist (slice `S-C8`).
//!
//! One row per peer server this server refuses federated requests from. Enforced at the
//! [federation-capability](https://docs/design/federation/#federation-capabilities) layer: a
//! blocked peer cannot pull, and cannot submit a federated report. A server-level block is a
//! manual admin action, deliberately distinct from a per-user block (which never propagates
//! as a server-wide block). SSoT:
//! [Moderation — Blocklists](https://docs/design/moderation/#blocklists).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "server_blocklist")]
pub struct Model {
    /// The blocked peer's canonical origin.
    #[sea_orm(primary_key, auto_increment = false)]
    pub server_id: String,
    /// Free-form admin note.
    pub reason: Option<String>,
    /// When the block was applied.
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub blocked_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
