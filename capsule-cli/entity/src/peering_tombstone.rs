//! An asset this device holds a tombstone for, kept past purge so a peer can never resurrect it
//! ([Peering § Sync]). The store refuses to update or delete a row (migration
//! `m20261023_000000_peering_state`), so a tombstone is permanent.
//!
//! [Peering § Sync]: https://docs/design/peering/#sync

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "peering_tombstones")]
pub struct Model {
    /// The tombstoned asset (a hyphenated UUID).
    #[sea_orm(primary_key, auto_increment = false)]
    pub asset_id: String,
    /// When this device recorded the tombstone (RFC 3339).
    pub recorded_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
