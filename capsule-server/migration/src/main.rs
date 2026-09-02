//! The operator's migration command.
//!
//! `capsule-server serve` deliberately does **not** run migrations: it reads `seaql_migrations`
//! and refuses to start when the schema is not the one it was built against
//! (`capsule_server::postgres::assert_schema_current`). A server that migrates on start runs
//! the migration once per replica on a rolling deploy, which is a schema change racing itself.

use sea_orm_migration::prelude::*;

#[tokio::main]
async fn main() {
    cli::run_cli(server_migration::Migrator).await;
}
