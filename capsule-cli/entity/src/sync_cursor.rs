//! The single opaque sync cursor persisted for the sync-fed local store (slice
//! `S-D5`). The cursor is server-MAC'd and never interpreted by the client; it is
//! round-tripped verbatim, so it is stored as raw bytes in a one-row table.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "sync_cursor")]
pub struct Model {
    /// Singleton row id — always `0`; there is exactly one feed cursor.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i32,
    /// The opaque, server-MAC'd cursor bytes (empty for the first-sync sentinel).
    #[sea_orm(column_type = "Blob")]
    pub cursor: Vec<u8>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
