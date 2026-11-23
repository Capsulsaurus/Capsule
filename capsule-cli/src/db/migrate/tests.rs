//! The CLI database migrator: a fresh create, the adoption of a sea-orm database in place, the
//! refusals that leave a database untouched, and the peering triggers holding on both paths.

use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use super::*;

/// SHA-256 (hex) of each shipped step's rendering, parallel to [`STEPS`]. Append only.
const STEP_FINGERPRINTS: &[&str] =
    &["305f683eb803a3c66af88948bf388571eedefc87b9ced828d2b4c743dce1a226"];

fn render(step: &Step) -> String {
    let mut out = format!("{}->{} {}\n", step.from, step.to, step.name);
    for ddl in step.ddl {
        match ddl {
            Ddl::Always(sql) => writeln!(out, "A|{sql}").expect("writing to a String"),
            Ddl::IfColumnMissing { table, column, sql } => {
                writeln!(out, "M|{table}.{column}|{sql}").expect("writing to a String");
            }
        }
    }
    out
}

fn fingerprint(step: &Step) -> String {
    hex::encode(Sha256::digest(render(step).as_bytes()))
}

#[test]
fn shipped_steps_are_immutable() {
    assert_eq!(
        STEPS.len(),
        STEP_FINGERPRINTS.len(),
        "a step was added without its fingerprint, or a fingerprint without its step"
    );
    for (step, expected) in STEPS.iter().zip(STEP_FINGERPRINTS) {
        assert_eq!(
            &fingerprint(step),
            expected,
            "step {} was edited after it shipped; add a new step instead",
            step.name
        );
    }
}

#[test]
fn steps_form_a_contiguous_chain_to_the_current_version() {
    for (idx, step) in STEPS.iter().enumerate() {
        assert_eq!(step.from, idx as u32);
        assert_eq!(step.to, step.from + 1);
    }
    assert_eq!(STEPS.last().map(|s| s.to), Some(SCHEMA_VERSION));
}

fn memory() -> Connection {
    Connection::open_in_memory().expect("an in-memory database")
}

fn tables(conn: &Connection) -> Vec<String> {
    user_tables(conn).expect("lists tables")
}

fn triggers(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type = 'trigger' ORDER BY name")
        .expect("prepares");
    stmt.query_map([], |row| row.get(0))
        .expect("queries")
        .collect::<Result<_, _>>()
        .expect("reads")
}

fn indexes(conn: &Connection) -> Vec<String> {
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'index' AND name NOT LIKE 'sqlite_%' \
             ORDER BY name",
        )
        .expect("prepares");
    stmt.query_map([], |row| row.get(0))
        .expect("queries")
        .collect::<Result<_, _>>()
        .expect("reads")
}

const CURRENT_TABLES: &[&str] = &[
    "peering_floor",
    "peering_tombstones",
    "sync_cursor",
    "synced_assets",
];

const CURRENT_TRIGGERS: &[&str] = &[
    "peering_floor_insert_never_lowers",
    "peering_floor_never_deleted",
    "peering_floor_never_lowers",
    "peering_tombstones_never_deleted",
    "peering_tombstones_never_updated",
];

/// The floor and tombstone triggers refuse what they refuse.
fn assert_triggers_hold(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO peering_floor VALUES ('d', 5);
         INSERT INTO peering_tombstones VALUES ('a', '2026-01-01T00:00:00Z');",
    )
    .expect("writes");
    conn.execute("INSERT INTO peering_floor VALUES ('d', 3)", [])
        .expect("a lower insert is skipped, not an error");
    let floor: i64 = conn
        .query_row(
            "SELECT directory_version FROM peering_floor WHERE device_id = 'd'",
            [],
            |row| row.get(0),
        )
        .expect("reads");
    assert_eq!(floor, 5);
    assert!(
        conn.execute("UPDATE peering_floor SET directory_version = 1", [])
            .is_err()
    );
    assert!(conn.execute("DELETE FROM peering_floor", []).is_err());
    assert!(conn.execute("DELETE FROM peering_tombstones", []).is_err());
    assert!(
        conn.execute("UPDATE peering_tombstones SET recorded_at = 'x'", [])
            .is_err()
    );
}

#[test]
fn a_fresh_database_gets_the_current_schema_and_stamp() {
    let mut conn = memory();
    let outcome = migrate(&mut conn).expect("migrates");
    assert_eq!(
        outcome,
        Outcome {
            from: 0,
            to: SCHEMA_VERSION,
            adopted: false,
            applied: vec![STEP_0_TO_1.name],
        }
    );
    assert_eq!(read_user_version(&conn).expect("reads"), SCHEMA_VERSION);
    assert_eq!(tables(&conn), CURRENT_TABLES);
    assert_eq!(triggers(&conn), CURRENT_TRIGGERS);
    assert_eq!(
        indexes(&conn),
        ["idx_synced_assets_album_id", "idx_synced_assets_tombstoned"]
    );
    assert!(has_column(&conn, "synced_assets", "retention_until").expect("reads"));
    assert_triggers_hold(&conn);
}

