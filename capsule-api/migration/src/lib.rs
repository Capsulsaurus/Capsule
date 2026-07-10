pub use sea_orm_migration::prelude::*;

mod m20250210_000000_initial_schema;
mod m20250302_000000_add_registered_via;
mod m20260322_000000_change_file_hash_to_sha256;
mod m20260710_000000_sync_feed;
mod m20260710_000001_device_directory;
mod m20260710_000002_quota;
mod m20260710_000003_blob_gc;
mod m20260710_000004_drops;
mod m20260710_000005_custody_receipts;
mod m20260710_000006_lifecycle_ops;
mod m20260710_000007_device_cohorts;
mod m20260710_000008_backup_escrow;
mod m20260710_000009_public_shares;
mod m20260710_000010_moderation;
mod m20260710_000011_federation;

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
            Box::new(m20260710_000005_custody_receipts::Migration),
            Box::new(m20260710_000006_lifecycle_ops::Migration),
            Box::new(m20260710_000007_device_cohorts::Migration),
            Box::new(m20260710_000008_backup_escrow::Migration),
            Box::new(m20260710_000009_public_shares::Migration),
            Box::new(m20260710_000010_moderation::Migration),
            Box::new(m20260710_000011_federation::Migration),
        ]
    }
}
