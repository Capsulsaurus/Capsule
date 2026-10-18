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
//! applied, naming the command an operator has to run — and reads the migration crate's
//! checksum ledger beside it, refusing an applied ordinal whose source has changed since it ran
//! (#524).
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

/// One migration this binary was built against: its name and the checksum of its source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpectedMigration {
    /// The ordinal's name, as `seaql_migrations.version` records it.
    pub name: &'static str,
    /// The lowercase hex SHA-256 of the migration's source, as the migration crate's
    /// `checksum` computes it and its checksum ledger records it.
    pub checksum: &'static str,
}

/// The migrations this binary was built against, oldest first, each with its checksum.
///
/// Compiled in rather than read from the migration crate, which `capsule-server` cannot link.
/// `postgres::tests::the_expected_migrations_are_the_migrations_that_exist` compares the two
/// under `cfg(test)`, where the migration crate *is* available — so neither a name nor a
/// checksum can drift from the crate that defines it without a red test. That test prints the
/// list to paste here when a migration is added.
pub const EXPECTED_MIGRATIONS: &[ExpectedMigration] = &[
    ExpectedMigration {
        name: "m20260902_000001_asset_index",
        checksum: "e593dd1fe8e19373d2ef82235413efcba58ffc0a4410200b868e88d7dbc3e9f2",
    },
    ExpectedMigration {
        name: "m20260902_000002_accounts",
        checksum: "0570068d319fc6e76ab91858eb5420cf9a79e57d4fcae1528d06c6bbb7395330",
    },
    ExpectedMigration {
        name: "m20260902_000003_cohorts",
        checksum: "7537434811f3cc1d7fe120808612f58dd9b2414b2ec8c9753df55790989eef97",
    },
    ExpectedMigration {
        name: "m20260902_000004_quota",
        checksum: "d813a6c6f8267dae80d28ad082eba3b2a9bb4edf691a89eb07ea77d35883f335",
    },
    ExpectedMigration {
        name: "m20260902_000005_album_membership",
        checksum: "274c5b01aff0c0f15a7e039788aca4ef9e563f547e8bd2d8f52f7ea28740bd8c",
    },
    ExpectedMigration {
        name: "m20260902_000006_federation",
        checksum: "243991f1a776bcff1a6525ed8e01f228b8eef4b179537a654c637db0782dde6c",
    },
    ExpectedMigration {
        name: "m20260902_000007_albums",
        checksum: "b70c13d6c2202dec7495cce071c304d78b1a4fd2673c6c75f7933586b1c6bbd2",
    },
    ExpectedMigration {
        name: "m20260902_000008_device_directories",
        checksum: "dd6256a4de3e5b4b662956fb376c28c117942201cfa7eecf11c0f8bfb6d0e124",
    },
    ExpectedMigration {
        name: "m20260902_000009_moderation",
        checksum: "6536a1cbe51fc767b5243fb396f72c096cf4c95884b09225292b620421340da1",
    },
    ExpectedMigration {
        name: "m20260902_000010_shares",
        checksum: "8446e6f8a253dc4374325c52e4d3f903907c25ad5fadac5e554a3fdd6bf024e3",
    },
    ExpectedMigration {
        name: "m20260902_000011_drops",
        checksum: "bae39984f5da2b0426b220145ed120b20864fdee0c0287f33bd8dd9408be4138",
    },
    ExpectedMigration {
        name: "m20260902_000012_escrow",
        checksum: "6a84a3ccc0f15cbdab7a86a5307f5d372e76c320cedd25da44e5d8e6967e3ffc",
    },
    ExpectedMigration {
        name: "m20260902_000013_collection_marks",
        checksum: "8cd759757f04ea3241fde1c30e70f75a30ba965e04f0e3e65922d72c4bba2a64",
    },
    ExpectedMigration {
        name: "m20260902_000014_receipts",
        checksum: "72fbd3c1019bb3cf5cb72dc499f25f8d1fabf9eab27956e7756e4a269233fcbb",
    },
    ExpectedMigration {
        name: "m20260902_000015_totp",
        checksum: "72af8506da7cee5dae68dce4e118b80dd9d5db767d298553661b725fe3cb3352",
    },
    ExpectedMigration {
        name: "m20260902_000016_federated_accounts",
        checksum: "a164872db69e744781ae9b61c6ada7fafa638182cc2980bdc9f1daf6f54831f7",
    },
    ExpectedMigration {
        name: "m20260902_000017_migration_checksums",
        checksum: "ec6f63e917e61ced06acf221c5d419f70341f6e8c68086fa2b328eea37ba5fe8",
    },
    ExpectedMigration {
        name: "m20260902_000018_sealed_identities",
        checksum: "d9187cb622fa496c5eb6adea98dd6313c8b4b45a2521e3edfba2c56bea8acbb9",
    },
];

