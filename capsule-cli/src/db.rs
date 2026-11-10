//! The CLI's local database, `capsule.sqlite`: the sync store ([`crate::syncstore`]) and
//! peering's durable state ([`crate::peering_state`]).
//!
//! One SQLite stack serves the whole client side: this is `rusqlite`, the same bundled driver
//! `capsule-library` keeps its catalog on. The file opens in WAL mode with a busy timeout, so a
//! second `capsule` process (a TUI beside a `capsule sync`) waits for a writer rather than
//! failing, and [`migrate`] brings the schema to the current version on every open.
//!
//! [`LocalDb`] is the handle every query takes. A `rusqlite::Connection` is `Send` but not
//! `Sync`, so the handle owns it behind a mutex and runs each unit of work on tokio's blocking
//! pool ([`LocalDb::call`]): the async callers never block a runtime worker on file I/O, and a
//! closure that opens a transaction holds the connection for exactly that transaction.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rusqlite::Connection;
use thiserror::Error;
use tracing::instrument;

use crate::utils::directories::get_sqlite_db_path;

pub mod migrate;

pub use migrate::{MigrationError, Outcome, SCHEMA_VERSION};

/// How long a statement waits on another connection's lock before it fails with `SQLITE_BUSY`.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the local database could not be opened.
#[derive(Debug, Error)]
pub enum OpenError {
    /// No data directory could be resolved for the database file.
    #[error("Path error: the local database path could not be resolved")]
    Path,
    /// The database's directory could not be created.
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    /// SQLite refused to open or configure the file.
    #[error("Database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// The schema could not be brought to the current version; nothing was lost.
    #[error("Database error: {0}")]
    Migration(#[from] MigrationError),
    /// The blocking task that opened the database did not finish.
    #[error("Database error: the open task failed: {0}")]
    Task(String),
}

/// Why a unit of work on an open database failed.
#[derive(Debug, Error)]
pub enum DbError {
    /// SQLite refused a statement.
    #[error("{0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A previous unit of work panicked while it held the connection. Any transaction it had
    /// open was rolled back when it unwound; the handle is refused rather than trusted after it.
    #[error("the local database connection is poisoned by an earlier panic")]
    Poisoned,
    /// The blocking task running the unit of work did not finish.
    #[error("the local database task failed: {0}")]
    Task(String),
}

/// A shared handle on one open, migrated connection to the CLI database. Cloning shares the
/// connection.
#[derive(Debug, Clone)]
pub struct LocalDb {
    conn: Arc<Mutex<Connection>>,
}

impl LocalDb {
    /// Open (creating it if absent) the database at `path`, and migrate it to
    /// [`SCHEMA_VERSION`]. Blocking: async callers use [`open_async`](Self::open_async).
    ///
    /// # Errors
    ///
    /// [`OpenError::Sqlite`] when the file cannot be opened or configured,
    /// [`OpenError::Migration`] when its schema is refused or a step fails.
    #[instrument(level = "debug", skip_all, fields(path = %path.display()), err)]
    pub fn open(path: &Path) -> Result<Self, OpenError> {
        let mut conn = Connection::open(path)?;
        configure(&conn)?;
        let outcome = migrate::migrate(&mut conn)?;
        tracing::debug!(?outcome, "local database: open and migrated");
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// [`open`](Self::open) on the blocking pool.
    ///
    /// # Errors
    ///
    /// The errors of [`open`](Self::open), or [`OpenError::Task`] when the task does not finish.
    pub async fn open_async(path: PathBuf) -> Result<Self, OpenError> {
        tokio::task::spawn_blocking(move || Self::open(&path))
            .await
            .map_err(|error| OpenError::Task(error.to_string()))?
    }

    /// Run `work` against the connection on the blocking pool, holding it for the duration.
    ///
    /// # Errors
    ///
    /// [`DbError::Sqlite`] for what `work` returns, [`DbError::Poisoned`] after an earlier
    /// panic, [`DbError::Task`] when the task does not finish.
    pub async fn call<T, F>(&self, work: F) -> Result<T, DbError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    {
        let conn = Arc::clone(&self.conn);
        tokio::task::spawn_blocking(move || {
            let mut guard = conn.lock().map_err(|_| DbError::Poisoned)?;
            work(&mut guard).map_err(DbError::from)
        })
        .await
        .map_err(|error| DbError::Task(error.to_string()))?
    }
}

/// WAL, so readers never block the writer, and a busy timeout, so two processes wait their turn.
fn configure(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(BUSY_TIMEOUT)?;
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    tracing::debug!(journal_mode = %mode, busy_timeout_ms = BUSY_TIMEOUT.as_millis(), "local database: configured");
    Ok(())
}

/// Open the CLI database at its default path (`capsule.sqlite` in the data directory), creating
/// the directory and the file as needed.
///
/// # Errors
///
/// [`OpenError::Path`] when there is no data directory, [`OpenError::Io`] when it cannot be
/// created, or the errors of [`LocalDb::open`].
pub(crate) async fn init_sqlite() -> Result<LocalDb, OpenError> {
    let db_path = get_sqlite_db_path().ok_or(OpenError::Path)?;
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    tracing::debug!(path = %db_path.display(), "local database: opening");
    LocalDb::open_async(db_path).await
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests;
