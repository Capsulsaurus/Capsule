//! The container harness the Postgres conformance suites run against.
//!
//! # The gate is load-bearing, not decorative
//!
//! `cargo nextest run` must be green on a machine with no container runtime — that is the
//! acceptance gap design/module-map.md sets for the framework, and it is what lets the rest of
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
//! each process would fill its own. Rather than pretend otherwise, each port contributes
//! exactly **one** container-backed test that runs its whole suite in one [`run_all`]-style
//! pass, which is what `index/conformance.rs` said a Postgres smoke test should be. Five
//! containers per run, serialized by the `containers` nextest group (`.config/nextest.toml`),
//! is a few seconds; thirty would not be.
//!
//! A fresh container per test also removes the isolation problem entirely: no schema
//! namespacing, no truncation between cases, and no case that passes because a previous one
//! left the database in a convenient state.

use sea_orm::DatabaseConnection;
use server_migration::MigratorTrait as _;
use testcontainers::ContainerAsync;
use testcontainers::runners::AsyncRunner as _;
use testcontainers_modules::postgres::Postgres;

/// The environment variable that admits the container-backed cases.
pub(crate) const GATE: &str = "CAPSULE_TEST_POSTGRES";

/// The image tag every run pins.
///
/// Pinned rather than left at the module's default (`11-alpine`) for two reasons: the server
/// targets a currently-supported PostgreSQL, and a floating tag makes "it passed yesterday"
/// unfalsifiable. Bump it deliberately.
const POSTGRES_TAG: &str = "18.0";

/// A running Postgres with the server's schema applied.
///
/// Holds the container: dropping this stops and removes it, so a case cannot leak one.
#[derive(Debug)]
pub(crate) struct TestDatabase {
    _container: ContainerAsync<Postgres>,
    connection: DatabaseConnection,
}

impl TestDatabase {
    /// The pool the adapter under test is built over.
    pub(crate) fn connection(&self) -> &DatabaseConnection {
        &self.connection
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
             reachable container runtime — for podman, `systemctl --user start podman.socket` \
             and `export DOCKER_HOST=unix:///run/user/$(id -u)/podman/podman.sock`."
        );
        return None;
    }

    let container = testcontainers::ImageExt::with_tag(Postgres::default(), POSTGRES_TAG)
        .start()
        .await
        .unwrap_or_else(|error| {
            panic!("{GATE}=1 was set but a Postgres container could not be started: {error}")
        });
    let host = container
        .get_host()
        .await
        .unwrap_or_else(|error| panic!("the container has no reachable host: {error}"));
    let port = container
        .get_host_port_ipv4(5432)
        .await
        .unwrap_or_else(|error| panic!("the container published no port: {error}"));
    // The module's own defaults; there is no secret here to keep out of a source file.
    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");

    let connection = super::connect(&url)
        .await
        .unwrap_or_else(|error| panic!("the test container refused a connection: {error}"));
    server_migration::Migrator::up(&connection, None)
        .await
        .unwrap_or_else(|error| panic!("the server's schema could not be applied: {error}"));

    Some(TestDatabase {
        _container: container,
        connection,
    })
}
