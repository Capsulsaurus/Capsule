//! Peering's anti-rollback floor: per device, the highest account device-directory version it has
//! verified ([Peering § Trust Model]). The store refuses to lower or delete a row (migration
//! `m20261023_000000_peering_state`), so the floor only ever rises.
//!
//! [Peering § Trust Model]: https://docs/design/peering/#trust-model

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "peering_floor")]
pub struct Model {
    /// The device the floor is held for (a hyphenated UUID).
    #[sea_orm(primary_key, auto_increment = false)]
    pub device_id: String,
    /// The highest `directory_version` this device has verified; never negative.
    pub directory_version: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
