use server_migration::MigratorTrait as _;

use super::{
    CHECKSUM_LEDGER, EXPECTED_MIGRATIONS, ExpectedMigration, PostgresError, assert_schema_current,
    check_applied, check_checksums, testing,
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
            .query_all_raw(sea_orm::Statement::from_string(
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
            &format!("UPDATE {CHECKSUM_LEDGER} SET checksum = 'edited' WHERE version = '{edited}'"),
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
        // ordinal after `000017` rolls back with it, so the step count is derived from the
        // list rather than written down, and a new ordinal cannot leave `000017` applied.
        let ledger = EXPECTED_MIGRATIONS
            .iter()
            .position(|migration| migration.name == "m20260902_000017_migration_checksums")
            .expect("`000017` is an expected migration");
        let steps = u32::try_from(EXPECTED_MIGRATIONS.len() - ledger)
            .expect("the migration count fits a u32");
        server_migration::Migrator::down(connection, Some(steps))
            .await
            .expect("`000017` and every later ordinal roll back");
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
            .query_all_raw(sea_orm::Statement::from_string(
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
            .query_all_raw(sea_orm::Statement::from_string(
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

    /// Whether `share_serves` carries an index named `idx_share_serves_address` whose only
    /// key is `address`.
    async fn has_address_index(connection: &sea_orm::DatabaseConnection) -> bool {
        let rows = connection
            .query_all_raw(sea_orm::Statement::from_string(
                sea_orm::DbBackend::Postgres,
                "SELECT indexdef FROM pg_indexes \
                     WHERE tablename = 'share_serves' AND indexname = 'idx_share_serves_address'"
                    .to_owned(),
            ))
            .await
            .expect("pg_indexes is readable");
        rows.iter().any(|row| {
            let definition: String = row.try_get("", "indexdef").expect("an indexdef column");
            definition.ends_with("(address)")
        })
    }

    /// `000019` indexes `share_serves` by address — what the collector's live-link check
    /// probes for every blob no asset holds (#659) — and its rollback drops that index alone.
    #[tokio::test]
    async fn share_serves_is_indexed_by_address() {
        let Some(database) = testing::start("the share_serves address index").await else {
            return;
        };
        let connection = database.connection();

        assert!(
            has_address_index(connection).await,
            "a migrated database indexes share_serves by address"
        );

        // Every ordinal after `000019` rolls back with it, so the step count is derived from
        // the list rather than written down, and a new ordinal cannot leave `000019` applied.
        let index = EXPECTED_MIGRATIONS
            .iter()
            .position(|migration| migration.name == "m20260902_000019_share_serves_address_index")
            .expect("`000019` is an expected migration");
        let steps = u32::try_from(EXPECTED_MIGRATIONS.len() - index)
            .expect("the migration count fits a u32");
        server_migration::Migrator::down(connection, Some(steps))
            .await
            .expect("`000019` and every later ordinal roll back");
        assert!(
            !has_address_index(connection).await,
            "rolling back `000019` drops the index"
        );

        server_migration::Migrator::up(connection, None)
            .await
            .expect("`000019` re-applies");
        assert!(has_address_index(connection).await);
        assert_schema_current(connection)
            .await
            .expect("the schema is current again");
    }
}
