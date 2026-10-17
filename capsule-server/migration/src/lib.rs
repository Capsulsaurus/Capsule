//! The server's PostgreSQL schema, one migration per ordinal.
//!
//! # What the six ordinals cover
//!
//! The four durable ports issue #402 lands adapters for — the asset index, the account
//! cluster, the device-cohort map and the quota ledger — plus album membership (`S-C51`, #405)
//! and federation's capabilities, revocation list and peers (`S-E2`, #406).
//! The remaining durable ports keep their
//! in-memory adapters and gain ordinals with their adapters, so a migration never describes a
//! table nothing reads.
//!
//! # Every instant is a `BIGINT` of microseconds since the Unix epoch
//!
//! Not `TIMESTAMPTZ`. Binding one needs sea-orm's `with-chrono` or `with-time`, and both are
//! refused: `chrono` is banned outside `capsule-cli/entity` by a gate
//! (design/dependencies.md), and `time` would be a third datetime crate with no row in that
//! table. `capsule_server::postgres::time` converts at the adapter boundary, exactly as the
//! CLI converts at its entity boundary. What that costs is SQL date arithmetic; every expiry
//! and retention comparison in these tables is an ordering on one column, and integers order
//! identically.

pub use sea_orm_migration::prelude::*;

mod m20260902_000001_asset_index;
mod m20260902_000002_accounts;
mod m20260902_000003_cohorts;
mod m20260902_000004_quota;
mod m20260902_000005_album_membership;
mod m20260902_000006_federation;
mod m20260902_000007_albums;
mod m20260902_000008_device_directories;
mod m20260902_000009_moderation;
mod m20260902_000010_shares;
mod m20260902_000011_drops;
mod m20260902_000012_escrow;
mod m20260902_000013_collection_marks;
mod m20260902_000014_receipts;
mod m20260902_000015_totp;

/// The server's migrator.
pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20260902_000001_asset_index::Migration),
            Box::new(m20260902_000002_accounts::Migration),
            Box::new(m20260902_000003_cohorts::Migration),
            Box::new(m20260902_000004_quota::Migration),
            Box::new(m20260902_000005_album_membership::Migration),
            Box::new(m20260902_000006_federation::Migration),
            Box::new(m20260902_000007_albums::Migration),
            Box::new(m20260902_000008_device_directories::Migration),
            Box::new(m20260902_000009_moderation::Migration),
            Box::new(m20260902_000010_shares::Migration),
            Box::new(m20260902_000011_drops::Migration),
            Box::new(m20260902_000012_escrow::Migration),
            Box::new(m20260902_000013_collection_marks::Migration),
            Box::new(m20260902_000014_receipts::Migration),
            Box::new(m20260902_000015_totp::Migration),
        ]
    }
}
