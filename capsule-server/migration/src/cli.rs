//! The operator's command line: `capsule-server-migration [up | down | status]`.
//!
//! The verbs and flags are the ones `sea-orm-migration`'s command line gave this binary, so the
//! documented `capsule-server-migration up` and a `DATABASE_URL` in the environment keep working.
//! Its destructive verbs (`fresh`, `refresh`, `reset`) and its code generators are not carried
//! over: a production schema is only ever moved forward, or back one reviewed step at a time.

use clap::{Parser, Subcommand};
use sea_orm::{Database, DbErr};

use crate::{Migrator, MigratorTrait as _};

/// Apply or roll back the server's schema migrations.
#[derive(Debug, Parser)]
#[command(name = "capsule-server-migration")]
pub struct Cli {
    /// The PostgreSQL database to migrate.
    #[arg(short = 'u', long, env = "DATABASE_URL", hide_env_values = true)]
    pub database_url: String,

    /// What to do; `up` when omitted.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// One migration operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Apply the pending migrations: all of them, or the first `-n`.
    Up {
        /// How many to apply.
        #[arg(short = 'n', long)]
        num: Option<u32>,
    },
    /// Roll back the newest applied migrations: one, or `-n`.
    Down {
        /// How many to roll back.
        #[arg(short = 'n', long, default_value_t = 1)]
        num: u32,
    },
    /// List every migration and whether it is applied.
    Status,
}

impl Cli {
    /// The command to run, `up` with no limit when none was given.
    #[must_use]
    pub fn operation(&self) -> Command {
        self.command.unwrap_or(Command::Up { num: None })
    }
}

/// Connect to the database and run the command.
///
/// # Errors
///
/// The connection, or the migration operation, failing.
pub async fn run(cli: &Cli) -> Result<(), DbErr> {
    let db = Database::connect(cli.database_url.as_str()).await?;
    match cli.operation() {
        Command::Up { num } => Migrator::up(&db, num).await,
        Command::Down { num } => Migrator::down(&db, Some(num)).await,
        Command::Status => {
            for (name, status) in Migrator::status(&db).await? {
                println!("{name}\t{status}");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
