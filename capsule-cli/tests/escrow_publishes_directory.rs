//! The user guide's set-up order, driven through the `capsule` binary alone: `auth register`,
//! `auth login`, `library init`, `auth escrow`, `import`, then `push` reaches verified custody
//! with no `auth recover` in between (#920).
//!
//! The server only ever accepts an upload from a device the account's published device
//! directory lists. Before #920 nothing on this path published one — only `auth recover` did —
//! so `push` refused every asset with `error.upload.device_not_authorized`. `auth escrow` now
//! joins the directory once it has bound the library, and this test is the acceptance for it.
//! `capsule-e2e`'s own harness publishes the directory in-process before any CLI command runs,
//! which is why it never saw the gap; nothing here touches the library or the server except
//! through a spawned `capsule`.
//!
//! # The two harness pieces
//!
//! - The server is `capsule-e2e`'s [`Server`]: the composition root `serve --memory` runs, bound
//!   in-process on an ephemeral port. Each `capsule` below is pointed at it with
//!   `CAPSULE_ENDPOINT` and given a scratch home, so no run reads the developer's session.
//! - `auth escrow` confirms its recovery secret on a prompt that refuses anything but a
//!   terminal, so that run gets a pseudo-terminal: its stdout and stderr are the PTY's slave,
//!   which is also its controlling terminal, and the test reads the secret off the master and
//!   types it back. The library passphrase still arrives on a piped stdin
//!   (`--passphrase-stdin`). The PTY calls are POSIX, so the file is unix only.
//!
//! Every wait is bounded: a run that does not finish within [`CLI_BOUND`] is killed and fails
//! the test with what it printed. The library is created at the CLI's own Argon2id cost, which
//! is what a user gets, and costs a few seconds per unlock in a debug build.
#![cfg(unix)]

use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use capsule_e2e::Server;
use serde_json::Value;

const PASSWORD: &str = "escrow-publishes-directory-password";
const PASSPHRASE: &str = "escrow-publishes-directory-passphrase";

/// How long one `capsule` run may take before the test kills it and fails. Generous: a debug
/// build pays a full Argon2id unlock per run, and escrow pays a second one to wrap the master key.
const CLI_BOUND: Duration = Duration::from_secs(120);

/// How often a bounded wait checks on its child.
const POLL: Duration = Duration::from_millis(20);

/// The prompt `auth escrow` reads the typed-back secret on.
const TYPE_BACK_PROMPT: &str = "Type the recovery secret back";

/// A temp directory that removes itself, so the test needs no `tempfile` dev-dependency.
struct ScratchDir(PathBuf);

impl ScratchDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("capsule-cli-scratch-{}", nanoid::nanoid!()));
        std::fs::create_dir_all(&path).expect("the scratch dir is created");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// One `capsule` installation: a scratch home pointed at one server.
struct Cli {
    home: PathBuf,
    endpoint: String,
}

/// What a finished run left.
struct Run {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl Cli {
    /// `capsule ARGS` with every directory the CLI resolves under the scratch home, the server
    /// endpoint set, and nothing inherited that would redirect it elsewhere.
    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_capsule"));
        for key in [
            "CAPSULE_AUTH_ENDPOINT",
            "CAPSULE_SYNC_ENDPOINT",
            "CAPSULE_UPLOAD_ENDPOINT",
            "CAPSULE_ALBUMS_ENDPOINT",
        ] {
            command.env_remove(key);
        }
        command
            .args(args)
            .env("CAPSULE_ENDPOINT", &self.endpoint)
            .env("NO_COLOR", "1")
            // The binary's fmt subscriber writes to stdout; a `--json` run's stdout is the
            // document under assertion.
            .env("RUST_LOG", "off")
            .env("LC_ALL", "en_US.UTF-8")
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join("config"))
            .env("XDG_DATA_HOME", self.home.join("data"))
            .env("XDG_CACHE_HOME", self.home.join("cache"));
        command
    }

    /// Run `capsule ARGS` to completion with `stdin` written and closed, within [`CLI_BOUND`].
    fn run(&self, args: &[&str], stdin: &str) -> Run {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("capsule spawns");
        write_stdin(&mut child, stdin);
        let status = wait_bounded(&mut child, args);
        let output = child.wait_with_output().expect("the finished run's output");
        Run {
            status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }

    /// [`Cli::run`], failing the test unless it exits 0.
    fn ok(&self, args: &[&str], stdin: &str) -> Run {
        let run = self.run(args, stdin);
        assert!(
            run.status.success(),
            "capsule {args:?} exited {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
            run.status,
            run.stdout,
            run.stderr
        );
        run
    }

    /// `capsule auth escrow --library LIBRARY --passphrase-stdin` over a pseudo-terminal: read
    /// the secret it shows, type it back at its prompt, and return everything it printed once
    /// it exits 0.
    fn escrow(&self, library: &Path) -> String {
        let (master, slave) = open_pty();
        let args = [
            "auth",
            "escrow",
            "--library",
            library.to_str().expect("a UTF-8 library path"),
            "--passphrase-stdin",
        ];
        let mut command = self.command(&args);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::from(slave.try_clone().expect("the PTY slave dups")))
            .stderr(Stdio::from(slave.try_clone().expect("the PTY slave dups")));
        // SAFETY: runs in the forked child before exec and calls only async-signal-safe
        // functions: a new session, then the slave (its stdout by now) as its controlling
        // terminal, which is what the prompt opens as `/dev/tty`.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::ioctl(1, libc::TIOCSCTTY, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().expect("capsule auth escrow spawns");
        // The command keeps the slave dups it was handed until it is dropped: only the child
        // may hold the slave, so the master reads end once it exits.
        drop(command);
        drop(slave);
        write_stdin(&mut child, &format!("{PASSPHRASE}\n"));

        let mut writer = File::from(master.try_clone().expect("the PTY master dups"));
        let screen = read_screen(File::from(master));
        let deadline = Instant::now() + CLI_BOUND;
        let mut printed = String::new();
        let mut typed = false;
        loop {
            match screen.recv_timeout(POLL) {
                Ok(chunk) => printed.push_str(&chunk),
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if !typed
                && printed.contains(TYPE_BACK_PROMPT)
                && let Some(secret) = shown_secret(&printed)
            {
                writer
                    .write_all(format!("{secret}\r").as_bytes())
                    .expect("the secret is typed back");
                typed = true;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("capsule auth escrow did not finish within {CLI_BOUND:?}:\n{printed}");
            }
        }
        let status = wait_bounded(&mut child, &args);
        assert!(
            typed,
            "auth escrow never showed its secret and prompt:\n{printed}"
        );
        assert!(
            status.success(),
            "capsule auth escrow exited {status}:\n{printed}"
        );
        printed
    }
}

