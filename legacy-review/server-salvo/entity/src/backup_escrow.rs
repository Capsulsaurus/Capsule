//! The master-key escrow store (slice `S-C12`). One row per user holding the
//! passphrase-wrapped account master key as an opaque blob.
//!
//! The server treats `blob` as opaque bytes it stores and serves verbatim — it never
//! interprets the wrap format (that lives entirely in `capsule_core::backup`). The row is
//! keyed by `user_id`, so a store-or-replace overwrites in place: there is exactly one
//! active escrow per user and the prior ciphertext is not retrievable after a replace.
//!
//! `user_id` is the account id (a nanoid, matching `users.id`) the escrow owner
//! authenticated as.

use chrono::{DateTime, Utc};
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "backup_escrow")]
pub struct Model {
    /// Account id (nanoid) the escrow belongs to — one active escrow per user.
    #[sea_orm(primary_key, column_type = "Char(Some(21))", auto_increment = false)]
    pub user_id: String,
    /// The passphrase-wrapped master key as an opaque blob, stored verbatim.
    #[sea_orm(column_type = "Blob")]
    pub blob: Vec<u8>,
    /// Instant the current escrow was stored.
    #[sea_orm(column_type = "TimestampWithTimeZone")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
