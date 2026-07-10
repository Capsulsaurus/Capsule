pub use sea_orm_migration::prelude::*;

mod m20250210_000000_initial_schema;
mod m20250302_000000_add_registered_via;
mod m20260322_000000_change_file_hash_to_sha256;
mod m20260710_000000_sync_feed;
mod m20260710_000001_device_directory;
mod m20260710_000002_quota;
mod m20260710_000003_blob_gc;
mod m20260710_000004_drops;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20250210_000000_initial_schema::Migration),
            Box::new(m20250302_000000_add_registered_via::Migration),
            Box::new(m20260322_000000_change_file_hash_to_sha256::Migration),
            Box::new(m20260710_000000_sync_feed::Migration),
            Box::new(m20260710_000001_device_directory::Migration),
            Box::new(m20260710_000002_quota::Migration),
            Box::new(m20260710_000003_blob_gc::Migration),
            Box::new(m20260710_000004_drops::Migration),
        ]
    }
}
