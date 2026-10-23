pub use sea_orm_migration::prelude::*;

mod m20250718_000000_initialize;
mod m20260710_000000_sync_store;
mod m20261023_000000_peering_state;
mod m20261024_000000_sync_retention;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20250718_000000_initialize::Migration),
            Box::new(m20260710_000000_sync_store::Migration),
            Box::new(m20261023_000000_peering_state::Migration),
            Box::new(m20261024_000000_sync_retention::Migration),
        ]
    }
}
