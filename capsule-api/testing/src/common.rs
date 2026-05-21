use sea_orm::{Database, DatabaseConnection};
use sea_orm_migration::MigratorTrait;
use std::{env, process::Command, str, sync::OnceLock, time::Duration};
use thiserror::Error;
use tokio::sync::Mutex;

static DB_URL: OnceLock<String> = OnceLock::new();
static CONTAINER_ID: OnceLock<String> = OnceLock::new();
static INIT_MUTEX: Mutex<()> = Mutex::const_new(());

/// Sets up a test postgres database. If TEST_DATABASE_URL is set, it will be used.
/// Otherwise this will start a postgres docker container (requires docker CLI) and
/// run migrations against it. The container is left running for the duration of the test process.
pub async fn setup_test_db() -> Result<DatabaseConnection, TestDbError> {
    dotenvy::dotenv().ok();

    // If the user provided TEST_DATABASE_URL, prefer it (useful for CI or local debugging)
    if let Ok(url) = env::var("TEST_DATABASE_URL") {
        let db = Database::connect(&url).await?;
        migration::Migrator::up(&db, None).await?;
        return Ok(db);
    }

    // Fast path: already initialized
    if let Some(url) = DB_URL.get() {
        let db = Database::connect(url).await?;
        return Ok(db);
    }

    // Serialize container startup to prevent multiple containers being spawned when
    // async tests run concurrently.
    let _guard = INIT_MUTEX.lock().await;

    // Double-check under lock
    if let Some(url) = DB_URL.get() {
        let db = Database::connect(url).await?;
        return Ok(db);
    }

    // Start a postgres container with docker CLI.
    if Command::new("docker").arg("--version").output().is_err() {
        return Err(TestDbError::Docker("docker CLI not found in PATH or not runnable. Install Docker or set TEST_DATABASE_URL to run tests.".to_string()));
    }

    let output = Command::new("docker")
        .args([
            "run",
            "-d",
            "-e",
            "POSTGRES_PASSWORD=postgres",
            "-e",
            "POSTGRES_DB=postgres",
            "-p",
            "5432",
            "postgres:15-alpine",
        ])
        .output()?;

    if !output.status.success() {
        return Err(TestDbError::Docker(format!(
            "failed to start docker postgres container: {}",
            str::from_utf8(&output.stderr).unwrap_or("[INVALID UTF-8]")
        )));
    }

    let container_id = str::from_utf8(&output.stdout)
        .map_err(|_| TestDbError::Docker("Failed to parse container ID".to_string()))?
        .trim()
        .to_string();

    // Inspect to find the mapped host port
    let inspect = Command::new("docker")
        .args([
            "inspect",
            "-f",
            "{{ (index (index .NetworkSettings.Ports \"5432/tcp\") 0).HostPort }}",
            &container_id,
        ])
        .output()?;
    if !inspect.status.success() {
        return Err(TestDbError::Docker(format!(
            "failed to inspect docker container: {}",
            str::from_utf8(&inspect.stderr).unwrap_or("[INVALID UTF-8]")
        )));
    }

    let port = str::from_utf8(&inspect.stdout)
        .map_err(|_| TestDbError::Docker("Failed to parse port".to_string()))?
        .trim()
        .to_string();
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/postgres", port);

    // Wait for Postgres to accept connections
    let mut attempts = 0u8;
    let db = loop {
        match Database::connect(&url).await {
            Ok(db) => break db,
            Err(_) if attempts < 20 => {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            Err(e) => return Err(e.into()),
        }
    };

    // Run migrations once
    migration::Migrator::up(&db, None).await?;

    DB_URL.set(url.clone()).ok();
    CONTAINER_ID.set(container_id).ok();

    Ok(db)
}

#[derive(Error, Debug)]
pub enum TestDbError {
    #[error("Database error: {0}")]
    Database(#[from] sea_orm::DbErr),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Docker error: {0}")]
    Docker(String),
}
