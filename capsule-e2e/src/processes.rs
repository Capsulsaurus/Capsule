//! The real `capsule` and `capsule-server` binaries, driven as processes that can be
//! `SIGKILL`ed — the kill-anywhere suite's half of the harness (`tests/kill_anywhere.rs`).
//!
//! The rest of this crate runs the server's composition root in-process, which is the right
//! shape for asserting what the server *decides*. It cannot say what survives a process dying
//! mid-write: an in-process server cannot be killed without killing the test, and an in-process
//! client shares the test's memory, so state that merely survived in a `HashMap` is
//! indistinguishable from state that reached disk. Two processes can tell them apart.
//!
//! # Where the binaries come from
//!
//! A test of this crate has no `CARGO_BIN_EXE_*` for another package's binaries, and a stale
//! binary left in the target directory would make a passing run prove an old build.
//! [`Binaries::build`] therefore runs `cargo build --bins -p capsule-cli -p capsule-server`
//! into the target directory and profile the running test was built for — a no-op when they
//! are current — unless `CAPSULE_E2E_BIN_DIR` names a directory holding both already, which is
//! what a CI job that built them in an earlier step sets.
//!
//! # Every wait is bounded
//!
//! A server that never reports its address, and a CLI that never exits, each fail the test
//! with the child's own log after a stated bound rather than hanging the suite. Every child's
//! stdout and stderr go to files under a log directory the caller owns, so nothing blocks on a
//! full pipe and a failure can quote what the process said.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// The variable naming a directory that already holds both binaries.
pub const BIN_DIR_VAR: &str = "CAPSULE_E2E_BIN_DIR";

/// The variable whose value, when set, is the spawned CLI's `RUST_LOG` — for diagnosing a
/// failure, knowing that a `--json` run's stdout then carries log lines too (#793).
pub const CLI_LOG_VAR: &str = "CAPSULE_E2E_CLI_LOG";

/// The variable whose value, when set, is the spawned server's `RUST_LOG` (default `warn`).
pub const SERVER_LOG_VAR: &str = "CAPSULE_E2E_SERVER_LOG";

/// How long a spawned server may take to report its bound address. Generous: a debug build
/// pays for an Argon2id decoy hash and the Postgres pool before it binds.
pub const BIND_TIMEOUT: Duration = Duration::from_secs(60);

/// How many times a restart on the old port is attempted before the test fails. A port a
/// killed process held can take a moment to be bindable again.
pub const RESTART_ATTEMPTS: u32 = 20;

/// How often a bounded wait checks on its child.
const POLL: Duration = Duration::from_millis(10);

/// Every server setting the binary reads, removed before a spawn sets its own: the test
/// runner's environment is not the test's to trust (`capsule-server/tests/binary.rs`).
const SERVER_SETTINGS: &[&str] = &[
    "BLOB_ROOT",
    "UPLOAD_DIR",
    "DATABASE_URL",
    "VALKEY_URL",
    "JWT_ED25519_DER",
    "SYNC_CURSOR_MAC_KEY",
    "ATTESTATION_KEY_SEED",
    "SERVER_HOST",
    "SERVER_PORT",
    "SERVER_DOMAIN",
    "API_BASE_URL",
    "CAPSULE_PROFILE",
    "PROTOCOL_MIN",
    "PROTOCOL_MAX",
    "GC_GRACE_WINDOW_HOURS",
    "SHUTDOWN_TIMEOUT_SECONDS",
    "MAX_CONNECTIONS",
    "LOG_FORMAT",
    "TRUSTED_PROXY_HOPS",
    "TRUSTED_PROXIES",
    "TRUSTED_PROXY_HEADER",
    "HEADER_READ_TIMEOUT_SECONDS",
    "CHUNK_STALL_TIMEOUT_SECONDS",
    "CHUNK_MIN_RATE_BYTES_PER_SECOND",
    "CHUNK_READ_TIMEOUT_SECONDS",
];

/// Every endpoint override the CLI reads, removed so only `CAPSULE_ENDPOINT` decides.
const CLI_SETTINGS: &[&str] = &[
    "CAPSULE_ENDPOINT",
    "CAPSULE_AUTH_ENDPOINT",
    "CAPSULE_SYNC_ENDPOINT",
    "CAPSULE_UPLOAD_ENDPOINT",
    "CAPSULE_ALBUMS_ENDPOINT",
    "CAPSULE_PROTOCOL",
];

