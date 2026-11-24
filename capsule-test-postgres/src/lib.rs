//! The Postgres test container that Capsule's container tiers start.
//!
//! One container per caller. [`start`] runs the PostgreSQL image under the container CLI
//! (`docker` unless [`CLI`] names another, such as `podman`), publishes 5432 on the loopback
//! interface, waits until the server accepts TCP connections, and hands back a
//! [`PostgresContainer`] that removes the container when it is dropped.
//!
//! It drives the CLI rather than a container API client because the tiers need very little:
//! one image, one published port, and `exec`. The CLI already does all three, on Docker and on
//! podman, and talking to it adds no dependency. The [`Runtime`] trait is the seam: [`Cli`] is
//! the real one, and the unit tests script a fake.
//!
//! # Readiness
//!
//! The official image starts PostgreSQL twice: once on a Unix socket only, to run its init
//! scripts, and then for real with TCP. So readiness is `pg_isready` over TCP *inside* the
//! container, polled until it answers or [`Request::ready_bound`] passes. Polling the published
//! port from the host instead would let a connection reach the init server's stop window.
//!
//! # Where the gate lives
//!
//! This crate starts containers; it does not decide whether a run may. Each tier reads
//! [`GATE`] itself and prints its own skip line, because what a skipped case should tell the
//! operator differs from tier to tier.

use std::future::Future;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};
use std::{fmt, io};

use tokio::io::AsyncWriteExt as _;

/// The environment variable that admits the container-backed cases.
pub const GATE: &str = "CAPSULE_TEST_POSTGRES";

/// The environment variable naming the container CLI. Unset, it is `docker`.
pub const CLI: &str = "CAPSULE_TEST_CONTAINER_CLI";

/// The environment variable naming a user-namespace mode for the container, when the host
/// needs one.
///
/// Unset on an ordinary Docker host and on CI. Under **rootless podman**, a container process
/// that is not the container's root maps to a host *subuid*, and that subuid has to traverse
/// the image store to reach anything. On a machine whose home directory is not
/// world-traversable, the official image then dies at its `gosu postgres` entrypoint with a bare
/// "permission denied". `keep-id` maps every container uid onto the invoking user, which fixes
/// the traversal and makes the entrypoint skip its drop-privileges branch. It is a variable
/// rather than a constant because `keep-id` is podman's spelling and Docker rejects it.
pub const USERNS_MODE: &str = "CAPSULE_TEST_CONTAINER_USERNS";

/// The image tag every tier pins.
///
/// **`18`, and glibc rather than alpine.** The major is the one a deployment runs
/// (`capsule-server/compose.yaml`). The libc matters to one case: musl collates `en_US.utf8`
/// byte for byte, so on alpine the index's byte-order case cannot tell a query that pins
/// `COLLATE "C"` from one that inherits the database's collation. Bump the tag deliberately,
/// and keep it glibc.
pub const POSTGRES_TAG: &str = "18";

/// The image, fully qualified so podman never has to resolve a short name.
pub const POSTGRES_IMAGE: &str = "docker.io/library/postgres";

/// The superuser the image creates.
pub const USER: &str = "postgres";

/// The superuser's password. A throwaway container's, so there is nothing here to keep secret.
pub const PASSWORD: &str = "postgres";

/// The database the image creates.
pub const DATABASE: &str = "postgres";

/// The port PostgreSQL listens on inside the container.
const CONTAINER_PORT: u16 = 5432;

/// Where the cluster lives inside the container.
///
/// Off the image's declared `VOLUME` deliberately. The data of a container that lives for one
/// test is throwaway, so an anonymous volume buys nothing and costs a volume to create and
/// reap per test. On a rootless runtime it also creates the volume with an ownership the
/// container's own user cannot `chmod`, so `initdb` fails before PostgreSQL ever listens.
const PGDATA: &str = "/tmp/pgdata";

/// The label every container this crate starts carries, so a leaked one can be found with
/// `docker ps --filter label=capsule.test=postgres`.
const LABEL: &str = "capsule.test=postgres";