/// The command an operator runs to apply them.
const MIGRATION_COMMAND: &str = "capsule-server-migration up";

/// The migration crate's companion ledger of applied checksums.
///
/// The name is `server_migration::CHECKSUM_LEDGER`'s; a test holds the two equal.
const CHECKSUM_LEDGER: &str = "capsule_migration_checksums";

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
    /// An applied migration is not the migration this binary was built against: it was edited
    /// after it ran, or its checksum was never recorded.
    ///
    /// Distinct from [`PostgresError::Schema`] because running the migration command does not
    /// fix it — nothing is pending — so the message must not send an operator there.
    #[error(
        "the database schema has drifted from this binary: {detail}; an applied migration must \
         never be edited — restore its original source and ship the change as a new migration"
    )]
    SchemaDrift {
        /// Which ordinals differ, with the checksum recorded and the one expected.
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

/// Refuse to continue unless every migration in [`EXPECTED_MIGRATIONS`] has been applied, and
/// applied as the text this binary was built against.
///
/// Reads `seaql_migrations` — the ledger `sea-orm-migration` maintains — and the migration
/// crate's checksum ledger beside it, and compares both against the compiled-in list:
///
/// - A **missing** ordinal is fatal ([`PostgresError::Schema`]): the binary would query a table
///   or a column that does not exist, and the first symptom would be a 500 on whichever request
///   reached it first.
/// - An applied ordinal whose recorded checksum differs from the expected one, or that has no
///   recorded checksum at all, is fatal too ([`PostgresError::SchemaDrift`]). Comparing names
///   alone would pass a migration that was edited after it ran (#524): the database would hold
///   whatever the old text built, and the binary would assume what the new text builds.
///
/// An ordinal the database holds and this binary does not know about is **not** fatal, and
/// neither is its checksum checked, and that asymmetry is the rolling-deploy contract: during a
/// deploy the migration runs first and the old replicas keep serving, so a newer schema
/// underneath an older binary is the normal state for the length of the rollout. A migration
/// that removes something an older binary reads is a migration that has to be split in two,
/// which is a property of the migration rather than something this check can enforce.
///
/// # Errors
///
/// Returns [`PostgresError::Schema`] if a ledger cannot be read or an expected ordinal is
/// absent, and [`PostgresError::SchemaDrift`] if an applied ordinal's checksum is not the
/// expected one.
pub async fn assert_schema_current(connection: &DatabaseConnection) -> Result<(), PostgresError> {
    let applied = read_ledger(
        connection,
        "SELECT version FROM seaql_migrations",
        "seaql_migrations",
    )
    .await?
    .into_iter()
    .map(|(version, _)| version)
    .collect::<Vec<_>>();
    // The applied set is checked first, so a database migrated before the checksum ledger
    // existed is told to run the migration that creates it rather than that it has drifted.
    check_applied(EXPECTED_MIGRATIONS, &applied)?;
    let recorded = read_ledger(
        connection,
        &format!("SELECT version, checksum FROM {CHECKSUM_LEDGER}"),
        CHECKSUM_LEDGER,
    )
    .await?;
    check_checksums(EXPECTED_MIGRATIONS, &recorded)?;

    tracing::info!(
        applied = applied.len(),
        expected = EXPECTED_MIGRATIONS.len(),
        checksums = recorded.len(),
        "the database schema is current"
    );
    Ok(())
}

/// Read `(version, checksum)` rows from one ledger; `checksum` is `None` where the query does
/// not select one.
async fn read_ledger(
    connection: &DatabaseConnection,
    query: &str,
    table: &str,
) -> Result<Vec<(String, Option<String>)>, PostgresError> {
    use sea_orm::ConnectionTrait as _;

    let rows = connection
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            query.to_owned(),
        ))
        .await
        .map_err(|error| PostgresError::Schema {
            detail: format!("`{table}` could not be read ({error})"),
        })?;
    let mut entries = Vec::with_capacity(rows.len());
    for row in rows {
        let version: String =
            row.try_get("", "version")
                .map_err(|error| PostgresError::Schema {
                    detail: format!("`{table}.version` could not be read ({error})"),
                })?;
        let checksum: Option<String> = row.try_get("", "checksum").ok();
        entries.push((version, checksum));
    }
    tracing::debug!(table, rows = entries.len(), "read a migration ledger");
    Ok(entries)
}