/// Write `stdin` to the child and close it, so a `--*-stdin` read sees its line and then EOF.
fn write_stdin(child: &mut Child, stdin: &str) {
    let mut pipe = child.stdin.take().expect("stdin is piped");
    pipe.write_all(stdin.as_bytes()).expect("stdin is written");
}

/// Wait for `child` to exit within [`CLI_BOUND`], killing it and failing the test otherwise.
fn wait_bounded(child: &mut Child, args: &[&str]) -> ExitStatus {
    let deadline = Instant::now() + CLI_BOUND;
    loop {
        if let Some(status) = child.try_wait().expect("the child's status reads") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("capsule {args:?} did not exit within {CLI_BOUND:?}");
        }
        std::thread::sleep(POLL);
    }
}

/// A fresh pseudo-terminal: its master and its slave.
fn open_pty() -> (OwnedFd, OwnedFd) {
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: `openpty` writes two descriptors it opened into the two out-parameters, and the
    // null name, termios and winsize pointers are documented as "not wanted".
    let opened = unsafe {
        libc::openpty(
            &raw mut master,
            &raw mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(
        opened,
        0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: both descriptors were just opened by `openpty` and nothing else owns them.
    let fds = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    assert!(fds.0.as_raw_fd() >= 0 && fds.1.as_raw_fd() >= 0);
    fds
}

/// Everything the PTY master yields, chunk by chunk, on a thread of its own; the channel closes
/// once every slave is closed (the read then fails with `EIO`, or returns 0).
fn read_screen(mut master: File) -> mpsc::Receiver<String> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        while let Ok(read) = master.read(&mut buffer) {
            if read == 0
                || sender
                    .send(String::from_utf8_lossy(&buffer[..read]).into_owned())
                    .is_err()
            {
                break;
            }
        }
    });
    receiver
}

/// The 64 lowercase hex characters `auth escrow` shows as the recovery secret, once printed.
fn shown_secret(printed: &str) -> Option<&str> {
    printed
        .as_bytes()
        .windows(64)
        .position(|window| {
            window
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        })
        .map(|start| &printed[start..start + 64])
}

/// The guide's set-up order reaches verified custody: after `library init`, `auth register`,
/// `auth login` and `auth escrow`, one imported photo pushes to `verified` with no failure and
/// no `auth recover` anywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_library_bound_by_auth_escrow_pushes_to_verified_custody() {
    let server = Server::boot().await;
    let scratch = ScratchDir::new();
    let cli = Cli {
        home: scratch.path().join("home"),
        endpoint: server.base_url().to_owned(),
    };
    let library = scratch.path().join("library");
    let photo = scratch.path().join("photo.jpg");
    std::fs::write(&photo, capsule_e2e::fixtures::synthetic_jpeg()).expect("the photo writes");
    let library_arg = library.to_str().expect("a UTF-8 library path").to_owned();
    let photo_arg = photo.to_str().expect("a UTF-8 photo path").to_owned();
    let email = format!("escrow-{}@capsule.test", nanoid::nanoid!(8).to_lowercase());

    // Every run blocks on a child process, so it runs off the runtime the server answers on.
    let push = tokio::task::spawn_blocking(move || {
        let password = format!("{PASSWORD}\n");
        let passphrase = format!("{PASSPHRASE}\n");
        cli.ok(
            &["auth", "register", "--email", &email, "--password-stdin"],
            &password,
        );
        cli.ok(
            &["auth", "login", "--email", &email, "--password-stdin"],
            &password,
        );
        cli.ok(&["library", "init", &library_arg, "--name", "Escrow"], "");
        let escrowed = cli.escrow(&library);
        assert!(
            escrowed.contains("Recovery is set up."),
            "auth escrow did not report recovery set up:\n{escrowed}"
        );
        cli.ok(
            &[
                "import",
                &photo_arg,
                "--library",
                &library_arg,
                "--passphrase-stdin",
            ],
            &passphrase,
        );
        cli.ok(
            &[
                "push",
                "--library",
                &library_arg,
                "--passphrase-stdin",
                "--json",
            ],
            &passphrase,
        )
    })
    .await
    .expect("the CLI runs finish");

    let summary: Value = serde_json::from_str(&push.stdout)
        .unwrap_or_else(|error| panic!("push --json is one document ({error}):\n{}", push.stdout));
    assert_eq!(summary["assets"], 1, "{summary:#}");
    assert_eq!(summary["failed_assets"], 0, "{summary:#}");
    assert_eq!(summary["verified_assets"], 1, "{summary:#}");
    let outcomes = summary["outcomes"]
        .as_array()
        .expect("outcomes is an array");
    assert_eq!(outcomes.len(), 1, "{summary:#}");
    assert_eq!(outcomes[0]["status"], "verified", "{summary:#}");
}
