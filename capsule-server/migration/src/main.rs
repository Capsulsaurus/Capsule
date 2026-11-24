//! The operator's migration command.
//!
//! `capsule-server serve` deliberately does **not** run migrations: it reads `seaql_migrations`
//! and the checksum ledger this binary writes beside it, and refuses to start when the schema is
//! not the one it was built against (`capsule_server::postgres::assert_schema_current`). A
//! server that migrates on start runs the migration once per replica on a rolling deploy, which
//! is a schema change racing itself.
//!
//! Every migration `up` applies records its source checksum in the same transaction, and every
//! migration `down` rolls back deletes it (`server_migration::CHECKSUM_LEDGER`).

use std::process::ExitCode;

use clap::Parser as _;
use server_migration::cli::{Cli, run};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    match run(&cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "migration failed");
            ExitCode::FAILURE
        }
    }
}