/// Every expected ordinal is in the applied set.
fn check_applied(expected: &[ExpectedMigration], applied: &[String]) -> Result<(), PostgresError> {
    let missing: Vec<&str> = expected
        .iter()
        .map(|migration| migration.name)
        .filter(|name| !applied.iter().any(|held| held == name))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(PostgresError::Schema {
            detail: format!("{} has not been applied", missing.join(", ")),
        })
    }
}

/// Every expected ordinal has a recorded checksum, and it is the expected one.
///
/// A recorded row for an ordinal this binary does not know is ignored, for the reason
/// [`assert_schema_current`] gives.
fn check_checksums(
    expected: &[ExpectedMigration],
    recorded: &[(String, Option<String>)],
) -> Result<(), PostgresError> {
    let drifted: Vec<String> = expected
        .iter()
        .filter_map(|migration| {
            let held = recorded
                .iter()
                .find(|(version, _)| version == migration.name)
                .and_then(|(_, checksum)| checksum.as_deref());
            match held {
                Some(held) if held == migration.checksum => None,
                Some(held) => Some(format!(
                    "{} was applied with checksum {held}, this binary expects {}",
                    migration.name, migration.checksum
                )),
                None => Some(format!(
                    "{} has no recorded checksum in `{CHECKSUM_LEDGER}`",
                    migration.name
                )),
            }
        })
        .collect();
    if drifted.is_empty() {
        Ok(())
    } else {
        for line in &drifted {
            tracing::error!(drift = %line, "an applied migration is not the one this binary expects");
        }
        Err(PostgresError::SchemaDrift {
            detail: drifted.join("; "),
        })
    }
}

#[cfg(test)]
mod tests {
    use server_migration::MigratorTrait as _;

    use super::{
        CHECKSUM_LEDGER, EXPECTED_MIGRATIONS, ExpectedMigration, PostgresError,
        assert_schema_current, check_applied, check_checksums, testing,
    };

    /// The compiled-in list is the migration crate's list, names and checksums both.
    ///
    /// `capsule-server` cannot link the migrator in a normal build, so the list it refuses to
    /// boot on is a copy. This is the assertion that keeps the copy honest — and it needs no
    /// container, so it runs on every ordinary test pass. A failure prints the list to paste.
    #[test]
    fn the_expected_migrations_are_the_migrations_that_exist() {
        let defined: Vec<String> = server_migration::Migrator::migrations()
            .iter()
            .map(|migration| migration.name().to_owned())
            .collect();
        let checksums = server_migration::checksums();
        assert_eq!(
            checksums.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            defined.iter().collect::<Vec<_>>(),
            "the migration crate's checksums are its migrations'"
        );
        let compiled: Vec<(String, String)> = EXPECTED_MIGRATIONS
            .iter()
            .map(|migration| (migration.name.to_owned(), migration.checksum.to_owned()))
            .collect();
        let paste = checksums
            .iter()
            .fold(String::new(), |mut paste, (name, checksum)| {
                use std::fmt::Write as _;
                let _ = write!(
                    paste,
                    "    ExpectedMigration {{\n        name: \"{name}\",\n        checksum: \
                 \"{checksum}\",\n    }},\n"
                );
                paste
            });
        assert_eq!(
            compiled, checksums,
            "`postgres::EXPECTED_MIGRATIONS` is what `serve` refuses to boot without; it has \
             drifted from `capsule-server-migration`. If a migration was *added*, replace the \
             list with:\n{paste}\nIf an applied migration was *edited*, revert the edit and add \
             a new migration instead."
        );
    }

