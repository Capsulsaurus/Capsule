use std::fmt;

use colored::Colorize;
use migration::{Migrator, MigratorTrait};
use sea_orm::{Database, DatabaseConnection, DbErr};
use thiserror::Error;
use tracing::debug;

use crate::utils::directories::get_sqlite_db_path;

#[derive(Debug, Error)]
pub enum InitDbError {
    Path(String),
    Db(#[from] DbErr),
    Io(#[from] std::io::Error),
}

impl fmt::Display for InitDbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InitDbError::Path(msg) => write!(f, "Path error: {msg}"),
            InitDbError::Db(err) => write!(f, "Database error: {err}"),
            InitDbError::Io(err) => write!(f, "IO error: {err}"),
        }
    }
}

pub async fn init_sqlite() -> Result<DatabaseConnection, InitDbError> {
    let db_path = get_sqlite_db_path().ok_or(InitDbError::Path(
        "Failed to get SQLite DB path".to_string(),
    ))?;
    {
        // Create the database directory if it doesn't exist
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let db_url = format!("sqlite://{}?mode=rwc", db_path.to_string_lossy());
    debug!(
        "{}",
        format!("Initializing Sqlite database connection to: {db_url}").blue()
    );
    let db = Database::connect(&db_url).await?;
    // db.close().await?;
    debug!("Connected to SQLite database at: {db_url}");
    Migrator::up(&db, None).await?;
    debug!("Database migration completed");

    Ok(db)
}