/// The two binaries under test.
#[derive(Debug, Clone)]
pub struct Binaries {
    /// `capsule`.
    pub cli: PathBuf,
    /// `capsule-server`.
    pub server: PathBuf,
}

impl Binaries {
    /// Build both binaries for the running test's target directory and profile, or take them
    /// from [`BIN_DIR_VAR`] when it is set. See the module docs.
    ///
    /// # Panics
    ///
    /// When the build fails, or a binary is missing afterwards.
    #[must_use]
    pub fn build() -> Self {
        if let Some(dir) = std::env::var_os(BIN_DIR_VAR) {
            return Self::in_dir(Path::new(&dir));
        }
        let exe = std::env::current_exe().expect("the test knows its own path");
        // `<target>/<profile>/deps/<test>-<hash>`.
        let profile_dir = exe
            .parent()
            .and_then(Path::parent)
            .expect("a test binary lives under <target>/<profile>/deps");
        let target_dir = profile_dir
            .parent()
            .expect("a profile directory lives under the target directory");
        let profile = match profile_dir.file_name().and_then(OsStr::to_str) {
            Some("debug") | None => "dev",
            Some(other) => other,
        };
        let cargo = std::env::var_os("CARGO")
            .unwrap_or_else(|| option_env!("CARGO").unwrap_or("cargo").into());
        tracing::info!(profile, target = %target_dir.display(), "building the binaries");
        let status = Command::new(cargo)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .args([
                "build",
                "--bins",
                "-p",
                "capsule-cli",
                "-p",
                "capsule-server",
            ])
            .args(["--profile", profile])
            .arg("--target-dir")
            .arg(target_dir)
            .status()
            .expect("cargo runs");
        assert!(
            status.success(),
            "building capsule and capsule-server failed"
        );
        Self::in_dir(profile_dir)
    }

    fn in_dir(dir: &Path) -> Self {
        let found = |name: &str| {
            let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
            assert!(
                path.is_file(),
                "{} is not a file; build it or unset {BIN_DIR_VAR}",
                path.display()
            );
            path
        };
        Self {
            cli: found("capsule"),
            server: found("capsule-server"),
        }
    }
}

/// A `capsule-server serve` process over durable backends, restartable on the same port.
pub struct ServerProcess {
    bin: PathBuf,
    blob_root: PathBuf,
    environment: Vec<(String, String)>,
    log_dir: PathBuf,
    /// `127.0.0.1:PORT`, fixed by the first spawn.
    listen: String,
    base_url: String,
    child: Option<Child>,
    generation: u32,
}

impl ServerProcess {
    /// Spawn `serve` on an ephemeral port with `environment` (the backend URLs and key
    /// material) and the blob root, and wait for it to say where it listens.
    ///
    /// # Panics
    ///
    /// When the server does not report an address within [`BIND_TIMEOUT`].
    #[must_use]
    pub fn spawn(
        bin: &Path,
        blob_root: &Path,
        environment: Vec<(String, String)>,
        log_dir: &Path,
    ) -> Self {
        let mut server = Self {
            bin: bin.to_owned(),
            blob_root: blob_root.to_owned(),
            environment,
            log_dir: log_dir.to_owned(),
            listen: "127.0.0.1:0".to_owned(),
            base_url: String::new(),
            child: None,
            generation: 0,
        };
        let base_url = server
            .start()
            .unwrap_or_else(|error| panic!("the server did not start: {error}"));
        server.listen = base_url.trim_start_matches("http://").to_owned();
        server.base_url = base_url;
        server
    }

    /// The API root (`http://127.0.0.1:PORT`), the same across restarts.
    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// `SIGKILL` the server: no drain, no shutdown hook. Returns whether it was still running.
    pub fn kill(&mut self) -> bool {
        let Some(mut child) = self.child.take() else {
            return false;
        };
        let running = child.try_wait().ok().flatten().is_none();
        let _ = child.kill();
        let _ = child.wait();
        tracing::info!(generation = self.generation, running, "server SIGKILLed");
        running
    }

