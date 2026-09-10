//! The Postgres adapters' shared half: the pool, the error mapping, the instant conversion, and
//! the container harness the conformance suites run against (#402).
//!
//! # Why the adapters are not in here
//!
//! Only the cross-cutting machinery lives in this module. Each adapter lives beside the port it
//! implements — `index/postgres.rs`, `auth/accounts_postgres.rs`, `store/cohorts_postgres.rs`,
//! `quota/postgres.rs`, `membership/postgres.rs` — because design/module-map.md assigns
//! contract ownership per *behaviour module* rather than per backend, and the tree already does it that way for the blob store
//! (`blob/fs.rs`, `blob/memory.rs`). A `postgres/` tree holding every adapter would make one
//! directory the co-owner of every durable contract in the crate.
//!
//! Nothing forces the alternative either: no invariant in these ports needs a transaction
//! spanning two of them. The sequence mint is inside `record_blob`, the attribution check is
//! inside `QuotaStore::charge`, and the account row is one table.
//!
//! # Migrations are applied by a separate binary, and `serve` refuses to run without them
//!
//! `capsule-server` takes `capsule-server-migration` as a **dev-dependency only**, so
//! `Migrator::up` is not reachable from the server binary at all — see that crate's manifest for
//! why (the `chrono` gate). What the server does instead is [`assert_schema_current`]: it reads
//! `seaql_migrations` and refuses to boot unless every ordinal it was built against has been
//! applied, naming the command an operator has to run.
//!
//! That is also the safer rollout. A server that migrates on start runs the migration once per
//! replica during a rolling deploy — a schema change racing itself — and gives every replica
//! the privileges a DDL statement needs.

pub mod error;
pub mod time;

#[cfg(test)]
pub(crate) mod testing;

use std::time::Duration;

use sea_orm::{ConnectOptions, Database, DatabaseConnection, DbBackend, Statement};

/// The migrations this binary was built against, oldest first.
///
/// Compiled in rather than read from the migration crate, which `capsule-server` cannot link.
/// `postgres::tests::the_expected_migrations_are_the_migrations_that_exist` compares the two
/// under `cfg(test)`, where the migration crate *is* available — so the list cannot drift from
/// the crate that defines it without a red test.
pub const EXPECTED_MIGRATIONS: &[&str] = &[
    "m20260902_000001_asset_index",
    "m20260902_000002_accounts",
    "m20260902_000003_cohorts",
    "m20260902_000004_quota",
    "m20260902_000005_album_membership",
];

/// The command an operator runs to apply them.
const MIGRATION_COMMAND: &str = "capsule-server-migration up";

/// Why a Postgres-backed process could not start.
///
/// A **startup** failure in both variants. Neither ever carries the connection URL: a
/// `DATABASE_URL` holds a password, and a startup error is the most-copied line in any incident
/// channel.
#[derive(Debug, thiserror::Error)]
pub enum PostgresError {
    /// The pool could not be opened.
    #[error("the Postgres connection could not be opened: {detail}")]
    Connect {
        /// The driver's own description. Never the URL.
        detail: String,
    },
    /// The schema is not the one this binary was built against.
    #[error("the database schema is not current: {detail}; run `{MIGRATION_COMMAND}`")]
    Schema {
        /// Which ordinals are missing, or why the ledger could not be read.
        detail: String,
    },
}

/// Open the pool `serve` and the operator workers share.
///
/// The numbers are a deployment default rather than a tuning surface, and each one is a
/// consequence of what this server does:
///
/// - `max_connections(32)` — the durable ports are on the request path but not the *byte* path;
///   a chunk append touches the blob store and Valkey, never Postgres. Thirty-two is comfortably
///   above the concurrency a finalization-bound workload reaches and well under a default
///   `max_connections = 100` shared with the migration binary and an operator's `psql`.
/// - `connect_timeout(5s)` / `acquire_timeout(10s)` — a request that waits longer than this has
///   already lost; failing it as `Unavailable` is what lets the route answer rather than hang.
/// - `idle_timeout(10m)` / `max_lifetime(30m)` — a connection that outlives a rolling restart of
///   the database is a connection that discovers it is dead inside somebody's transaction.
/// - `sqlx_logging(false)` — `tracing` owns the log line. sqlx's own would double every query
///   *and* print bound parameters, which here means an email address and an Argon2id PHC string.
///
/// # Errors
///
/// Returns [`PostgresError::Connect`] if the pool cannot be opened.
pub async fn connect(database_url: &str) -> Result<DatabaseConnection, PostgresError> {
    let mut options = ConnectOptions::new(database_url.to_owned());
    options
        .max_connections(32)
        .min_connections(2)
        .connect_timeout(Duration::from_secs(5))
        .acquire_timeout(Duration::from_secs(10))
        .idle_timeout(Duration::from_mins(10))
        .max_lifetime(Duration::from_mins(30))
        .sqlx_logging(false);

    let connection = Database::connect(options)
        .await
        .map_err(|error| PostgresError::Connect {
            detail: error.to_string(),
        })?;
    tracing::info!("opened the Postgres connection pool");
    Ok(connection)
}

