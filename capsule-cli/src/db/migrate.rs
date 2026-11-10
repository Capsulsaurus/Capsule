//! Forward-only stepwise migrator for the CLI database, keyed on `PRAGMA user_version`.
//!
//! Modelled on `capsule-library`'s catalog migrator: opening a database stamped below
//! [`SCHEMA_VERSION`] walks it up one step per version; there is no downgrade; a database stamped
//! above it was written by a newer build and is refused untouched
//! ([Versioning § System Invariants]: a version mismatch may refuse a write, never lose data).
//!
//! # A shipped step is immutable
//!
//! Once a step appears in [`STEPS`] it is never edited, only followed by another: some user's
//! database has been walked through the old text, so editing it would make one `user_version`
//! name two schemas. `shipped_steps_are_immutable` in this module's tests locks each step's
//! rendering behind a SHA-256 fingerprint.
//!
//! # Adopting a sea-orm database
//!
//! Before this migrator the CLI kept the same file on sea-orm, whose migrator records what it ran
//! in a `seaql_migrations` table and leaves `user_version` at `0`. Such a database is adopted in
//! place, in one transaction: every migration it lists must be one this module knows, and the
//! tables each one created must be there; the dormant `profiles`, `albums` and `assets` tables
//! (created by the first sea-orm migration and never read or written) are dropped; the steps run
//! (idempotent, so they complete a database an older build left short of the last migration);
//! `seaql_migrations` is dropped and the stamp written. Anything else in a `seaql_migrations`
//! table is refused, so peering's floor and tombstones are never discarded by a guess.
//!
//! [Versioning § System Invariants]: https://docs/design/versioning/#system-invariants

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use thiserror::Error;

/// The schema version this build writes.
pub const SCHEMA_VERSION: u32 = 1;

/// A statement in a migration step, and the condition it runs under. Declarative, so a step's
/// whole behaviour is data and can be fingerprinted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ddl {
    /// Run unconditionally. Must be idempotent (`IF NOT EXISTS`).
    Always(&'static str),
    /// Run only when `table` lacks `column`: `ALTER TABLE … ADD COLUMN` is not idempotent.
    IfColumnMissing {
        table: &'static str,
        column: &'static str,
        sql: &'static str,
    },
}

/// One version-to-version step; `to` is always `from + 1`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Step {
    pub(crate) from: u32,
    pub(crate) to: u32,
    /// Stable identifier, in logs and in the immutability fingerprint.
    pub(crate) name: &'static str,
    pub(crate) ddl: &'static [Ddl],
}

// ── The shipped steps ───────────────────────────────────────────────────────
//
// APPEND ONLY. See the module docs: a step that has shipped is never edited.

/// The sync store: the single opaque feed cursor (one row, id = 0) and the assets the feed has
/// landed, which `capsule list` queries client-side ([`crate::syncstore`]).
const SYNC_STORE: &str = "
CREATE TABLE IF NOT EXISTS sync_cursor (
    id INTEGER NOT NULL PRIMARY KEY,
    cursor BLOB NOT NULL
);