    /// Start a new server process on the port the first one bound, retrying while the port is
    /// not yet free.
    ///
    /// # Panics
    ///
    /// After [`RESTART_ATTEMPTS`] failed starts.
    pub fn restart(&mut self) {
        self.kill();
        let mut last = String::new();
        for attempt in 1..=RESTART_ATTEMPTS {
            match self.start() {
                Ok(url) => {
                    assert_eq!(url, self.base_url, "a restart binds the same address");
                    tracing::info!(attempt, generation = self.generation, "server restarted");
                    return;
                }
                Err(error) => {
                    tracing::debug!(attempt, %error, "server restart failed; retrying");
                    last = error;
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
        panic!("the server did not restart on {}: {last}", self.listen);
    }

    /// One spawn, returning the URL the process reported or why it did not.
    fn start(&mut self) -> Result<String, String> {
        self.generation += 1;
        let stderr_path = self
            .log_dir
            .join(format!("server-{}.stderr", self.generation));
        let stderr = File::create(&stderr_path).map_err(|error| error.to_string())?;
        let root = self.blob_root.display().to_string();
        let mut command = Command::new(&self.bin);
        for key in SERVER_SETTINGS {
            command.env_remove(key);
        }
        command
            .args(["serve", "--listen", &self.listen, "--blob-root", &root])
            .envs(self.environment.iter().map(|(k, v)| (k, v)))
            .env(
                "RUST_LOG",
                std::env::var(SERVER_LOG_VAR).unwrap_or_else(|_| "warn".to_owned()),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(stderr);
        let mut child = command.spawn().map_err(|error| error.to_string())?;
        let stdout = child.stdout.take().expect("stdout is piped");
        let (sender, receiver) = mpsc::channel();
        // Reads the address line, then keeps draining so the server never blocks on stdout.
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Some(url) = line.strip_prefix("listening on ") {
                    let _ = sender.send(url.to_owned());
                }
            }
        });
        let deadline = Instant::now() + BIND_TIMEOUT;
        loop {
            if let Ok(url) = receiver.recv_timeout(POLL * 10) {
                self.child = Some(child);
                return Ok(url);
            }
            if let Ok(Some(status)) = child.try_wait() {
                let said = std::fs::read_to_string(&stderr_path).unwrap_or_default();
                return Err(format!("it exited with {status} before binding: {said}"));
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                let said = std::fs::read_to_string(&stderr_path).unwrap_or_default();
                return Err(format!("no address within {BIND_TIMEOUT:?}: {said}"));
            }
        }
    }
}

impl Drop for ServerProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

/// One user's `capsule` installation: its own home (so its own session file and data
/// directory) pointed at one server.
pub struct Cli {
    bin: PathBuf,
    endpoint: String,
    home: TempDir,
    log_dir: PathBuf,
    runs: u32,
}

impl Cli {
    /// A CLI whose home is a fresh temp directory and whose server is `endpoint`.
    #[must_use]
    pub fn new(bin: &Path, endpoint: &str, log_dir: &Path) -> Self {
        Self {
            bin: bin.to_owned(),
            endpoint: endpoint.to_owned(),
            home: tempfile::tempdir().expect("a temp CLI home"),
            log_dir: log_dir.to_owned(),
            runs: 0,
        }
    }

