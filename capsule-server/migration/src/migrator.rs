//! The migration runner: applies and rolls back the server's migrations, recording each one in
//! `seaql_migrations`.
//!
//! # Why the server runs its own
//!
//! This replaced `sea-orm-migration`, whose `sea-orm` dependency keeps sea-orm's default
//! features on, `with-chrono` among them: as long as it was in the graph, chrono was in the lock
//! file. The runner keeps that crate's contract with a database, because existing databases
//! were migrated by it:
//!
//! - the ledger is the same `seaql_migrations` table, `version` (the migration's name) as its
//!   primary key and `applied_at` in seconds since the Unix epoch, created if it is missing;
//! - a migration is applied, or rolled back, in a transaction of its own that also writes, or
//!   deletes, its ledger row, so a failure leaves the ledger naming exactly what was applied;
//! - a recorded version no migration defines is a refusal, never something to skip: that
//!   database was migrated by a newer build, or by a build with a migration this one lost.
//!
//! The migrations themselves keep their `use sea_orm_migration::prelude::*`: the crate root
//! names itself `sea_orm_migration`, and an applied migration's source is never edited (its
//! checksum is in the ledger; see the crate docs).

use sea_orm::sea_query::{
    Alias, ColumnDef, IndexCreateStatement, IndexDropStatement, Order, Query, Table,
    TableAlterStatement, TableCreateStatement, TableDropStatement,
};
use sea_orm::{
    ConnectionTrait, DatabaseExecutor, DbBackend, DbErr, IntoDatabaseExecutor, Statement,
    StatementBuilder, TransactionTrait,
};
use tracing::{debug, info, instrument};

/// The ledger of applied migrations, under the name `sea-orm-migration` gave it.
pub const MIGRATION_TABLE: &str = "seaql_migrations";

/// A migration's name: the stem of the file that defines it, which is also its ledger version.
pub trait MigrationName {
    /// The name recorded in [`MIGRATION_TABLE`] once the migration is applied.
    fn name(&self) -> &str;
}

/// One schema change and its inverse.
#[async_trait::async_trait]
pub trait MigrationTrait: MigrationName + Send + Sync {
    /// Apply the change.
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr>;

    /// Undo [`up`](Self::up). A migration that does not define it cannot be rolled back.
    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(format!(
            "migration {} cannot be rolled back",
            self.name()
        )))
    }
}

/// What a migration runs its statements against: the transaction the runner opened for it.
pub struct SchemaManager<'c> {
    connection: DatabaseExecutor<'c>,
}

impl<'c> SchemaManager<'c> {
    /// A manager over `connection`.
    pub fn new(connection: impl IntoDatabaseExecutor<'c>) -> Self {
        Self {
            connection: connection.into_database_executor(),
        }
    }

    /// The connection, for a statement the builder methods below do not cover.
    pub fn get_connection(&self) -> &DatabaseExecutor<'c> {
        &self.connection
    }

    /// The connection's backend.
    pub fn get_database_backend(&self) -> DbBackend {
        self.connection.get_database_backend()
    }

    async fn execute(&self, statement: &impl StatementBuilder) -> Result<(), DbErr> {
        self.connection.execute(statement).await.map(|_| ())
    }

    /// Run a `CREATE TABLE`.
    pub async fn create_table(&self, statement: TableCreateStatement) -> Result<(), DbErr> {
        self.execute(&statement).await
    }

    /// Run an `ALTER TABLE`.
    pub async fn alter_table(&self, statement: TableAlterStatement) -> Result<(), DbErr> {
        self.execute(&statement).await
    }

    /// Run a `DROP TABLE`.
    pub async fn drop_table(&self, statement: TableDropStatement) -> Result<(), DbErr> {
        self.execute(&statement).await
    }

    /// Run a `CREATE INDEX`.
    pub async fn create_index(&self, statement: IndexCreateStatement) -> Result<(), DbErr> {
        self.execute(&statement).await
    }

    /// Run a `DROP INDEX`.
    pub async fn drop_index(&self, statement: IndexDropStatement) -> Result<(), DbErr> {
        self.execute(&statement).await
    }
}

/// Whether a migration is recorded in [`MIGRATION_TABLE`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationStatus {
    /// Not recorded: `up` applies it.
    Pending,
    /// Recorded: `down` may roll it back.
    Applied,
}

impl std::fmt::Display for MigrationStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Pending => "pending",
            Self::Applied => "applied",
        })
    }
}

/// The status of each of `defined` (oldest first), given the versions the ledger `recorded`.
///
/// # Errors
///
/// A recorded version that no migration defines, which is a refusal: this build does not know
/// the schema that database is in.
pub fn statuses(defined: &[&str], recorded: &[String]) -> Result<Vec<MigrationStatus>, DbErr> {
    let unknown: Vec<&str> = recorded
        .iter()
        .map(String::as_str)
        .filter(|version| !defined.contains(version))
        .collect();
    if !unknown.is_empty() {
        return Err(DbErr::Migration(format!(
            "the database records migrations this build does not define: {}",
            unknown.join(", ")
        )));
    }
    Ok(defined
        .iter()
        .map(|name| {
            if recorded.iter().any(|version| version == name) {
                MigrationStatus::Applied
            } else {
                MigrationStatus::Pending
            }
        })
        .collect())
}

/// The indices `up` applies, in order: the pending migrations, at most `steps` of them.
#[must_use]
pub fn to_apply(statuses: &[MigrationStatus], steps: Option<u32>) -> Vec<usize> {
    select(statuses.iter().enumerate(), MigrationStatus::Pending, steps)
}