#[test]
fn a_current_database_is_reopened_without_a_write() {
    let mut conn = memory();
    migrate(&mut conn).expect("migrates");
    conn.execute("INSERT INTO sync_cursor VALUES (0, x'01')", [])
        .expect("writes");
    let outcome = migrate(&mut conn).expect("reopens");
    assert!(outcome.applied.is_empty());
    assert!(!outcome.adopted);
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM sync_cursor", [], |row| row.get(0))
        .expect("reads");
    assert_eq!(count, 1);
}

/// The shape the sea-orm migrations left, as their SQLite DDL rendered it, with data in every
/// table that matters.
fn sea_orm_database(listed: &[&str]) -> Connection {
    let conn = memory();
    conn.execute_batch(
        r#"
        CREATE TABLE seaql_migrations ("version" varchar NOT NULL PRIMARY KEY, "applied_at" bigint NOT NULL);
        CREATE TABLE IF NOT EXISTS "profiles" ("id" char(21) NOT NULL PRIMARY KEY, "username" varchar(64) NOT NULL, "name" varchar NOT NULL);
        CREATE INDEX IF NOT EXISTS "idx_username" ON "profiles" ("username");
        CREATE TABLE IF NOT EXISTS "albums" ("id" char(21) NOT NULL PRIMARY KEY, "owner_id" char(21) NOT NULL, "name" varchar NOT NULL, "description" text NOT NULL, "created_at" timestamp_with_timezone_text NOT NULL DEFAULT CURRENT_TIMESTAMP, "modified_at" timestamp_with_timezone_text NOT NULL DEFAULT CURRENT_TIMESTAMP, "deleted_at" timestamp_with_timezone_text NULL, CONSTRAINT "fk_albums_owner_id" FOREIGN KEY ("owner_id") REFERENCES "profiles" ("id") ON DELETE RESTRICT);
        CREATE INDEX IF NOT EXISTS "idx_owner_id" ON "albums" ("owner_id");
        CREATE INDEX IF NOT EXISTS "idx_name" ON "albums" ("name");
        CREATE TABLE IF NOT EXISTS "assets" ("id" char(21) NOT NULL PRIMARY KEY, "owner_id" char(21) NOT NULL, "album_id" char(21) NULL, "width" integer NOT NULL, "height" integer NOT NULL, "date" timestamp_with_timezone_text NOT NULL, "uploaded_at" timestamp_with_timezone_text NOT NULL, "modified_at" timestamp_with_timezone_text NOT NULL, "deleted_at" timestamp_with_timezone_text NULL);
        CREATE INDEX IF NOT EXISTS "idx_album_id" ON "assets" ("album_id");
        CREATE TABLE IF NOT EXISTS "sync_cursor" ("id" integer NOT NULL PRIMARY KEY, "cursor" blob NOT NULL);
        CREATE TABLE IF NOT EXISTS "synced_assets" ("asset_id" varchar NOT NULL PRIMARY KEY, "album_id" varchar NOT NULL, "sync_seq" bigint NOT NULL, "kind" varchar NOT NULL, "original_held" boolean NOT NULL, "tombstoned" boolean NOT NULL);
        CREATE INDEX IF NOT EXISTS "idx_synced_assets_album_id" ON "synced_assets" ("album_id");
        CREATE INDEX IF NOT EXISTS "idx_synced_assets_tombstoned" ON "synced_assets" ("tombstoned");
        INSERT INTO sync_cursor VALUES (0, x'0102');
        INSERT INTO synced_assets VALUES ('YQ==', 'Yg==', 7, 'created', 1, 0);
        "#,
    )
    .expect("builds the sea-orm tables");
    if listed.contains(&"m20261023_000000_peering_state") {
        conn.execute_batch(PEERING_STATE)
            .expect("builds the peering tables");
        conn.execute_batch(
            "INSERT INTO peering_floor VALUES ('dev', 9);
             INSERT INTO peering_tombstones VALUES ('t', '2026-01-01T00:00:00Z');",
        )
        .expect("writes peering state");
    }
    if listed.contains(&SEAQL_RETENTION) {
        conn.execute_batch(SYNC_RETENTION)
            .expect("adds the retention column");
        conn.execute("UPDATE synced_assets SET retention_until = 42", [])
            .expect("writes");
    }
    for (applied_at, version) in listed.iter().enumerate() {
        conn.execute(
            "INSERT INTO seaql_migrations VALUES (?1, ?2)",
            rusqlite::params![version, applied_at as i64],
        )
        .expect("lists a migration");
    }
    conn
}

const ALL_SEAQL: &[&str] = &[
    "m20250718_000000_initialize",
    "m20260710_000000_sync_store",
    "m20261023_000000_peering_state",
    "m20261024_000000_sync_retention",
];