    /// Start `capsule ARGS`, writing `stdin` to it and closing it.
    ///
    /// # Panics
    ///
    /// When the process cannot be spawned.
    pub fn spawn<S: AsRef<OsStr>>(&mut self, args: &[S], stdin: &str) -> CliRun {
        self.runs += 1;
        let stdout_path = self.log_dir.join(format!("cli-{}.stdout", self.runs));
        let stderr_path = self.log_dir.join(format!("cli-{}.stderr", self.runs));
        let home = self.home.path();
        let mut command = Command::new(&self.bin);
        for key in CLI_SETTINGS {
            command.env_remove(key);
        }
        command
            .args(args)
            .env("CAPSULE_ENDPOINT", &self.endpoint)
            .env("HOME", home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("XDG_CACHE_HOME", home.join("cache"))
            .env("NO_COLOR", "1")
            // Off unless asked for: the CLI writes its log lines to stdout (#793), which puts
            // them inside the one JSON document `--json` prints there.
            .env(
                "RUST_LOG",
                std::env::var(CLI_LOG_VAR).unwrap_or_else(|_| "off".to_owned()),
            )
            .stdin(Stdio::piped())
            .stdout(File::create(&stdout_path).expect("the stdout log opens"))
            .stderr(File::create(&stderr_path).expect("the stderr log opens"));
        let mut child = command.spawn().expect("capsule spawns");
        let mut pipe = child.stdin.take().expect("stdin is piped");
        // A child killed before it read stdin closes the pipe; that is not this write's failure.
        let _ = pipe.write_all(stdin.as_bytes());
        drop(pipe);
        let described: Vec<String> = args
            .iter()
            .map(|a| a.as_ref().to_string_lossy().into_owned())
            .collect();
        tracing::debug!(run = self.runs, args = ?described, "capsule spawned");
        CliRun {
            child,
            args: described,
            stdout_path,
            stderr_path,
            started: Instant::now(),
        }
    }

    /// Run `capsule ARGS` to completion within `bound`.
    ///
    /// # Panics
    ///
    /// See [`CliRun::wait`].
    pub fn run<S: AsRef<OsStr>>(&mut self, args: &[S], stdin: &str, bound: Duration) -> CliOutput {
        self.spawn(args, stdin).wait(bound)
    }
}

/// A running `capsule` process.
pub struct CliRun {
    child: Child,
    args: Vec<String>,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
    started: Instant,
}

impl CliRun {
    /// How long ago the process started.
    #[must_use]
    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Whether the process has not exited yet.
    pub fn is_running(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    /// `SIGKILL` the process. Returns whether it was still running, i.e. whether the kill cut
    /// anything.
    pub fn kill(&mut self) -> bool {
        let running = self.is_running();
        let _ = self.child.kill();
        running
    }

    /// Wait for the process to exit, within `bound`.
    ///
    /// # Panics
    ///
    /// When it has not exited within `bound`: it is killed, and the panic quotes its stderr.
    #[must_use]
    pub fn wait(mut self, bound: Duration) -> CliOutput {
        let deadline = Instant::now() + bound;
        let status = loop {
            if let Some(status) = self.child.try_wait().expect("the child is waitable") {
                break status;
            }
            if Instant::now() >= deadline {
                let _ = self.child.kill();
                let _ = self.child.wait();
                let said = std::fs::read_to_string(&self.stderr_path).unwrap_or_default();
                panic!(
                    "capsule {:?} did not exit within {bound:?}; stderr:\n{said}",
                    self.args
                );
            }
            std::thread::sleep(POLL);
        };
        let output = CliOutput {
            args: self.args,
            status,
            stdout: std::fs::read_to_string(&self.stdout_path).unwrap_or_default(),
            stderr: std::fs::read_to_string(&self.stderr_path).unwrap_or_default(),
            elapsed: self.started.elapsed(),
        };
        tracing::debug!(args = ?output.args, status = %output.status, elapsed = ?output.elapsed, "capsule exited");
        output
    }
}

/// What a finished `capsule` process left.
#[derive(Debug)]
pub struct CliOutput {
    /// The arguments it ran with.
    pub args: Vec<String>,
    /// How it exited.
    pub status: ExitStatus,
    /// Everything it wrote to stdout.
    pub stdout: String,
    /// Everything it wrote to stderr.
    pub stderr: String,
    /// How long it ran.
    pub elapsed: Duration,
}

impl CliOutput {
    /// Assert it exited zero, quoting its output otherwise.
    ///
    /// # Panics
    ///
    /// When it did not.
    #[must_use]
    pub fn success(self) -> Self {
        assert!(
            self.status.success(),
            "capsule {:?} exited {}\nstdout:\n{}\nstderr:\n{}",
            self.args,
            self.status,
            self.stdout,
            self.stderr
        );
        self
    }

    /// stdout as the one JSON document a `--json` command prints.
    ///
    /// # Panics
    ///
    /// When stdout is not JSON.
    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_str(&self.stdout).unwrap_or_else(|error| {
            panic!(
                "capsule {:?} printed no JSON document ({error}):\n{}\nstderr:\n{}",
                self.args, self.stdout, self.stderr
            )
        })
    }
}