/// How long [`start`] waits for PostgreSQL to accept connections before it gives up.
const READY_BOUND: Duration = Duration::from_secs(120);

/// How often [`start`] asks whether PostgreSQL is ready.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// What to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    tag: String,
    env: Vec<(String, String)>,
    userns: Option<String>,
    ready_bound: Duration,
    poll_interval: Duration,
}

impl Request {
    /// The pinned image with the superuser password and `PGDATA` set, and no user-namespace
    /// mode. Reads no environment.
    #[must_use]
    pub fn postgres() -> Self {
        Self {
            tag: POSTGRES_TAG.to_owned(),
            env: vec![
                ("POSTGRES_PASSWORD".to_owned(), PASSWORD.to_owned()),
                ("PGDATA".to_owned(), PGDATA.to_owned()),
            ],
            userns: None,
            ready_bound: READY_BOUND,
            poll_interval: POLL_INTERVAL,
        }
    }

    /// [`Request::postgres`], with the user-namespace mode [`USERNS_MODE`] names, if any.
    #[must_use]
    pub fn from_env() -> Self {
        Self::postgres().with_userns(std::env::var(USERNS_MODE).ok())
    }

    /// Run under `mode` as the user-namespace mode. `None` or an empty string leaves it unset.
    #[must_use]
    pub fn with_userns(mut self, mode: Option<String>) -> Self {
        self.userns = mode.filter(|mode| !mode.is_empty());
        self
    }

    /// Wait at most `bound` for PostgreSQL to accept connections, asking every `interval`.
    #[must_use]
    pub fn with_readiness(mut self, bound: Duration, interval: Duration) -> Self {
        self.ready_bound = bound;
        self.poll_interval = interval;
        self
    }

    /// How long [`start`] waits for PostgreSQL to accept connections.
    #[must_use]
    pub fn ready_bound(&self) -> Duration {
        self.ready_bound
    }

    /// The `run` arguments this request becomes.
    fn run_arguments(&self) -> Vec<String> {
        let mut arguments = vec![
            "run".to_owned(),
            "--detach".to_owned(),
            // The runtime removes the container once it stops. That covers the one leak `Drop`
            // cannot: a `run` that created the container and then failed to start it exits
            // non-zero before printing an id this crate could own.
            "--rm".to_owned(),
            "--label".to_owned(),
            LABEL.to_owned(),
            // Loopback only: a container with a well-known password is not published on every
            // interface of the host running the tests.
            "--publish".to_owned(),
            format!("127.0.0.1::{CONTAINER_PORT}"),
        ];
        for (key, value) in &self.env {
            arguments.push("--env".to_owned());
            arguments.push(format!("{key}={value}"));
        }
        if let Some(mode) = &self.userns {
            arguments.push(format!("--userns={mode}"));
        }
        arguments.push(format!("{POSTGRES_IMAGE}:{}", self.tag));
        arguments
    }
}

/// Why a container could not be started or driven.
#[derive(Debug, thiserror::Error)]
pub enum ContainerError {
    /// The container CLI could not be run at all.
    #[error("`{program}` could not be run ({source}); set {CLI} to the container CLI on PATH")]
    Spawn {
        /// The CLI that was run.
        program: String,
        /// Why it could not be.
        source: io::Error,
    },

    /// The container CLI started, but feeding it input or collecting its output failed.
    #[error("`{program}` started but its input or output failed: {source}")]
    Io {
        /// The CLI that was run.
        program: String,
        /// What failed.
        source: io::Error,
    },

    /// The container CLI ran and failed.
    #[error("`{command}` failed with {status}: {stderr}")]
    Failed {
        /// The command line, for the operator to rerun.
        command: String,
        /// How it exited.
        status: String,
        /// What it said.
        stderr: String,
    },

    /// `port` did not name a host port for 5432.
    #[error("the container published no host port for {CONTAINER_PORT}: {output:?}")]
    Port {
        /// What `port` printed.
        output: String,
    },

    /// PostgreSQL did not accept connections within the bound.
    #[error("PostgreSQL was not ready after {waited:?}; the last check said: {last}")]
    NotReady {
        /// How long [`start`] waited.
        waited: Duration,
        /// What the last `pg_isready` printed.
        last: String,
    },
}

