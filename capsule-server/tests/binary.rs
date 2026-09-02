//! The `capsule-server` binary, driven as a process (issue #401).
//!
//! # Why a subprocess when everything else here is in-process
//!
//! Every other case in this suite drives a built `Service` through
//! `kynos::test::TestClient` — no socket, no port, nothing to flake — and that is the right
//! shape for asserting what the server *decides*. It cannot assert anything about the
//! **binary**, and the binary is what this issue delivers. Four properties only a process has:
//!
//! - it **binds**, and on `--listen 127.0.0.1:0` it says which port it got, so a caller that
//!   asked the operating system to choose one can find it;
//! - it **drains on SIGTERM and exits 0**, which is the contract an orchestrator's termination
//!   window is written against;
//! - it **refuses to start** on a bad configuration, with a non-zero code and every fault named
//!   once — the aggregate report exists for an operator reading a crash loop's logs;
//! - and a real client reaches it over TCP. The client is `capsule-sdk`'s, generated from the
//!   committed `openapi.json`, which makes this the round trip `tests/sdk_client.rs` proves for
//!   the router proved for the *binary*: the document, the generated client, the socket and the
//!   composition root all agreeing at once.
//!
//! # Sending the signal
//!
//! `kill -TERM` through a subprocess rather than `libc::kill`. `libc` is not a dependency of
//! this crate and adding one so a test can send a signal would be a dependency in the binary's
//! own tree; `Child::kill` is `SIGKILL`, which is the one signal that proves nothing about a
//! graceful drain. The signal cases are `#[cfg(unix)]`; Windows's console-event equivalent is
//! not something a test can raise in a child process.

#![cfg(unix)]

use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use capsule_server::auth::SessionTokens;
use capsule_server::store::SystemClock;

/// A PKCS#8 v1 Ed25519 key, base64.
///
/// The retired deployment's own `.env.example` value, and it signs nothing anywhere: no
/// deployment ever used it. A committed key rather than a generated one because these cases
/// assert the **published** key is the one the tokens verify under, which needs a key both
/// sides can name.
const EXAMPLE_DER: &str = "MC4CAQAwBQYDK2VwBCIEIN6eTvXEL7xMZWHY8rTk7VbQSGSuRkle5MVfiiYUStLF";

/// The address the operating system will pick a port under.
const EPHEMERAL: &str = "127.0.0.1:0";

/// How long to wait for a spawned server to say where it is listening.
///
/// Generous: a debug-build first run pays for `Credentials::new`'s Argon2id decoy hash before it
/// binds, and a loaded CI machine can take a while over it. A timeout here fails the test with
/// the child's own output rather than hanging the suite.
const BIND_TIMEOUT: Duration = Duration::from_secs(60);

/// How long to wait for a signalled server to exit.
const EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// A `capsule-server` invocation with a clean environment.
///
/// Every setting this binary reads is removed before anything is set, because the test runner's
/// own environment is not this test's to trust: a developer with `DATABASE_URL` exported would
/// otherwise see a different server from CI.
fn server(args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_capsule-server"));
    for key in [
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
    ] {
        command.env_remove(key);
    }
    // Errors and the bind line only. A debug-build default of `debug` would put a few hundred
    // lines of module chatter into the failure output of every case here.
    command.env("RUST_LOG", "warn");
    command.args(args);
    command
}

