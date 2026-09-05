//! The server's PostgreSQL schema, one migration per ordinal.
//!
//! # What the four ordinals cover
//!
//! The durable ports issue #402 lands adapters for, and no others: the asset index, the account
//! cluster, the device-cohort map and the quota ledger. The remaining durable ports keep their
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
        ]
    }
}