#[test]
fn a_sea_orm_database_is_adopted_in_place_with_its_data() {
    let mut conn = sea_orm_database(ALL_SEAQL);
    let outcome = migrate(&mut conn).expect("adopts");
    assert!(outcome.adopted);
    assert_eq!(outcome.to, SCHEMA_VERSION);
    assert_eq!(read_user_version(&conn).expect("reads"), SCHEMA_VERSION);
    assert_eq!(
        tables(&conn),
        CURRENT_TABLES,
        "the dormant tables and seaql_migrations are gone"
    );
    assert_eq!(triggers(&conn), CURRENT_TRIGGERS);

    let floor: i64 = conn
        .query_row(
            "SELECT directory_version FROM peering_floor WHERE device_id = 'dev'",
            [],
            |row| row.get(0),
        )
        .expect("the floor survives");
    assert_eq!(floor, 9);
    let tombstone: String = conn
        .query_row("SELECT asset_id FROM peering_tombstones", [], |row| {
            row.get(0)
        })
        .expect("the tombstone survives");
    assert_eq!(tombstone, "t");
    let (seq, held, retention): (i64, bool, Option<i64>) = conn
        .query_row(
            "SELECT sync_seq, original_held, retention_until FROM synced_assets",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("the synced asset survives");
    assert_eq!((seq, held, retention), (7, true, Some(42)));
    let cursor: Vec<u8> = conn
        .query_row("SELECT cursor FROM sync_cursor WHERE id = 0", [], |row| {
            row.get(0)
        })
        .expect("the cursor survives");
    assert_eq!(cursor, [1, 2]);

    assert_triggers_hold(&conn);
    let again = migrate(&mut conn).expect("reopens");
    assert!(!again.adopted && again.applied.is_empty());
}

#[test]
fn a_sea_orm_database_an_older_build_left_short_is_completed() {
    let mut conn = sea_orm_database(&ALL_SEAQL[..2]);
    let outcome = migrate(&mut conn).expect("adopts");
    assert!(outcome.adopted);
    assert_eq!(tables(&conn), CURRENT_TABLES);
    assert_eq!(triggers(&conn), CURRENT_TRIGGERS);
    assert!(has_column(&conn, "synced_assets", "retention_until").expect("reads"));
    assert_triggers_hold(&conn);
}

/// A refusal leaves every table, row and the stamp exactly as they were.
fn assert_untouched(conn: &Connection, tables_before: &[String], version: u32) {
    assert_eq!(tables(conn), tables_before);
    assert_eq!(read_user_version(conn).expect("reads"), version);
}

#[test]
fn a_sea_orm_database_listing_an_unknown_migration_is_refused_untouched() {
    let mut conn = sea_orm_database(ALL_SEAQL);
    conn.execute(
        "INSERT INTO seaql_migrations VALUES ('m20270101_000000_future', 9)",
        [],
    )
    .expect("lists a future migration");
    let before = tables(&conn);
    let error = migrate(&mut conn).expect_err("refused");
    assert!(
        matches!(&error, MigrationError::UnknownSeaOrmMigration { migration } if migration == "m20270101_000000_future"),
        "{error}"
    );
    assert_untouched(&conn, &before, 0);
}

#[test]
fn a_sea_orm_database_missing_a_listed_table_is_refused_untouched() {
    let mut conn = sea_orm_database(ALL_SEAQL);
    conn.execute_batch("DROP TABLE peering_floor;")
        .expect("drops");
    let before = tables(&conn);
    let error = migrate(&mut conn).expect_err("refused");
    assert!(
        matches!(&error, MigrationError::AdoptionIncomplete { missing, .. } if missing == "peering_floor"),
        "{error}"
    );
    assert_untouched(&conn, &before, 0);
}

#[test]
fn a_newer_database_is_refused_untouched() {
    let mut conn = memory();
    migrate(&mut conn).expect("migrates");
    conn.execute_batch(&format!("PRAGMA user_version = {};", SCHEMA_VERSION + 1))
        .expect("stamps");
    let before = tables(&conn);
    let error = migrate(&mut conn).expect_err("refused");
    assert!(
        matches!(error, MigrationError::TooNew { found, supported } if found == SCHEMA_VERSION + 1 && supported == SCHEMA_VERSION)
    );
    assert_untouched(&conn, &before, SCHEMA_VERSION + 1);
}

#[test]
fn an_unstamped_database_with_unknown_tables_is_refused_untouched() {
    let mut conn = memory();
    conn.execute_batch("CREATE TABLE stranger (x INTEGER);")
        .expect("creates");
    let error = migrate(&mut conn).expect_err("refused");
    assert!(matches!(error, MigrationError::Unrecognised { .. }));
    assert_untouched(&conn, &["stranger".to_owned()], 0);
}