/// The container CLI, as the one seam this crate's logic is tested through.
pub trait Runtime: Send + Sync {
    /// Run the CLI with `arguments`, feeding it `stdin` when given, and collect its output.
    fn run(
        &self,
        arguments: Vec<String>,
        stdin: Option<Vec<u8>>,
    ) -> impl Future<Output = Result<Output, ContainerError>> + Send;

    /// Remove container `id`, blocking until the CLI returns. Best effort: called from `Drop`,
    /// where a failure has nowhere to go but the log.
    fn remove(&self, id: &str);
}

/// The container CLI on `PATH`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    program: String,
}

impl Cli {
    /// The CLI named `program`.
    #[must_use]
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
        }
    }

    /// The CLI [`CLI`] names, or `docker`.
    #[must_use]
    pub fn from_env() -> Self {
        Self::new(program_from(std::env::var(CLI).ok()))
    }

    /// The program this runs.
    #[must_use]
    pub fn program(&self) -> &str {
        &self.program
    }
}

/// The CLI to run, given the value of [`CLI`].
fn program_from(configured: Option<String>) -> String {
    configured
        .filter(|program| !program.trim().is_empty())
        .unwrap_or_else(|| "docker".to_owned())
}

impl Runtime for Cli {
    async fn run(
        &self,
        arguments: Vec<String>,
        stdin: Option<Vec<u8>>,
    ) -> Result<Output, ContainerError> {
        let spawn_error = |source| ContainerError::Spawn {
            program: self.program.clone(),
            source,
        };
        let io_error = |source| ContainerError::Io {
            program: self.program.clone(),
            source,
        };
        let mut command = tokio::process::Command::new(&self.program);
        command
            .args(&arguments)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(spawn_error)?;
        if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
            pipe.write_all(&bytes).await.map_err(io_error)?;
            // Dropping the pipe is the end of input the command waits for.
            drop(pipe);
        }
        child.wait_with_output().await.map_err(io_error)
    }

    fn remove(&self, id: &str) {
        let removed = std::process::Command::new(&self.program)
            .args(["rm", "--force", "--volumes", id])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output();
        match removed {
            Ok(output) if output.status.success() => {
                tracing::debug!(container = id, "removed the test container");
            }
            Ok(output) => tracing::warn!(
                container = id,
                stderr = %String::from_utf8_lossy(&output.stderr).trim(),
                "the test container could not be removed"
            ),
            Err(error) => tracing::warn!(
                container = id,
                %error,
                "the container CLI could not be run to remove the test container"
            ),
        }
    }
}

/// A running PostgreSQL, removed when this is dropped.
pub struct PostgresContainer<R: Runtime = Cli> {
    runtime: R,
    id: String,
    port: u16,
}

impl<R: Runtime> fmt::Debug for PostgresContainer<R> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgresContainer")
            .field("id", &self.id)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

/// Start a PostgreSQL under the container CLI [`CLI`] names, as `request` describes.
///
/// # Errors
///
/// Returns a [`ContainerError`] when the CLI cannot be run, the container cannot be started,
/// it publishes no port, or PostgreSQL is not ready within [`Request::ready_bound`]. A
/// container that was created before the failure is removed.
pub async fn start(request: &Request) -> Result<PostgresContainer, ContainerError> {
    start_with(Cli::from_env(), request).await
}

/// [`start`], under `runtime`.
///
/// # Errors
///
/// As [`start`].
pub async fn start_with<R: Runtime>(
    runtime: R,
    request: &Request,
) -> Result<PostgresContainer<R>, ContainerError> {
    let started = Instant::now();
    let output = checked(&runtime, request.run_arguments(), None).await?;
    let id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    tracing::info!(container = %id, tag = %request.tag, "started a PostgreSQL test container");
    // Owned from here, so every failure below removes the container on its way out.
    let mut container = PostgresContainer {
        runtime,
        id,
        port: 0,
    };

    let published = checked(
        &container.runtime,
        vec![
            "port".to_owned(),
            container.id.clone(),
            format!("{CONTAINER_PORT}/tcp"),
        ],
        None,
    )
    .await?;
    container.port = parse_port(&String::from_utf8_lossy(&published.stdout))?;

    container.wait_until_ready(request).await?;
    tracing::info!(
        container = %container.id,
        port = container.port,
        elapsed_ms = started.elapsed().as_millis(),
        "the PostgreSQL test container is ready"
    );
    Ok(container)
}

