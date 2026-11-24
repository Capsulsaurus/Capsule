use std::collections::VecDeque;
use std::process::{ExitStatus, Output};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

/// An exit status with `code`.
fn status(code: i32) -> ExitStatus {
    #[cfg(unix)]
    {
        std::os::unix::process::ExitStatusExt::from_raw(code << 8)
    }
    #[cfg(windows)]
    {
        std::os::windows::process::ExitStatusExt::from_raw(code as u32)
    }
}

fn answer(code: i32, stdout: &str, stderr: &str) -> Output {
    Output {
        status: status(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: stderr.as_bytes().to_vec(),
    }
}

/// One call the fake saw.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Call {
    arguments: Vec<String>,
    stdin: Option<Vec<u8>>,
}

#[derive(Debug, Default)]
struct Script {
    answers: VecDeque<Output>,
    calls: Vec<Call>,
    removed: Vec<String>,
}

/// A container CLI that answers from a script and records what it was asked.
#[derive(Debug, Clone, Default)]
struct Fake(Arc<Mutex<Script>>);

impl Fake {
    fn answering(answers: impl IntoIterator<Item = Output>) -> Self {
        let fake = Self::default();
        fake.0.lock().expect("unpoisoned").answers = answers.into_iter().collect();
        fake
    }

    fn calls(&self) -> Vec<Call> {
        self.0.lock().expect("unpoisoned").calls.clone()
    }

    fn removed(&self) -> Vec<String> {
        self.0.lock().expect("unpoisoned").removed.clone()
    }
}

impl Runtime for Fake {
    async fn run(
        &self,
        arguments: Vec<String>,
        stdin: Option<Vec<u8>>,
    ) -> Result<Output, ContainerError> {
        let mut script = self.0.lock().expect("unpoisoned");
        script.calls.push(Call { arguments, stdin });
        Ok(script
            .answers
            .pop_front()
            .expect("the script has an answer for every call"))
    }

    fn remove(&self, id: &str) {
        self.0
            .lock()
            .expect("unpoisoned")
            .removed
            .push(id.to_owned());
    }
}

/// A request that polls fast, so a test about the bound does not take the real one.
fn quick() -> Request {
    Request::postgres().with_readiness(Duration::from_millis(30), Duration::from_millis(1))
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|&part| part.to_owned()).collect()
}

/// The answers a healthy start sees: an id, a port, and a ready server.
fn healthy() -> Vec<Output> {
    vec![
        answer(0, "abc123\n", ""),
        answer(0, "127.0.0.1:41234\n", ""),
        answer(0, "127.0.0.1:5432 - accepting connections\n", ""),
    ]
}

#[tokio::test]
async fn a_start_runs_the_pinned_image_on_loopback_and_asks_for_its_port() {
    let fake = Fake::answering(healthy());
    let container = start_with(fake.clone(), &quick()).await.expect("it starts");

    let calls = fake.calls();
    assert_eq!(
        calls[0].arguments,
        strings(&[
            "run",
            "--detach",
            "--rm",
            "--label",
            "capsule.test=postgres",
            "--publish",
            "127.0.0.1::5432",
            "--env",
            "POSTGRES_PASSWORD=postgres",
            "--env",
            "PGDATA=/tmp/pgdata",
            "docker.io/library/postgres:18",
        ])
    );
    assert_eq!(calls[1].arguments, strings(&["port", "abc123", "5432/tcp"]));
    assert_eq!(
        calls[2].arguments,
        strings(&[
            "exec",
            "abc123",
            "pg_isready",
            "--host",
            "127.0.0.1",
            "--username",
            "postgres",
        ])
    );
    assert_eq!(container.port(), 41234);
    assert_eq!(
        container.url("rebuilt"),
        "postgres://postgres:postgres@127.0.0.1:41234/rebuilt"
    );
}

#[tokio::test]
async fn a_user_namespace_mode_is_passed_and_an_empty_one_is_not() {
    let fake = Fake::answering(healthy());
    let request = quick().with_userns(Some("keep-id".to_owned()));
    let _container = start_with(fake.clone(), &request).await.expect("it starts");
    let run = &fake.calls()[0].arguments;
    assert!(run.contains(&"--userns=keep-id".to_owned()), "{run:?}");
    // The image is the last argument, after every option.
    assert_eq!(
        run.last().map(String::as_str),
        Some("docker.io/library/postgres:18")
    );

    assert_eq!(quick().with_userns(Some(String::new())), quick());
}

#[tokio::test]
async fn readiness_is_polled_until_postgres_answers() {
    let fake = Fake::answering([
        answer(0, "abc123", ""),
        answer(0, "127.0.0.1:41234", ""),
        answer(2, "127.0.0.1:5432 - no response", ""),
        answer(1, "127.0.0.1:5432 - rejecting connections", ""),
        answer(0, "127.0.0.1:5432 - accepting connections", ""),
    ]);
    let request =
        Request::postgres().with_readiness(Duration::from_secs(5), Duration::from_millis(1));
    let _container = start_with(fake.clone(), &request).await.expect("it starts");
    assert_eq!(fake.calls().len(), 5);
}