CREATE TABLE IF NOT EXISTS synced_assets (
    asset_id TEXT NOT NULL PRIMARY KEY,
    album_id TEXT NOT NULL,
    sync_seq BIGINT NOT NULL,
    kind TEXT NOT NULL,
    original_held BOOLEAN NOT NULL,
    tombstoned BOOLEAN NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_synced_assets_album_id ON synced_assets (album_id);
CREATE INDEX IF NOT EXISTS idx_synced_assets_tombstoned ON synced_assets (tombstoned);
";

/// The retention deadline a synced delete names: the `retention_until` of the asset's signed
/// delete manifest, as unix seconds rounded up. Null on a live asset, on a purge already
/// recorded, and on a delete whose manifest names no deadline.
const SYNC_RETENTION: &str = "ALTER TABLE synced_assets ADD COLUMN retention_until BIGINT;";

/// Peering's durable state: the anti-rollback floor on the account's device directory and the
/// tombstones of purged assets ([`crate::peering_state`]).
///
/// Both are security state, so the store itself refuses to weaken them: the floor's version
/// never decreases and its row is never deleted, and a tombstone is never updated or deleted.
/// Each refusal aborts the statement that attempted it, except an insert below the stored floor,
/// which is skipped: that is how a lower raise is a no-op, and how `INSERT OR REPLACE` (whose
/// implicit delete fires no trigger) cannot lower it either.
const PEERING_STATE: &str = r"
CREATE TABLE IF NOT EXISTS peering_floor (
    device_id TEXT NOT NULL PRIMARY KEY,
    directory_version BIGINT NOT NULL CHECK (directory_version >= 0)
);

CREATE TRIGGER IF NOT EXISTS peering_floor_insert_never_lowers
BEFORE INSERT ON peering_floor
WHEN EXISTS (
    SELECT 1 FROM peering_floor
    WHERE device_id = NEW.device_id AND directory_version > NEW.directory_version
)
BEGIN
    SELECT RAISE(IGNORE);
END;

CREATE TRIGGER IF NOT EXISTS peering_floor_never_lowers
BEFORE UPDATE ON peering_floor
WHEN NEW.directory_version < OLD.directory_version OR NEW.device_id <> OLD.device_id
BEGIN
    SELECT RAISE(ABORT, 'the peering directory floor never lowers');
END;

CREATE TRIGGER IF NOT EXISTS peering_floor_never_deleted
BEFORE DELETE ON peering_floor
BEGIN
    SELECT RAISE(ABORT, 'the peering directory floor is never deleted');
END;

CREATE TABLE IF NOT EXISTS peering_tombstones (
    asset_id TEXT NOT NULL PRIMARY KEY,
    recorded_at TEXT NOT NULL
);

CREATE TRIGGER IF NOT EXISTS peering_tombstones_never_updated
BEFORE UPDATE ON peering_tombstones
BEGIN
    SELECT RAISE(ABORT, 'a peering tombstone is never updated');
END;

CREATE TRIGGER IF NOT EXISTS peering_tombstones_never_deleted
BEFORE DELETE ON peering_tombstones
BEGIN
    SELECT RAISE(ABORT, 'a peering tombstone is never deleted');
END;
";

/// v0 → v1: the sync store and peering state, the effective schema the sea-orm migrations left.
/// Idempotent throughout, so it both creates a fresh database and completes an adopted one.
const STEP_0_TO_1: Step = Step {
    from: 0,
    to: 1,
    name: "v0_to_v1_sync_store_and_peering_state",
    ddl: &[
        Ddl::Always(SYNC_STORE),
        Ddl::IfColumnMissing {
            table: "synced_assets",
            column: "retention_until",
            sql: SYNC_RETENTION,
        },
        Ddl::Always(PEERING_STATE),
    ],
};

/// Every shipped step, in ascending order. Append only.
pub(crate) const STEPS: &[Step] = &[STEP_0_TO_1];

/// The sea-orm migrations an adopted database may list, and the tables each one created that
/// must still be there. The first created only the dormant tables adoption drops; the last added
/// a column, checked on its own.
const SEAQL_KNOWN: &[(&str, &[&str])] = &[
    ("m20250718_000000_initialize", &[]),
    (
        "m20260710_000000_sync_store",
        &["sync_cursor", "synced_assets"],
    ),
    (
        "m20261023_000000_peering_state",
        &["peering_floor", "peering_tombstones"],
    ),
    ("m20261024_000000_sync_retention", &["synced_assets"]),
];

/// The sea-orm migration that added `synced_assets.retention_until`.
const SEAQL_RETENTION: &str = "m20261024_000000_sync_retention";

/// The tables the first sea-orm migration created and nothing ever used, dropped on adoption.
const DORMANT_TABLES: &[&str] = &["assets", "albums", "profiles"];

/// Why the database could not be brought to [`SCHEMA_VERSION`]. Every variant leaves it as it was.
#[derive(Debug, Error)]
pub enum MigrationError {
    /// Written by a newer build than this one; left byte-for-byte untouched.
    #[error(
        "the CLI database schema v{found} is newer than this build supports (v{supported}); \
         update capsule to open it"
    )]
    TooNew {
        /// The stamp the database carries.
        found: u32,
        /// The newest this build knows.
        supported: u32,
    },
    /// A sea-orm database lists a migration this build does not know: a newer build wrote it.
    #[error(
        "the CLI database was migrated by another build ({migration} is not a migration this \
         build knows); it was left untouched so its peering state is not lost"
    )]
    UnknownSeaOrmMigration {
        /// The unknown migration's name.
        migration: String,
    },
    /// A sea-orm database lists a migration whose table or column is gone.
    #[error(
        "the CLI database lists {migration} but {missing} is missing; it was left untouched \
         rather than recreated empty"
    )]
    AdoptionIncomplete {
        /// The migration that should have created it.
        migration: String,
        /// The missing table or column.
        missing: String,
    },
    /// Neither stamped nor a sea-orm database, yet not empty.
    #[error("the CLI database has tables ({tables}) but no schema version; it was left untouched")]
    Unrecognised {
        /// The tables it holds.
        tables: String,
    },
    /// No step is registered out of `from`.
    #[error("no CLI database migration step is registered from v{from} (target v{to})")]
    MissingStep {
        /// The version with no step out of it.
        from: u32,
        /// The target version.
        to: u32,
    },
    /// A step failed; the transaction rolled back.
    #[error("CLI database migration step v{from} -> v{to} ({name}) failed: {source}")]
    Step {
        /// The step's source version.
        from: u32,
        /// The step's target version.
        to: u32,
        /// The step's name.
        name: &'static str,
        /// The statement's error.
        #[source]
        source: rusqlite::Error,
    },
    /// SQLite refused a read or the transaction itself.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// What [`migrate`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The stamp the database carried on open (`0` for a fresh or a sea-orm database).
    pub from: u32,
    /// The stamp it carries now: always [`SCHEMA_VERSION`] on success.
    pub to: u32,
    /// Whether a sea-orm database was adopted.
    pub adopted: bool,
    /// The steps applied, by name.
    pub applied: Vec<&'static str>,
}