impl<R: Runtime> PostgresContainer<R> {
    /// The host the published port is on.
    #[must_use]
    pub fn host(&self) -> &'static str {
        "127.0.0.1"
    }

    /// The host port 5432 is published on.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The connection string of `database`, as the superuser.
    #[must_use]
    pub fn url(&self, database: &str) -> String {
        format!(
            "postgres://{USER}:{PASSWORD}@{}:{}/{database}",
            self.host(),
            self.port
        )
    }

    /// Run `command` inside the container and collect its output, whatever its exit status.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerError::Spawn`] when the CLI cannot be run.
    pub async fn exec(&self, command: &[&str]) -> Result<Output, ContainerError> {
        self.runtime
            .run(self.exec_arguments(false, command), None)
            .await
    }

    /// Write `bytes` to `path` inside the container.
    ///
    /// # Errors
    ///
    /// Returns a [`ContainerError`] when the CLI cannot be run or the write fails.
    pub async fn copy_in(&self, path: &str, bytes: Vec<u8>) -> Result<(), ContainerError> {
        // `$1` rather than an interpolated path, so the path is never parsed as shell.
        let arguments = self.exec_arguments(true, &["sh", "-c", "cat > \"$1\"", "sh", path]);
        checked(&self.runtime, arguments, Some(bytes))
            .await
            .map(drop)
    }

    fn exec_arguments(&self, interactive: bool, command: &[&str]) -> Vec<String> {
        let mut arguments = vec!["exec".to_owned()];
        if interactive {
            arguments.push("--interactive".to_owned());
        }
        arguments.push(self.id.clone());
        arguments.extend(command.iter().map(|&part| part.to_owned()));
        arguments
    }

    /// Ask `pg_isready` over TCP until it says yes or the request's bound passes.
    async fn wait_until_ready(&self, request: &Request) -> Result<(), ContainerError> {
        let started = Instant::now();
        let probe = ["pg_isready", "--host", "127.0.0.1", "--username", USER];
        loop {
            let output = self.exec(&probe).await?;
            if output.status.success() {
                return Ok(());
            }
            let last = describe(&output);
            tracing::trace!(container = %self.id, %last, "PostgreSQL is not ready yet");
            if started.elapsed() >= request.ready_bound {
                return Err(ContainerError::NotReady {
                    waited: started.elapsed(),
                    last,
                });
            }
            tokio::time::sleep(request.poll_interval).await;
        }
    }
}

impl<R: Runtime> Drop for PostgresContainer<R> {
    fn drop(&mut self) {
        self.runtime.remove(&self.id);
    }
}

/// Run `arguments` and turn a non-zero exit into [`ContainerError::Failed`].
async fn checked<R: Runtime>(
    runtime: &R,
    arguments: Vec<String>,
    stdin: Option<Vec<u8>>,
) -> Result<Output, ContainerError> {
    let command = arguments.join(" ");
    let output = runtime.run(arguments, stdin).await?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(ContainerError::Failed {
            command,
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}

/// The host port in `port`'s output: its first line, `HOST:PORT`.
fn parse_port(output: &str) -> Result<u16, ContainerError> {
    output
        .lines()
        .next()
        .and_then(|line| line.trim().rsplit_once(':'))
        .and_then(|(_, port)| port.parse().ok())
        .ok_or_else(|| ContainerError::Port {
            output: output.to_owned(),
        })
}

/// What a probe printed, for the error that reports it.
fn describe(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("{} {}", stdout.trim(), stderr.trim())
        .trim()
        .to_owned()
}

#[cfg(test)]
mod tests;