#[tokio::test]
async fn a_server_that_never_answers_is_an_error_and_its_container_is_removed() {
    let mut answers = vec![answer(0, "abc123", ""), answer(0, "127.0.0.1:41234", "")];
    answers.extend((0..1_000).map(|_| answer(2, "127.0.0.1:5432 - no response", "")));
    let fake = Fake::answering(answers);

    let error = start_with(fake.clone(), &quick())
        .await
        .expect_err("it never became ready");
    match error {
        ContainerError::NotReady { waited, last } => {
            assert!(waited >= Duration::from_millis(30), "{waited:?}");
            assert_eq!(last, "127.0.0.1:5432 - no response");
        }
        other => panic!("expected NotReady, got {other:?}"),
    }
    assert_eq!(fake.removed(), vec!["abc123".to_owned()]);
}

#[tokio::test]
async fn a_refused_run_is_an_error_carrying_what_the_cli_said_and_removes_nothing() {
    let fake = Fake::answering([answer(125, "", "Error: image not known\n")]);
    let error = start_with(fake.clone(), &quick())
        .await
        .expect_err("the run failed");
    match error {
        ContainerError::Failed {
            command, stderr, ..
        } => {
            assert!(command.starts_with("run --detach"), "{command}");
            assert_eq!(stderr, "Error: image not known");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
    assert!(fake.removed().is_empty());
}

#[tokio::test]
async fn a_container_with_no_published_port_is_removed() {
    let fake = Fake::answering([answer(0, "abc123", ""), answer(0, "\n", "")]);
    let error = start_with(fake.clone(), &quick())
        .await
        .expect_err("no port");
    assert!(matches!(error, ContainerError::Port { .. }), "{error:?}");
    assert_eq!(fake.removed(), vec!["abc123".to_owned()]);
}

#[test]
fn the_port_is_read_from_the_first_published_address() {
    // Docker prints the IPv4 binding and then the IPv6 one; podman prints one line.
    assert_eq!(
        parse_port("0.0.0.0:32768\n[::]:32768\n").expect("a port"),
        32768
    );
    assert_eq!(parse_port("127.0.0.1:41234").expect("a port"), 41234);
    assert!(parse_port("").is_err());
    assert!(parse_port("127.0.0.1:not-a-port").is_err());
}

#[tokio::test]
async fn dropping_the_container_removes_it_exactly_once() {
    let fake = Fake::answering(healthy());
    let container = start_with(fake.clone(), &quick()).await.expect("it starts");
    assert!(fake.removed().is_empty());
    drop(container);
    assert_eq!(fake.removed(), vec!["abc123".to_owned()]);
}

#[tokio::test]
async fn exec_returns_the_output_whatever_the_exit_status() {
    let mut answers = healthy();
    answers.push(answer(1, "", "pg_restore: error: nope"));
    let fake = Fake::answering(answers);
    let container = start_with(fake.clone(), &quick()).await.expect("it starts");

    let output = container
        .exec(&["pg_restore", "--dbname", "postgres"])
        .await
        .expect("the CLI ran");
    assert!(!output.status.success());
    assert_eq!(output.stderr, b"pg_restore: error: nope");
    assert_eq!(
        fake.calls()[3].arguments,
        strings(&["exec", "abc123", "pg_restore", "--dbname", "postgres"])
    );
}

#[tokio::test]
async fn copy_in_pipes_the_bytes_to_the_path_without_parsing_it_as_shell() {
    let mut answers = healthy();
    answers.push(answer(0, "", ""));
    let fake = Fake::answering(answers);
    let container = start_with(fake.clone(), &quick()).await.expect("it starts");

    container
        .copy_in("/tmp/capsule dump; rm -rf /", b"dump bytes".to_vec())
        .await
        .expect("it copies");
    let call = &fake.calls()[3];
    assert_eq!(
        call.arguments,
        strings(&[
            "exec",
            "--interactive",
            "abc123",
            "sh",
            "-c",
            "cat > \"$1\"",
            "sh",
            "/tmp/capsule dump; rm -rf /",
        ])
    );
    assert_eq!(call.stdin.as_deref(), Some(b"dump bytes".as_slice()));
}

#[tokio::test]
async fn a_failed_copy_is_an_error() {
    let mut answers = healthy();
    answers.push(answer(
        1,
        "",
        "sh: can't create /nope/dump: nonexistent directory",
    ));
    let fake = Fake::answering(answers);
    let container = start_with(fake, &quick()).await.expect("it starts");
    assert!(matches!(
        container.copy_in("/nope/dump", Vec::new()).await,
        Err(ContainerError::Failed { .. })
    ));
}

#[tokio::test]
async fn a_cli_that_is_not_on_the_path_is_a_spawn_error() {
    let error = Cli::new("capsule-test-postgres-no-such-cli")
        .run(strings(&["version"]), None)
        .await
        .expect_err("there is no such program");
    assert!(matches!(error, ContainerError::Spawn { .. }), "{error:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn a_cli_that_stops_reading_its_input_is_an_io_error_not_a_spawn_error() {
    // `true` exits without reading stdin, so a write larger than a pipe buffer fails. That is
    // the CLI running and then failing, which `Spawn`'s "set the CLI" advice would misdirect.
    let error = Cli::new("true")
        .run(Vec::new(), Some(vec![0; 4 << 20]))
        .await
        .expect_err("the input cannot be written");
    assert!(matches!(error, ContainerError::Io { .. }), "{error:?}");
}

#[test]
fn the_cli_is_docker_unless_one_is_named() {
    assert_eq!(program_from(None), "docker");
    assert_eq!(program_from(Some("  ".to_owned())), "docker");
    assert_eq!(program_from(Some("podman".to_owned())), "podman");
}

#[test]
fn the_default_request_reads_no_environment() {
    let request = Request::postgres();
    assert_eq!(request.userns, None);
    assert_eq!(request.ready_bound(), READY_BOUND);
}