    #[test]
    fn the_checksum_ledger_is_the_one_the_migration_crate_writes() {
        assert_eq!(CHECKSUM_LEDGER, server_migration::CHECKSUM_LEDGER);
    }

    const ONE: ExpectedMigration = ExpectedMigration {
        name: "m1",
        checksum: "aaaa",
    };
    const TWO: ExpectedMigration = ExpectedMigration {
        name: "m2",
        checksum: "bbbb",
    };

    fn recorded(rows: &[(&str, Option<&str>)]) -> Vec<(String, Option<String>)> {
        rows.iter()
            .map(|(version, checksum)| ((*version).to_owned(), checksum.map(str::to_owned)))
            .collect()
    }

    #[test]
    fn a_missing_ordinal_is_not_current_and_names_the_migration_command() {
        let error = check_applied(&[ONE, TWO], &["m1".to_owned()]).expect_err("m2 is missing");
        assert!(matches!(error, PostgresError::Schema { .. }), "{error:?}");
        let rendered = format!("{error}");
        assert!(rendered.contains("m2 has not been applied"), "{rendered}");
        assert!(
            rendered.contains("capsule-server-migration up"),
            "{rendered}"
        );
    }

    #[test]
    fn an_ordinal_ahead_of_the_binary_is_not_fatal() {
        check_applied(&[ONE], &["m1".to_owned(), "m9".to_owned()]).expect("a newer schema");
        check_checksums(
            &[ONE],
            &recorded(&[("m1", Some("aaaa")), ("m9", Some("anything"))]),
        )
        .expect("an unknown ordinal's checksum is not this binary's to judge");
    }

    #[test]
    fn matching_checksums_are_current() {
        check_checksums(
            &[ONE, TWO],
            &recorded(&[("m2", Some("bbbb")), ("m1", Some("aaaa"))]),
        )
        .expect("every checksum matches");
    }