/// Refuse to continue unless every migration in [`EXPECTED_MIGRATIONS`] has been applied.
///
/// Reads `seaql_migrations` — the ledger `sea-orm-migration` maintains — and compares its
/// applied names against the compiled-in list. A **missing** ordinal is fatal: the binary would
/// query a table or a column that does not exist, and the first symptom would be a 500 on
/// whichever request reached it first.
///
/// An ordinal the database holds and this binary does not know about is **not** fatal, and that
/// asymmetry is the rolling-deploy contract: during a deploy the migration runs first and the
/// old replicas keep serving, so a newer schema underneath an older binary is the normal state
/// for the length of the rollout. A migration that removes something an older binary reads is a
/// migration that has to be split in two, which is a property of the migration rather than
/// something this check can enforce.
///
/// # Errors
///
/// Returns [`PostgresError::Schema`] if the ledger cannot be read or an expected ordinal is
/// absent.
pub async fn assert_schema_current(connection: &DatabaseConnection) -> Result<(), PostgresError> {
    use sea_orm::ConnectionTrait as _;

    let statement = Statement::from_string(
        DbBackend::Postgres,
        "SELECT version FROM seaql_migrations".to_owned(),
    );
    let rows = connection
        .query_all(statement)
        .await
        .map_err(|error| PostgresError::Schema {
            detail: format!("`seaql_migrations` could not be read ({error})"),
        })?;

    let mut applied = Vec::with_capacity(rows.len());
    for row in rows {
        let version: String =
            row.try_get("", "version")
                .map_err(|error| PostgresError::Schema {
                    detail: format!("`seaql_migrations.version` could not be read ({error})"),
                })?;
        applied.push(version);
    }

    let missing: Vec<&str> = EXPECTED_MIGRATIONS
        .iter()
        .copied()
        .filter(|expected| !applied.iter().any(|held| held == expected))
        .collect();
    if !missing.is_empty() {
        return Err(PostgresError::Schema {
            detail: format!("{} has not been applied", missing.join(", ")),
        });
    }

    tracing::info!(
        applied = applied.len(),
        expected = EXPECTED_MIGRATIONS.len(),
        "the database schema is current"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use server_migration::MigratorTrait as _;

    use super::{EXPECTED_MIGRATIONS, assert_schema_current, testing};

    /// The compiled-in list is the migration crate's list.
    ///
    /// `capsule-server` cannot link the migrator in a normal build, so the list it refuses to
    /// boot on is a copy. This is the assertion that keeps the copy honest — and it needs no
    /// container, so it runs on every ordinary test pass.
    #[test]
    fn the_expected_migrations_are_the_migrations_that_exist() {
        let defined: Vec<String> = server_migration::Migrator::migrations()
            .iter()
            .map(|migration| migration.name().to_owned())
            .collect();
        assert_eq!(
            defined, EXPECTED_MIGRATIONS,
            "`postgres::EXPECTED_MIGRATIONS` is what `serve` refuses to boot without; it has \
             drifted from `capsule-server-migration`"
        );
    }

    mod postgres_conformance {
        use sea_orm::ConnectionTrait as _;
        use server_migration::MigratorTrait as _;

        use super::{EXPECTED_MIGRATIONS, assert_schema_current, testing};

        /// Up, down, up: the schema a rollback leaves behind is one the next deploy can build on.
        #[tokio::test]
        async fn the_migrations_apply_and_roll_back() {
            let Some(database) = testing::start("the migration round trip").await else {
                return;
            };
            let connection = database.connection();

            assert_schema_current(connection)
                .await
                .expect("a freshly migrated database is current");

            server_migration::Migrator::down(connection, None)
                .await
                .expect("every migration rolls back");
            let error = assert_schema_current(connection)
                .await
                .expect_err("a rolled-back schema is not current");
            assert!(
                format!("{error}").contains("capsule-server-migration up"),
                "the refusal must name the command that fixes it, got {error}"
            );

            server_migration::Migrator::up(connection, None)
                .await
                .expect("every migration re-applies");
            assert_schema_current(connection)
                .await
                .expect("the schema is current again");

            // And the ledger holds exactly the ordinals the server was built against — not an
            // extra one a partially-applied `down` left behind.
            let rows = connection
                .query_all(sea_orm::Statement::from_string(
                    sea_orm::DbBackend::Postgres,
                    "SELECT version FROM seaql_migrations ORDER BY version".to_owned(),
                ))
                .await
                .expect("the ledger is readable");
            let applied: Vec<String> = rows
                .iter()
                .map(|row| row.try_get("", "version").expect("a version column"))
                .collect();
            assert_eq!(applied, EXPECTED_MIGRATIONS);
        }
    }
}