/// Bring `conn` to [`SCHEMA_VERSION`].
///
/// A current database is read and nothing is written. Otherwise everything runs in one
/// `IMMEDIATE` transaction that re-reads the stamp, so two processes opening the same file at
/// once migrate it once: a fresh database gets the steps from `0`, a sea-orm database is adopted
/// (module docs), and a database newer than this build is refused untouched.
///
/// # Errors
///
/// See [`MigrationError`]; on any of them the database is unchanged.
#[tracing::instrument(level = "debug", skip_all, fields(target_version = SCHEMA_VERSION), err)]
pub fn migrate(conn: &mut Connection) -> Result<Outcome, MigrationError> {
    let stamped = read_user_version(conn)?;
    if stamped == SCHEMA_VERSION && !table_exists(conn, "seaql_migrations")? {
        tracing::debug!(
            version = stamped,
            "local database migration: already current"
        );
        return Ok(Outcome {
            from: stamped,
            to: stamped,
            adopted: false,
            applied: Vec::new(),
        });
    }
    refuse_too_new(stamped)?;

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    // Re-read under the write lock: another process may have migrated since.
    let stamped = read_user_version(&tx)?;
    refuse_too_new(stamped)?;
    let seaql = table_exists(&tx, "seaql_migrations")?;
    let adopted = stamped == 0 && seaql;
    if adopted {
        verify_seaql(&tx)?;
    } else if stamped == 0 {
        let tables = user_tables(&tx)?;
        if !tables.is_empty() {
            tracing::error!(
                ?tables,
                "local database migration: unstamped and unrecognised"
            );
            return Err(MigrationError::Unrecognised {
                tables: tables.join(", "),
            });
        }
        tracing::info!(
            version = SCHEMA_VERSION,
            "local database migration: creating a fresh database"
        );
    }

    let mut applied = Vec::new();
    let mut cursor = stamped;
    while cursor < SCHEMA_VERSION {
        let step = STEPS
            .iter()
            .find(|s| s.from == cursor)
            .ok_or(MigrationError::MissingStep {
                from: cursor,
                to: SCHEMA_VERSION,
            })?;
        apply_step(&tx, step).map_err(|source| {
            tracing::error!(from = step.from, to = step.to, name = step.name, error = %source, "local database migration: step failed; rolled back");
            MigrationError::Step {
                from: step.from,
                to: step.to,
                name: step.name,
                source,
            }
        })?;
        applied.push(step.name);
        cursor = step.to;
    }

    if adopted {
        for table in DORMANT_TABLES {
            tracing::debug!(table, "local database adoption: dropping a dormant table");
            tx.execute_batch(&format!("DROP TABLE IF EXISTS {table};"))?;
        }
        tx.execute_batch("DROP TABLE seaql_migrations;")?;
    }
    tx.execute_batch(&format!("PRAGMA user_version = {cursor};"))?;
    tx.commit()?;

    tracing::info!(
        from = stamped,
        to = cursor,
        adopted,
        steps = applied.len(),
        "local database migration: complete"
    );
    Ok(Outcome {
        from: stamped,
        to: cursor,
        adopted,
        applied,
    })
}

