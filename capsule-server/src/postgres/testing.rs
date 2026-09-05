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
/// **`18`, and glibc rather than alpine**, which is a decision with two halves.
///
/// The major is the one a deployment runs: `capsule-server/compose.yaml` and `.env.example` ship
/// PostgreSQL 18, and design/filesystem/server.md records it. A harness that tested a different
/// major would be proving the adapters work against something nobody deploys.
///
/// The libc is load-bearing for one case. musl collates `en_US.utf8` byte-for-byte, so on an
/// alpine image `index/conformance.rs`'s `the_row_walk_orders_by_the_identifiers_own_bytes`
/// cannot tell a query that pins `COLLATE "C"` from one that inherits the database's collation —
/// it passes either way, which is worse than failing. glibc orders
/// `walkord-a-b, walkord-ab, walkord-a-c` where bytes order
/// `walkord-a-b, walkord-a-c, walkord-ab`, so on this image the case is an assertion. Bump the
/// tag deliberately, and keep it glibc.
const POSTGRES_TAG: &str = "18";

/// Where the container keeps its cluster.
///
/// Off the image's declared `VOLUME` deliberately. The data of a container that lives for one
/// test is throwaway by definition, so an anonymous volume buys nothing and costs two things: a
/// volume to create and reap per test, and — on a rootless runtime, where the volume is created
/// with an ownership the container's own user cannot `chmod` — an `initdb` that fails before
/// Postgres ever listens.
const PGDATA: &str = "/tmp/pgdata";

/// The user-namespace mode to run the container under, when the host needs one.
///
/// Unset on an ordinary Docker host and on CI, which is why this is an environment variable
/// rather than a constant: `keep-id` is podman's spelling and Docker rejects it.
///
/// It exists because of a real and non-obvious host shape. Under **rootless podman**, a
/// container process that is not the container's root maps to a host *subuid*, and that subuid
/// has to traverse the image store to reach anything — so on a machine whose home directory is
/// not world-traversable (`drwxrws---`), the official Postgres image dies at
/// `gosu postgres /usr/local/bin/docker-entrypoint.sh` with a bare "permission denied" that
/// names the entrypoint and says nothing about why. `--userns=keep-id` maps every container uid
/// onto the invoking user, which both fixes the traversal and makes the entrypoint skip its
/// drop-privileges branch entirely.
const USERNS_MODE: &str = "CAPSULE_TEST_CONTAINER_USERNS";

/// A running Postgres with the server's schema applied.
///
/// Holds the container: dropping this stops and removes it, so a case cannot leak one.
#[derive(Debug)]
pub(crate) struct TestDatabase {
    _container: ContainerAsync<Postgres>,
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
             reachable container runtime — for rootless podman, `systemctl --user start \
             podman.socket`, `export DOCKER_HOST=unix:///run/user/$(id -u)/podman/podman.sock` \
             and `export {USERNS_MODE}=keep-id`."
        );
        return None;
    }

    let mut request = testcontainers::ImageExt::with_tag(Postgres::default(), POSTGRES_TAG);
    request = testcontainers::ImageExt::with_env_var(request, "PGDATA", PGDATA);
    if let Ok(userns) = std::env::var(USERNS_MODE) {
        request = testcontainers::ImageExt::with_userns_mode(request, &userns);
    }
    let container = request.start().await.unwrap_or_else(|error| {
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
        url,
    })
}
