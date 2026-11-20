//! The container harness the Postgres conformance suites run against.
//!
//! # The gate is load-bearing, not decorative
//!
//! `cargo nextest run` must be green on a machine with no container runtime — that is the
//! framework's acceptance gap, and it is what lets the rest of
//! the rebuild be tested without live infrastructure. So every Postgres case asks [`start`]
//! first, and [`start`] returns `None` — after printing why — unless `CAPSULE_TEST_POSTGRES=1`
//! is set.
//!
//! A **skip that says nothing** is the failure mode this is written against: a suite that
//! silently runs zero cases reads exactly like a suite that passes. Every skip prints one line
//! naming the case and how to run it.
//!
//! # One container per test, and why that is the right trade here
//!
//! nextest runs a process per test, so a `OnceCell` shared between cases would buy nothing —
//! each process would fill its own. Rather than pretend otherwise, most ports contribute one
//! container-backed test that runs their whole suite in one [`run_all`]-style pass, which is
//! what `index/conformance.rs` said a Postgres smoke test should be; a few (boot, the index, the
//! migration checks in `postgres/tests.rs`) add targeted cases. The count therefore grows with
//! the ports, and every one of those containers is serialized by the `containers` nextest group
//! (`.config/nextest.toml`).
//!
//! A fresh container per test also removes the isolation problem entirely: no schema
//! namespacing, no truncation between cases, and no case that passes because a previous one
//! left the database in a convenient state.

use capsule_test_postgres::{CLI, DATABASE, PostgresContainer, Request, USERNS_MODE};
use sea_orm::DatabaseConnection;
use server_migration::MigratorTrait as _;

/// The environment variable that admits the container-backed cases.
pub(crate) const GATE: &str = capsule_test_postgres::GATE;

/// A running Postgres with the server's schema applied.
///
/// Holds the container: dropping this stops and removes it, so a case cannot leak one.
#[derive(Debug)]
pub(crate) struct TestDatabase {
    _container: PostgresContainer,
    connection: DatabaseConnection,
    url: String,
}

impl TestDatabase {
    /// The pool the adapter under test is built over.
    pub(crate) fn connection(&self) -> &DatabaseConnection {
        &self.connection
    }

    /// The connection string, for a case that has to drive a boot path rather than an adapter.
    pub(crate) fn url(&self) -> &str {
        &self.url
    }

    /// Undo every migration, leaving a reachable database with no schema.
    ///
    /// What `boot`'s unmigrated case needs: a database that answers and has nothing in it is the
    /// state a deployment is in between `docker compose up` and the migration command, and it is
    /// exactly the state `assert_schema_current` exists to refuse.
    ///
    /// # Panics
    ///
    /// Panics if the rollback fails; a harness that cannot reach the state the case is about has
    /// nothing useful to report.
    pub(crate) async fn roll_back(&self) {
        server_migration::Migrator::down(&self.connection, None)
            .await
            .unwrap_or_else(|error| panic!("the schema could not be rolled back: {error}"));
    }
}

/// Whether the container-backed cases are admitted.
fn enabled() -> bool {
    matches!(std::env::var(GATE).as_deref(), Ok("1"))
}

/// Start a Postgres for `case`, or explain why it was skipped.
///
/// `None` is a **skip**, and the caller returns rather than failing: the whole point of the gate
/// is that a run with no container runtime is green.
///
/// # Panics
///
/// Panics if the gate is set and the container cannot be started or migrated. An operator who
/// asked for the Postgres tier and did not get it must be told, not quietly skipped — that would
/// make the gate a way to hide a broken adapter.
pub(crate) async fn start(case: &str) -> Option<TestDatabase> {
    if !enabled() {
        eprintln!(
            "skipping {case}: the Postgres conformance tier is unavailable. Set {GATE}=1 with a \
             container CLI on PATH (`docker`, or name another in {CLI}); for rootless podman, \
             `export {CLI}=podman` and `export {USERNS_MODE}=keep-id`."
        );
        return None;
    }

    // The image, its tag and where it keeps its cluster, and why each is what it is, live in
    // `capsule_test_postgres`, which every container tier shares.
    let container = capsule_test_postgres::start(&Request::from_env())
        .await
        .unwrap_or_else(|error| {
            panic!("{GATE}=1 was set but a Postgres container could not be started: {error}")
        });
    let url = container.url(DATABASE);

    let connection = super::connect(&url)
        .await
        .unwrap_or_else(|error| panic!("the test container refused a connection: {error}"));
    server_migration::Migrator::up(&connection, None)
        .await
        .unwrap_or_else(|error| panic!("the server's schema could not be applied: {error}"));

    Some(TestDatabase {
        _container: container,
        connection,
        url,
    })
}
