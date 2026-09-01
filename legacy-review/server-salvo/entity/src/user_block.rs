//! The per-user block ledger (slice `S-C8`).
//!
//! One row per `(blocker_id, blocked_id)` — a user blocking another user. Enforced by the
//! blocker's home server: the blocked user is removed from albums shared with the blocker and
//! cannot share new albums with them. A per-user block is **scoped to that user**: it does
//! **not** propagate as a server-wide federation block, so one user (or a coordinated group)
//! cannot weaponize blocks to sever a peer from the federation. SSoT:
//! [Moderation — Blocklists](https://docs/design/moderation/#blocklists).

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "user_blocks")]
pub struct Model {
    /// The user who placed the block.
    #[sea_orm(primary_key, auto_increment = false)]
    pub blocker_id: String,
    /// The blocked user.
    #[sea_orm(primary_key, auto_increment = false)]
    pub blocked_id: String,
    /// When the block was placed.
    #[sea_orm(
        column_type = "TimestampWithTimeZone",
        default_value = "CURRENT_TIMESTAMP"
    )]
    pub created_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