    #[test]
    fn an_edited_migration_is_drift_and_does_not_send_the_operator_to_migrate() {
        let error = check_checksums(
            &[ONE, TWO],
            &recorded(&[("m1", Some("aaaa")), ("m2", Some("cccc"))]),
        )
        .expect_err("m2 was edited");
        assert!(
            matches!(error, PostgresError::SchemaDrift { .. }),
            "{error:?}"
        );
        let rendered = format!("{error}");
        assert!(
            rendered.contains("m2 was applied with checksum cccc, this binary expects bbbb"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("m1"),
            "only the drifted ordinal: {rendered}"
        );
        assert!(
            !rendered.contains("capsule-server-migration up"),
            "migrating does not fix drift: {rendered}"
        );
    }

    #[test]
    fn an_applied_migration_without_a_checksum_is_drift() {
        let error = check_checksums(&[ONE, TWO], &recorded(&[("m1", Some("aaaa"))]))
            .expect_err("m2 has no row");
        assert!(
            matches!(error, PostgresError::SchemaDrift { .. }),
            "{error:?}"
        );
        assert!(
            format!("{error}").contains("m2 has no recorded checksum"),
            "{error}"
        );
        let error = check_checksums(&[ONE], &recorded(&[("m1", None)])).expect_err("a null row");
        assert!(
            matches!(error, PostgresError::SchemaDrift { .. }),
            "{error:?}"
        );
    }

    mod postgres_conformance {
        use sea_orm::ConnectionTrait as _;
        use server_migration::MigratorTrait as _;

        use super::{
            CHECKSUM_LEDGER, EXPECTED_MIGRATIONS, PostgresError, assert_schema_current, testing,
        };

        /// Run one statement against the test database.
        async fn execute(connection: &sea_orm::DatabaseConnection, sql: &str) {
            connection
                .execute_unprepared(sql)
                .await
                .unwrap_or_else(|error| panic!("`{sql}` failed: {error}"));
        }

        /// Every applied migration's checksum is recorded as it is applied, and a recorded
        /// checksum that is not the binary's — an edited migration — refuses to boot.
        #[tokio::test]
        async fn a_drifted_checksum_refuses() {
            let Some(database) = testing::start("the checksum drift refusal").await else {
                return;
            };
            let connection = database.connection();

            let rows = connection
                .query_all(sea_orm::Statement::from_string(
                    sea_orm::DbBackend::Postgres,
                    format!("SELECT version, checksum FROM {CHECKSUM_LEDGER} ORDER BY version"),
                ))
                .await
                .expect("the checksum ledger is readable");
            let recorded: Vec<(String, String)> = rows
                .iter()
                .map(|row| {
                    (
                        row.try_get("", "version").expect("a version column"),
                        row.try_get("", "checksum").expect("a checksum column"),
                    )
                })
                .collect();
            let expected: Vec<(String, String)> = EXPECTED_MIGRATIONS
                .iter()
                .map(|migration| (migration.name.to_owned(), migration.checksum.to_owned()))
                .collect();
            assert_eq!(recorded, expected, "applying records every checksum");

            let edited = EXPECTED_MIGRATIONS[1].name;
            execute(
                connection,
                &format!(
                    "UPDATE {CHECKSUM_LEDGER} SET checksum = 'edited' WHERE version = '{edited}'"
                ),
            )
            .await;
            let error = assert_schema_current(connection)
                .await
                .expect_err("an edited migration is not current");
            assert!(
                matches!(error, PostgresError::SchemaDrift { .. }),
                "{error:?}"
            );
            assert!(format!("{error}").contains(edited), "{error}");

            execute(
                connection,
                &format!("DELETE FROM {CHECKSUM_LEDGER} WHERE version = '{edited}'"),
            )
            .await;
            let error = assert_schema_current(connection)
                .await
                .expect_err("an unrecorded migration is not current");
            assert!(
                matches!(error, PostgresError::SchemaDrift { .. }),
                "{error:?}"
            );
        }

        /// A database migrated before the checksum ledger existed is backfilled by `000017`, not
        /// refused for the checksums nobody recorded.
        #[tokio::test]
        async fn a_database_migrated_before_the_ledger_is_backfilled() {
            let Some(database) = testing::start("the checksum ledger backfill").await else {
                return;
            };
            let connection = database.connection();

            // The state a pre-#524 database is in: `000001`–`000016` applied, no ledger. Every
            // ordinal after `000017` rolls back with it.
            server_migration::Migrator::down(connection, Some(2))
                .await
                .expect("`000018` and `000017` roll back");
            execute(connection, &format!("DROP TABLE {CHECKSUM_LEDGER}")).await;
            let error = assert_schema_current(connection)
                .await
                .expect_err("the backfill is pending");
            assert!(matches!(error, PostgresError::Schema { .. }), "{error:?}");

            server_migration::Migrator::up(connection, None)
                .await
                .expect("`000017` applies");
            assert_schema_current(connection)
                .await
                .expect("the backfill recorded every checksum");
        }

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
            // Each rollback removed its own checksum with it.
            let left = connection
                .query_all(sea_orm::Statement::from_string(
                    sea_orm::DbBackend::Postgres,
                    format!("SELECT version FROM {CHECKSUM_LEDGER}"),
                ))
                .await
                .expect("the checksum ledger is readable");
            assert!(
                left.is_empty(),
                "{} checksum rows outlived a full rollback",
                left.len()
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
            let expected: Vec<&str> = EXPECTED_MIGRATIONS
                .iter()
                .map(|migration| migration.name)
                .collect();
            assert_eq!(applied, expected);
        }
    }
}
