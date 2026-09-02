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

/// A 64-byte attestation seed, base64.
///
/// A durable deployment has to supply its own — it is deliberately not derived from
/// `JWT_ED25519_DER`, because a receipt that verified under the operational key would let
/// anything holding that key manufacture custody evidence.
const EXAMPLE_SEED: &str =
    "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQ==";

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
            if let Some(status) = self.child.try_wait().expect("the child is waitable") {
                return status.code();
            }
            assert!(
                Instant::now() < deadline,
                "the server did not exit within {EXIT_TIMEOUT:?} of SIGTERM"
            );
            std::thread::sleep(Duration::from_millis(25));
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
    let (code, _, stderr) = run(server(&[
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
    let (code, _, stderr) = run(server(&[
        "serve",
        "--listen",
        EPHEMERAL,
        "--blob-root",
        &root.path().display().to_string(),
    ])
    .env("JWT_ED25519_DER", EXAMPLE_DER)
    .env("ATTESTATION_KEY_SEED", EXAMPLE_SEED)
    .env("VALKEY_URL", "redis://127.0.0.1:6379"));
    assert_ne!(code, Some(0), "{stderr}");
    assert!(stderr.contains("#403"), "{stderr}");
}

#[test]
fn a_durable_serve_is_refused_without_an_attestation_seed_of_its_own() {
    // The attestation key must be distinct from the token signer — `attestation/mod.rs` requires
    // it so that holding the operational key does not let anything manufacture custody evidence.
    // Deriving the seed from `JWT_ED25519_DER` under a different HKDF label is not a separation:
    // anyone with the token key recomputes it. So a real deployment is made to say what its
    // attestation identity is, and only `--memory` keeps the derivation.
    let root = tempfile::tempdir().expect("a scratch directory");
    let (code, _, stderr) = run(server(&[
        "serve",
        "--listen",
        EPHEMERAL,
        "--blob-root",
        &root.path().display().to_string(),
    ])
    .env("JWT_ED25519_DER", EXAMPLE_DER)
    .env("VALKEY_URL", "redis://127.0.0.1:6379"));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("ATTESTATION_KEY_SEED"), "{stderr}");
}

#[test]
fn the_memory_profile_still_needs_only_one_key() {
    // The other side of the same decision: a development server derives its attestation seed, so
    // `serve --memory` comes up on one variable rather than two. Asserted through `gc`'s sibling
    // path — a full `serve` is covered by the socket case above — by checking that the config
    // layer raises no seed fault for `--memory`.
    let root = tempfile::tempdir().expect("a scratch directory");
    let (code, _, stderr) =
        run(server(&["serve", "--memory", "--listen", EPHEMERAL])
            .env("JWT_ED25519_DER", EXAMPLE_DER));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("BLOB_ROOT"), "{stderr}");
    assert!(
        !stderr.contains("ATTESTATION_KEY_SEED"),
        "the development profile derives it, so naming it would send a developer looking for a \
         variable it does not want: {stderr}"
    );
    let _ = root;
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
// ===========================================================================================
// The operator commands
// ===========================================================================================

/// A blob root holding one file under `blobs/`, shaped the way the store shards them.
///
/// `blobs/aa/aa/<64 a's>.bin`: the two shard segments are the address's own first four hex
/// characters and the suffix is `ContentAddress::file_name`'s, because a file the enumeration
/// walk cannot turn back into an address is *debris* rather than a blob — which would be a
/// different finding from the one each case here is about.
///
/// Written directly rather than uploaded, because the point is a store the *index* knows
/// nothing about: in the `--memory` profile the index is empty on every invocation, so every
/// blob on disk is genuinely unreferenced and both the collector and the scrub have something
/// true to say about it.
fn seeded_root() -> (tempfile::TempDir, String) {
    let root = tempfile::tempdir().expect("a scratch directory");
    let address = "a".repeat(64);
    let shard = root.path().join("blobs").join("aa").join("aa");
    std::fs::create_dir_all(&shard).expect("the shard is created");
    std::fs::write(
        shard.join(format!("{address}.bin")),
        b"unreferenced ciphertext",
    )
    .expect("the blob is written");
    (root, address)
}

/// The path a seeded blob occupies, for asserting it is still there.
fn seeded_blob(root: &std::path::Path, address: &str) -> std::path::PathBuf {
    root.join("blobs")
        .join("aa")
        .join("aa")
        .join(format!("{address}.bin"))
}

/// An operator command over `root`, in the development profile.
fn operator(subcommand: &str, root: &std::path::Path, extra: &[&str]) -> Command {
    let mut args = vec![subcommand, "--memory", "--blob-root"];
    let root = root.display().to_string();
    args.push(&root);
    args.extend_from_slice(extra);
    let mut command = server(&args);
    command.env_remove("JWT_ED25519_DER");
    command
}

#[test]
fn a_collection_dry_run_names_the_unreferenced_blob_and_changes_nothing() {
    let (root, address) = seeded_root();
    let blob = seeded_blob(root.path(), &address);

    let (code, stdout, stderr) = run(&mut operator("gc", root.path(), &[]));
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("dry run"), "{stdout}");
    assert!(stdout.contains("marked (1)"), "{stdout}");
    assert!(stdout.contains(&address), "{stdout}");
    assert!(blob.is_file(), "a dry run does not touch the store");
}