/// The indices `down` rolls back, in order: the applied migrations newest first, at most `steps`
/// of them.
#[must_use]
pub fn to_roll_back(statuses: &[MigrationStatus], steps: Option<u32>) -> Vec<usize> {
    select(
        statuses.iter().enumerate().rev(),
        MigrationStatus::Applied,
        steps,
    )
}

fn select<'a>(
    statuses: impl Iterator<Item = (usize, &'a MigrationStatus)>,
    wanted: MigrationStatus,
    steps: Option<u32>,
) -> Vec<usize> {
    let limit = steps.map_or(usize::MAX, |steps| {
        usize::try_from(steps).unwrap_or(usize::MAX)
    });
    statuses
        .filter(|(_, status)| **status == wanted)
        .map(|(index, _)| index)
        .take(limit)
        .collect()
}

/// A set of migrations and the operations over a database that run them.
#[async_trait::async_trait]
pub trait MigratorTrait: Send {
    /// Every migration, oldest first.
    fn migrations() -> Vec<Box<dyn MigrationTrait>>;

    /// Each migration's name and status.
    ///
    /// # Errors
    ///
    /// A database error, or a recorded version no migration defines.
    async fn status<'c, C>(db: C) -> Result<Vec<(String, MigrationStatus)>, DbErr>
    where
        C: IntoDatabaseExecutor<'c>,
    {
        let db = db.into_database_executor();
        let migrations = Self::migrations();
        let statuses = load_statuses(&db, &migrations).await?;
        Ok(migrations
            .iter()
            .map(|migration| migration.name().to_owned())
            .zip(statuses)
            .collect())
    }

    /// Apply the pending migrations, oldest first; all of them, or the first `steps`.
    ///
    /// # Errors
    ///
    /// A failed migration (its transaction is rolled back, and the ones before it stay applied),
    /// or a recorded version no migration defines.
    async fn up<'c, C>(db: C, steps: Option<u32>) -> Result<(), DbErr>
    where
        C: IntoDatabaseExecutor<'c>,
    {
        let db = db.into_database_executor();
        let migrations = Self::migrations();
        let statuses = load_statuses(&db, &migrations).await?;
        let indices = to_apply(&statuses, steps);
        info!(pending = indices.len(), ?steps, "applying migrations");
        for index in indices {
            apply(&db, migrations[index].as_ref(), Direction::Up).await?;
        }
        Ok(())
    }

    /// Roll back the applied migrations, newest first; all of them, or the last `steps`.
    ///
    /// # Errors
    ///
    /// A failed rollback (its transaction is rolled back, and the ones after it stay rolled
    /// back), or a recorded version no migration defines.
    async fn down<'c, C>(db: C, steps: Option<u32>) -> Result<(), DbErr>
    where
        C: IntoDatabaseExecutor<'c>,
    {
        let db = db.into_database_executor();
        let migrations = Self::migrations();
        let statuses = load_statuses(&db, &migrations).await?;
        let indices = to_roll_back(&statuses, steps);
        info!(applied = indices.len(), ?steps, "rolling back migrations");
        for index in indices {
            apply(&db, migrations[index].as_ref(), Direction::Down).await?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug)]
enum Direction {
    Up,
    Down,
}

/// Create the ledger if it is missing, read it, and pair it with `migrations`.
async fn load_statuses(
    db: &DatabaseExecutor<'_>,
    migrations: &[Box<dyn MigrationTrait>],
) -> Result<Vec<MigrationStatus>, DbErr> {
    let mut create = Table::create();
    create
        .table(Alias::new(MIGRATION_TABLE))
        .if_not_exists()
        .col(
            ColumnDef::new(Alias::new("version"))
                .string()
                .not_null()
                .primary_key(),
        )
        .col(
            ColumnDef::new(Alias::new("applied_at"))
                .big_integer()
                .not_null(),
        );
    db.execute(&create).await?;

    let select = Query::select()
        .column(Alias::new("version"))
        .from(Alias::new(MIGRATION_TABLE))
        .order_by(Alias::new("version"), Order::Asc)
        .to_owned();
    let recorded = db
        .query_all(&select)
        .await?
        .into_iter()
        .map(|row| row.try_get::<String>("", "version"))
        .collect::<Result<Vec<_>, _>>()?;
    debug!(recorded = recorded.len(), "read the migration ledger");

    let names: Vec<&str> = migrations
        .iter()
        .map(|migration| migration.name())
        .collect();
    statuses(&names, &recorded)
}

/// Run one migration in a transaction of its own, with its ledger row.
#[instrument(skip_all, fields(migration = migration.name(), ?direction))]
async fn apply(
    db: &DatabaseExecutor<'_>,
    migration: &dyn MigrationTrait,
    direction: Direction,
) -> Result<(), DbErr> {
    let transaction = db.begin().await?;
    let backend = transaction.get_database_backend();
    {
        let manager = SchemaManager::new(&transaction);
        match direction {
            Direction::Up => migration.up(&manager).await?,
            Direction::Down => migration.down(&manager).await?,
        }
    }
    let ledger = match direction {
        Direction::Up => Statement::from_sql_and_values(
            backend,
            format!("INSERT INTO {MIGRATION_TABLE} (version, applied_at) VALUES ($1, $2)"),
            [
                migration.name().into(),
                jiff::Timestamp::now().as_second().into(),
            ],
        ),
        Direction::Down => Statement::from_sql_and_values(
            backend,
            format!("DELETE FROM {MIGRATION_TABLE} WHERE version = $1"),
            [migration.name().into()],
        ),
    };
    transaction.execute_raw(ledger).await?;
    transaction.commit().await?;
    info!("migration done");
    Ok(())
}

#[cfg(test)]
mod tests;