/// Run `args` to completion and return `(exit code, stdout, stderr)`.
fn run(command: &mut Command) -> (Option<i32>, String, String) {
    let output = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("the binary runs");
    (
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A running server, and the base URL it answers on.
struct Serving {
    child: Child,
    base_url: String,
}

impl Serving {
    /// Spawn `serve --memory` on an ephemeral port and wait for it to say where it landed.
    fn spawn(blob_root: &std::path::Path) -> Self {
        let mut child = server(&[
            "serve",
            "--memory",
            "--listen",
            EPHEMERAL,
            "--blob-root",
            &blob_root.display().to_string(),
        ])
        .env("JWT_ED25519_DER", EXAMPLE_DER)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("the binary spawns");

        let stdout = child.stdout.take().expect("stdout is piped");
        let mut lines = BufReader::new(stdout).lines();
        let deadline = Instant::now() + BIND_TIMEOUT;
        // A blocking read on the child's pipe. The runtime this test is on has nothing else to
        // do until the address arrives, and the child is a separate process.
        let address = loop {
            assert!(
                Instant::now() < deadline,
                "the server did not report a bound address within {BIND_TIMEOUT:?}"
            );
            let line = lines
                .next()
                .expect("the server closed stdout without reporting an address")
                .expect("the line is readable");
            if let Some(address) = line.strip_prefix("listening on ") {
                break address.to_owned();
            }
        };

        Self {
            child,
            base_url: address,
        }
    }

    /// Ask it to stop the way an orchestrator does, and return its exit code.
    fn terminate(mut self) -> Option<i32> {
        let status = Command::new("kill")
            .args(["-TERM", &self.child.id().to_string()])
            .status()
            .expect("kill runs");
        assert!(status.success(), "the signal was delivered");

        let deadline = Instant::now() + EXIT_TIMEOUT;
        loop {
            match self.child.try_wait().expect("the child is waitable") {
                Some(status) => return status.code(),
                None => {
                    assert!(
                        Instant::now() < deadline,
                        "the server did not exit within {EXIT_TIMEOUT:?} of SIGTERM"
                    );
                    std::thread::sleep(Duration::from_millis(25));
                }
            }
        }
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        // A case that panicked before `terminate` must not leave a listener behind for the next
        // one. `SIGKILL` is right here: the assertion has already failed and a drain would only
        // delay the report.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[tokio::test]
async fn it_binds_serves_the_generated_client_and_drains_on_sigterm() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let serving = Serving::spawn(root.path());

    // The generated client, over reqwest, over TCP, against the binary. Nothing in this call is
    // hand-written: the path, the response shape and the decoding all come from the committed
    // `openapi.json`.
    let client = capsule_sdk::rest::Client::new(&serving.base_url).expect("a base url");
    let published = client
        .server_info()
        .await
        .expect("the record is served")
        .into_inner();

    // The signing key it publishes is derived from the configured private key, so an operator
    // cannot publish one the tokens do not verify under.
    let expected = SessionTokens::from_pkcs8(
        &base64::Engine::decode(&base64::engine::general_purpose::STANDARD, EXAMPLE_DER)
            .expect("the example key is base64"),
        Arc::new(SystemClock),
    )
    .expect("the example key parses")
    .public_key()
    .to_vec();
    assert_eq!(
        published.signing_key,
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &expected)
    );
    // The published login endpoint is one this server actually serves, which is the property
    // `api_base_url` exists to keep: it is composed from the configuration, not pasted.
    assert!(
        published.auth.login.ends_with("/v1/auth/login"),
        "{}",
        published.auth.login
    );

    assert_eq!(
        serving.terminate(),
        Some(0),
        "a drained shutdown is a successful one"
    );
}

#[tokio::test]
async fn an_account_registers_and_signs_in_against_the_running_binary() {
    // The whole reason the development profile ships a real account adapter rather than a
    // fail-closed stub: `mise run serve-memory` is a server a client can be pointed at.
    let root = tempfile::tempdir().expect("a scratch directory");
    let serving = Serving::spawn(root.path());

    let auth = capsule_sdk::auth::AuthClient::new(&format!("{}/v1/auth", serving.base_url))
        .expect("a base url");
    auth.register("somebody@example.test", "correct horse battery staple")
        .await
        .expect("registration succeeds");
    let signed_in = auth
        .login("somebody@example.test", "correct horse battery staple")
        .await
        .expect("the account signs in")
        .into_session();
    assert!(
        signed_in.is_ok(),
        "a fresh account has no second factor to answer"
    );

    // And the credential is actually checked — this is not the permissive double.
    let refused = auth
        .login("somebody@example.test", "the wrong password entirely")
        .await;
    assert!(refused.is_err(), "a wrong password is refused");

    assert_eq!(serving.terminate(), Some(0));
}

#[test]
fn serving_without_valkey_and_without_the_memory_profile_refuses_by_name() {
    // `store/mod.rs` has documented this refusal since `S-C29` and nothing enforced it, because
    // there was no boot path to enforce it in.
    let root = tempfile::tempdir().expect("a scratch directory");
    let (code, _, stderr) = run(&mut server(&[
        "serve",
        "--listen",
        EPHEMERAL,
        "--blob-root",
        &root.path().display().to_string(),
    ])
    .env("JWT_ED25519_DER", EXAMPLE_DER));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("VALKEY_URL"), "{stderr}");
}

#[test]
fn a_durable_backend_refuses_with_the_issue_that_will_honour_it() {
    // The other half: the operator *did* set `VALKEY_URL`, and nothing reads it yet. Falling
    // back to the in-memory adapters here is the one thing that must never happen.
    let root = tempfile::tempdir().expect("a scratch directory");
    let (code, _, stderr) = run(&mut server(&[
        "serve",
        "--listen",
        EPHEMERAL,
        "--blob-root",
        &root.path().display().to_string(),
    ])
    .env("JWT_ED25519_DER", EXAMPLE_DER)
    .env("VALKEY_URL", "redis://127.0.0.1:6379"));
    assert_ne!(code, Some(0), "{stderr}");
    assert!(stderr.contains("#403"), "{stderr}");
}

#[test]
fn every_missing_setting_is_named_in_one_message() {
    // An operator reading a crash loop's logs learns about both at once rather than restarting
    // the process to discover the second.
    let (code, _, stderr) = run(&mut server(&["serve", "--memory", "--listen", EPHEMERAL]));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("BLOB_ROOT"), "{stderr}");
    assert!(stderr.contains("JWT_ED25519_DER"), "{stderr}");
}

#[test]
fn a_config_file_is_refused_with_what_to_do_instead() {
    let (code, _, stderr) = run(&mut server(&[
        "--config",
        "/etc/capsule/server.toml",
        "gen-openapi",
        "--check",
    ]));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("not supported yet"), "{stderr}");
    assert!(stderr.contains("environment"), "{stderr}");
}

#[test]
fn the_committed_openapi_document_is_reproduced_byte_for_byte() {
    // The gate `mise run openapi-check-kynos` runs, asserted here too so a change to the
    // subcommand's own plumbing cannot quietly stop checking anything. `CARGO_MANIFEST_DIR` is
    // `capsule-server/`, and the default output path is relative to the repo root.
    let committed = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("openapi.json");
    let (code, stdout, stderr) = run(&mut server(&[
        "gen-openapi",
        &committed.display().to_string(),
        "--check",
    ]));
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("up to date"), "{stdout}");
}