#[test]
fn an_applied_collection_pass_marks_rather_than_sweeps_on_its_first_look() {
    // Two passes by design: a blob that reaches zero references is marked, and swept only on a
    // later pass once the grace window has passed. In this profile the mark store does not
    // survive the process, so a fresh invocation can only ever mark — which is stated in
    // `boot`'s docs and asserted here rather than left as a surprise. The cross-invocation
    // sweep needs the durable mark store #402 brings; `gc`'s own unit tests prove the
    // mark-then-sweep sequence in process.
    let (root, address) = seeded_root();
    let blob = seeded_blob(root.path(), &address);

    let (code, stdout, stderr) = run(&mut operator("gc", root.path(), &["--apply"]));
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("applied"), "{stdout}");
    assert!(stdout.contains("marked (1)"), "{stdout}");
    assert!(!stdout.contains("swept"), "{stdout}");
    assert!(
        blob.is_file(),
        "nothing has waited out its grace window yet"
    );
}

#[test]
fn a_collection_pass_over_an_empty_store_has_nothing_to_do() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let (code, stdout, stderr) = run(&mut operator("gc", root.path(), &[]));
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("nothing to do"), "{stdout}");
}

#[test]
fn a_retention_purge_runs_and_reports_an_empty_pass() {
    // The index is empty in this profile, so there is no tombstone to purge. What is asserted
    // is that the command runs, reports, and does not invent work.
    let root = tempfile::tempdir().expect("a scratch directory");
    let (code, stdout, stderr) = run(&mut operator("purge", root.path(), &[]));
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("retention purge"), "{stdout}");
    assert!(stdout.contains("nothing to do"), "{stdout}");
}

#[test]
fn a_scrub_exits_non_zero_on_a_finding_and_mutates_nothing() {
    // `design/filesystem/maintenance.md`: it "exits non-zero, and mutates nothing".
    let (root, address) = seeded_root();
    let blob = seeded_blob(root.path(), &address);
    let before = std::fs::read(&blob).expect("the blob is readable");

    let (code, stdout, stderr) = run(&mut operator("scrub", root.path(), &[]));
    assert_eq!(code, Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("orphan (1)"), "{stdout}");
    assert!(stdout.contains(&address), "{stdout}");
    assert_eq!(
        std::fs::read(&blob).expect("the blob is still readable"),
        before,
        "the store is byte-identical afterwards"
    );
}

#[test]
fn a_scrub_over_a_clean_store_exits_zero() {
    let root = tempfile::tempdir().expect("a scratch directory");
    let (code, stdout, stderr) = run(&mut operator("scrub", root.path(), &[]));
    assert_eq!(code, Some(0), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("agree"), "{stdout}");
}

#[test]
fn a_deep_scrub_re_hashes_the_bytes_it_reads() {
    // The bit-rot check. The seeded file's name is not its own hash, so a deep pass finds the
    // mismatch a structural one cannot see — and reports how many bytes it read.
    let (root, _) = seeded_root();
    let (code, stdout, stderr) = run(&mut operator("scrub", root.path(), &["--deep"]));
    assert_eq!(code, Some(1), "stdout: {stdout}\nstderr: {stderr}");
    assert!(stdout.contains("byte_mismatch"), "{stdout}");
    assert!(!stdout.contains("0 bytes hashed"), "{stdout}");
}

#[test]
fn the_operator_commands_need_no_key_material() {
    // A maintenance host that had to hold the production token-signing key to sweep a directory
    // would be a reason to put the key on a maintenance host. `operator` removes it, so every
    // case above already asserts this — this one says so on purpose.
    let root = tempfile::tempdir().expect("a scratch directory");
    for subcommand in ["gc", "purge", "scrub"] {
        let (code, stdout, stderr) = run(&mut operator(subcommand, root.path(), &[]));
        assert_eq!(code, Some(0), "{subcommand}: {stdout}{stderr}");
        assert!(
            !stderr.contains("JWT_ED25519_DER"),
            "{subcommand}: {stderr}"
        );
    }
}

#[test]
fn an_operator_command_without_memory_is_told_that_and_not_about_valkey() {
    // An operator running `capsule-server scrub` has typically set no backend variable at all.
    // Naming `VALKEY_URL` — which these commands never demand — would send them to configure
    // something that would not have helped; what is missing is the durable index they read.
    let root = tempfile::tempdir().expect("a scratch directory");
    for subcommand in ["gc", "purge", "scrub"] {
        let (code, _, stderr) = run(&mut server(&[
            subcommand,
            "--blob-root",
            &root.path().display().to_string(),
        ]));
        assert_ne!(code, Some(0), "{subcommand}: {stderr}");
        assert!(stderr.contains("--memory"), "{subcommand}: {stderr}");
        assert!(!stderr.contains("VALKEY_URL"), "{subcommand}: {stderr}");
    }
}

#[test]
fn an_operator_command_without_a_blob_root_refuses_by_name() {
    let (code, _, stderr) = run(&mut server(&["scrub", "--memory"]));
    assert_eq!(code, Some(2), "{stderr}");
    assert!(stderr.contains("BLOB_ROOT"), "{stderr}");
}