fn refuse_too_new(stamped: u32) -> Result<(), MigrationError> {
    if stamped > SCHEMA_VERSION {
        tracing::error!(
            found = stamped,
            supported = SCHEMA_VERSION,
            "local database migration: newer than this build; nothing was written"
        );
        return Err(MigrationError::TooNew {
            found: stamped,
            supported: SCHEMA_VERSION,
        });
    }
    Ok(())
}

/// Check a sea-orm database is one this build can adopt: every listed migration known, and
/// every table (and the retention column) a listed migration created still there.
fn verify_seaql(tx: &Transaction<'_>) -> Result<(), MigrationError> {
    let mut stmt = tx.prepare("SELECT version FROM seaql_migrations ORDER BY version")?;
    let listed = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    tracing::debug!(
        ?listed,
        "local database adoption: sea-orm migrations listed"
    );
    for migration in &listed {
        let Some((_, tables)) = SEAQL_KNOWN.iter().find(|(name, _)| name == migration) else {
            tracing::error!(%migration, "local database adoption: an unknown sea-orm migration; refusing");
            return Err(MigrationError::UnknownSeaOrmMigration {
                migration: migration.clone(),
            });
        };
        for table in *tables {
            if !table_exists(tx, table)? {
                tracing::error!(%migration, table, "local database adoption: a listed table is missing; refusing");
                return Err(MigrationError::AdoptionIncomplete {
                    migration: migration.clone(),
                    missing: (*table).to_owned(),
                });
            }
        }
        if migration == SEAQL_RETENTION && !has_column(tx, "synced_assets", "retention_until")? {
            return Err(MigrationError::AdoptionIncomplete {
                migration: migration.clone(),
                missing: "synced_assets.retention_until".to_owned(),
            });
        }
    }
    tracing::debug!("local database adoption: verified");
    Ok(())
}

/// Run one step's statements on `tx`.
fn apply_step(tx: &Transaction<'_>, step: &Step) -> rusqlite::Result<()> {
    tracing::debug!(
        from = step.from,
        to = step.to,
        name = step.name,
        "local database migration: applying step"
    );
    for (idx, ddl) in step.ddl.iter().enumerate() {
        match ddl {
            Ddl::Always(sql) => {
                tracing::trace!(step = step.name, idx, "local database migration: exec");
                tx.execute_batch(sql)?;
            }
            Ddl::IfColumnMissing { table, column, sql } => {
                let present = has_column(tx, table, column)?;
                tracing::trace!(
                    step = step.name,
                    idx,
                    table,
                    column,
                    present,
                    "local database migration: guarded exec (if column missing)"
                );
                if !present {
                    tx.execute_batch(sql)?;
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn read_user_version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
}

fn table_exists(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
}

fn user_tables(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut stmt = conn.prepare(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' \
         ORDER BY name",
    )?;
    stmt.query_map([], |row| row.get(0))?.collect()
}

/// Whether `table` has `column`. `table` is always one of this module's own identifiers.
fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests;
