//! **Kill-anywhere** (issue #678): the real `capsule` binary and the real `capsule-server`
//! binary over a fixture corpus, with either process `SIGKILL`ed at a seeded point during each
//! phase the suite runs ([`PHASES`]), and every phase rerun until it converges.
//!
//! This is **E2E case 11's process-restart variant** — a server killed outright mid-transfer
//! loses nothing it committed and leaves nothing referencing bytes it never stored — and the
//! crash half of **E2E case 2** (import + upload + finalize), whose in-process half lives in
//! `case_02_import_upload_finalize.rs`. The restore phase, the crash half of **E2E case 6**, is
//! built here but not yet in [`PHASES`]: `capsule restore` cannot restore from a real server
//! until #791 lands, so an unkilled restore would not converge either (#794).
//!
//! # What converges
//!
//! For each seed, a fresh account and library, then for each phase in order: the phase runs
//! once with its kill point armed, then again unkilled, and the rerun must exit zero. After the
//! last phase:
//!
//! - **every asset durable exactly once** — the library holds one asset per corpus file, the
//!   final `capsule push --json` reports every one verified (durable verdict plus a signed custody
//!   receipt per blob) and none failed, and the server's feed lists exactly the library's assets,
//!   each with its original held;
//! - **no orphaned index rows** — the server's own integrity scrub, run over the same Postgres,
//!   Valkey and blob root, finds no row without its blob, no chain or ledger fault, and no
//!   mirrored fact out of step. What a crash may legitimately leave — bytes nothing references,
//!   debris under the blob root, a stage with no session — is the collector's to reclaim, and is
//!   counted in the log rather than failed. One more is filtered by its exact shape until #790
//!   lands ([`is_issue_790`]);
//! - **a byte-equal restore**, once the restore phase runs — the final `capsule restore` writes
//!   one file per asset, and the files are the corpus, byte for byte.
//!
//! A kill point is a fraction of the phase (see `capsule_e2e::killpoints`), scaled by how long
//! the same phase took in an unkilled baseline run over the same corpus. A point that lands
//! after the process already exited is logged as such: the rerun must converge either way.
//!
//! # Running it
//!
//! It needs a container runtime for Postgres and Valkey, so it skips — one line on stderr, and
//! a pass — unless `CAPSULE_TEST_KILL_ANYWHERE=1`. `CAPSULE_KILL_SEED` names the first seed and
//! `CAPSULE_KILL_SEEDS` how many to try; every seed's schedule is printed, so a nightly failure
//! replays with `CAPSULE_KILL_SEED=<seed> CAPSULE_KILL_SEEDS=1`. Rootless podman needs
//! `DOCKER_HOST` at its socket and `CAPSULE_TEST_CONTAINER_USERNS=keep-id`, as the server's
//! container tiers do.

#![cfg(unix)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use capsule_core::lifecycle::Workspace;
use capsule_e2e::fixtures::synthetic_jpeg_sized;
use capsule_e2e::killpoints::{self, KillPlan, KillPoint, Phase, Victim};
use capsule_e2e::processes::{Binaries, Cli, CliOutput, ServerProcess};
use capsule_e2e::{Device, FAST_KDF, JWT_ED25519_DER, PASSPHRASE, PASSWORD, auth_base_of, feed_at};
use capsule_sdk::auth::AuthClient;
use capsule_sdk::sync::ChangeKind;
use capsule_server::boot;
use capsule_server::config::{Config, Demands, Overrides};
use capsule_server::scrub::{self, Depth, Finding};
use server_migration::MigratorTrait as _;
use tempfile::TempDir;
use testcontainers::runners::AsyncRunner as _;
use testcontainers::{ContainerAsync, ImageExt as _};
use testcontainers_modules::postgres::Postgres;
use testcontainers_modules::valkey::{VALKEY_PORT, Valkey};
use uuid::Uuid;

/// The phases the suite cuts, in pipeline order. [`Phase::Restore`] joins once `capsule
/// restore` can restore from a real server (#791, tracked by #794); every seed's plan already
/// draws its restore point, so adding it changes no other phase's schedule.
const PHASES: [Phase; 2] = [Phase::Import, Phase::Push];

/// The variable that admits the suite.
const GATE: &str = "CAPSULE_TEST_KILL_ANYWHERE";

/// The variable naming a directory to keep every process's logs in, so a failed run can be
/// read afterwards (CI uploads it). Unset, the logs go to a temp directory.
const LOG_DIR_VAR: &str = "CAPSULE_KILL_LOG_DIR";

/// The rootless-podman user-namespace mode, when the host needs one (`postgres::testing`).
const USERNS_MODE: &str = "CAPSULE_TEST_CONTAINER_USERNS";

/// The tags `capsule-server/compose.yaml` runs, as the server's container tiers pin them.
const POSTGRES_TAG: &str = "18";
/// See [`POSTGRES_TAG`].
const VALKEY_TAG: &str = "9.0.4";

/// A 64-byte attestation seed, base64 — the durable profile needs one of its own, and it signs
/// nothing anywhere (`capsule-server/tests/binary.rs` uses the same bytes).
const ATTESTATION_SEED: &str =
    "CQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQ==";

/// How long any one `capsule` run may take before the suite fails it. A CLI that outlives this
/// after its server came back is a hang, which is a finding in its own right.
const CLI_BOUND: Duration = Duration::from_secs(180);

/// Scrub findings a crash may legitimately leave behind: the collector's work, not a fault.
const RECLAIMABLE: &[&str] = &["orphan", "debris", "stale_stage"];

/// The fixture corpus: distinct stills, so every file is its own asset. One is past the
/// thumbnail tier's 256-pixel cap, so a real derivative rides the preview tier and a kill can
/// land between tiers; the rest stay small to keep a seed fast.
fn corpus() -> Vec<(String, Vec<u8>)> {
    [(8, 8), (16, 8), (8, 16), (24, 16), (16, 32), (264, 264)]
        .into_iter()
        .map(|(width, height)| {
            (
                format!("still-{width}x{height}.jpg"),
                synthetic_jpeg_sized(width, height),
            )
        })
        .collect()
}

/// A migrated Postgres and a Valkey, held for the length of the test.
struct Backends {
    database_url: String,
    valkey_url: String,
    _postgres: ContainerAsync<Postgres>,
    _valkey: ContainerAsync<Valkey>,
}

impl Backends {
    async fn start() -> Self {
        let userns = std::env::var(USERNS_MODE)
            .ok()
            .filter(|mode| !mode.is_empty());

        let mut postgres = Postgres::default()
            .with_tag(POSTGRES_TAG)
            // Off the image's declared volume, for the reason `postgres::testing` gives.
            .with_env_var("PGDATA", "/tmp/pgdata");
        if let Some(mode) = &userns {
            postgres = postgres.with_userns_mode(mode);
        }
        let postgres = postgres
            .start()
            .await
            .expect("the Postgres container starts");
        let host = postgres.get_host().await.expect("a host");
        let port = postgres.get_host_port_ipv4(5432).await.expect("a port");
        let database_url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
        let connection = capsule_server::postgres::connect(&database_url)
            .await
            .expect("the container accepts a connection");
        // What an operator runs before the first `serve`.
        server_migration::Migrator::up(&connection, None)
            .await
            .expect("the schema applies");
        drop(connection);

        let mut valkey = Valkey::default().with_tag(VALKEY_TAG);
        if let Some(mode) = &userns {
            valkey = valkey.with_userns_mode(mode);
        }
        let valkey = valkey.start().await.expect("the Valkey container starts");
        let host = valkey.get_host().await.expect("a host");
        let port = valkey
            .get_host_port_ipv4(VALKEY_PORT)
            .await
            .expect("a port");

        Self {
            database_url,
            valkey_url: format!("redis://{host}:{port}"),
            _postgres: postgres,
            _valkey: valkey,
        }
    }

    /// What a durable server process reads on top of its blob root.
    fn environment(&self) -> Vec<(String, String)> {
        vec![
            ("DATABASE_URL".to_owned(), self.database_url.clone()),
            ("VALKEY_URL".to_owned(), self.valkey_url.clone()),
            ("JWT_ED25519_DER".to_owned(), JWT_ED25519_DER.to_owned()),
            (
                "ATTESTATION_KEY_SEED".to_owned(),
                ATTESTATION_SEED.to_owned(),
            ),
        ]
    }
}

/// Everything the seeds share: the backends, one blob root, and the server process over them.
struct World {
    binaries: Binaries,
    backends: Backends,
    blob_root: TempDir,
    /// Where every process's stdout and stderr go: [`LOG_DIR_VAR`] when set, so the logs
    /// outlive a failed run, else a temp directory removed with the world.
    logs: PathBuf,
    _logs_temp: Option<TempDir>,
    server: ServerProcess,
}

impl World {
    async fn start() -> Self {
        let binaries = Binaries::build();
        let backends = Backends::start().await;
        let blob_root = tempfile::tempdir().expect("a temp blob root");
        let (logs, logs_temp) = match std::env::var_os(LOG_DIR_VAR) {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                std::fs::create_dir_all(&dir).expect("the log directory");
                (dir, None)
            }
            None => {
                let temp = tempfile::tempdir().expect("a temp log directory");
                (temp.path().to_owned(), Some(temp))
            }
        };
        let server = ServerProcess::spawn(
            &binaries.server,
            blob_root.path(),
            backends.environment(),
            &logs,
        );
        eprintln!(
            "kill-anywhere: server at {}, logs under {}",
            server.base_url(),
            logs.display()
        );
        Self {
            binaries,
            backends,
            blob_root,
            logs,
            _logs_temp: logs_temp,
            server,
        }
    }

    /// The server's integrity scrub over the same Postgres, Valkey and blob root the server
    /// process writes, run in-process through the composition root `capsule-server scrub` runs,
    /// so the findings come back typed rather than parsed out of a report.
    async fn scrub(&self) -> Vec<Finding> {
        let mut env: BTreeMap<String, String> = self.backends.environment().into_iter().collect();
        env.insert(
            "BLOB_ROOT".to_owned(),
            self.blob_root.path().display().to_string(),
        );
        let config = Config::load(&env, &Overrides::default(), Demands::Maintenance)
            .expect("the durable maintenance profile loads");
        let maintenance = boot::assemble_maintenance(&config)
            .await
            .expect("the maintenance workers assemble over the durable backends");
        scrub::scrub(&maintenance.scrub, Depth::Structural)
            .await
            .expect("the scrub finishes")
            .findings
    }
}

/// One account, its library, its corpus and its CLI: what one seed (or the baseline) works on.
struct Run {
    label: String,
    email: String,
    library: TempDir,
    corpus_dir: TempDir,
    out: TempDir,
    cli: Cli,
}

impl Run {
    /// Register an account, create its library bound to the account with its directory
    /// published and its default album provisioned (the harness's [`Device::register_at`]), put
    /// the corpus on disk, and sign the CLI in with `capsule auth login`.
    ///
    /// The library is created in-process at the fast fixture cost because `capsule auth escrow`,
    /// the CLI's way to bind a library, confirms its secret on an interactive prompt; the library
    /// records the cost it was wrapped under, so every `capsule` below runs the full unlock.
    async fn provision(world: &World, label: &str) -> Self {
        let device = Device::register_at(world.server.base_url(), label).await;
        let Device {
            email,
            root: library,
            workspace,
            ..
        } = device;
        drop(workspace);

        let corpus_dir = tempfile::tempdir().expect("a temp corpus directory");
        for (name, bytes) in corpus() {
            std::fs::write(corpus_dir.path().join(name), bytes).expect("the corpus writes");
        }
        let log_dir = world.logs.join(label);
        std::fs::create_dir_all(&log_dir).expect("the run's log directory");
        let mut cli = Cli::new(&world.binaries.cli, world.server.base_url(), &log_dir);
        let _ = cli
            .run(
                &["auth", "login", "--email", &email, "--password-stdin"],
                &format!("{PASSWORD}\n"),
                CLI_BOUND,
            )
            .success();
        Self {
            label: label.to_owned(),
            email,
            library,
            corpus_dir,
            out: tempfile::tempdir().expect("a temp restore directory"),
            cli,
        }
    }

    /// The `capsule` arguments that run `phase` over this run's library.
    fn args(&self, phase: Phase) -> Vec<OsString> {
        let library = self.library.path().as_os_str().to_owned();
        let mut args: Vec<OsString> = match phase {
            Phase::Import => vec![
                "import".into(),
                self.corpus_dir.path().as_os_str().to_owned(),
            ],
            Phase::Push => vec!["push".into(), "--json".into()],
            Phase::Restore => vec![
                "restore".into(),
                "--out".into(),
                self.out.path().as_os_str().to_owned(),
                "--json".into(),
            ],
        };
        args.extend(["--library".into(), library, "--passphrase-stdin".into()]);
        args
    }

    /// Run `phase` to completion, unkilled.
    fn run(&mut self, phase: Phase) -> CliOutput {
        let args = self.args(phase);
        self.cli.run(&args, &passphrase_line(), CLI_BOUND)
    }

    /// Run `phase` with `point` armed: `SIGKILL` its victim once `point.delay(reference)` has
    /// passed, restart the server at once if it was the victim, and let the CLI finish however
    /// it does. What the cut process managed is whatever the rerun finds.
    fn run_killed(&mut self, world: &mut World, point: KillPoint, reference: Duration) {
        let delay = point.delay(reference);
        let args = self.args(point.phase);
        let mut running = self.cli.spawn(&args, &passphrase_line());
        std::thread::sleep(delay.saturating_sub(running.elapsed()));
        let cut = match point.victim {
            Victim::Cli => {
                let cut = running.kill();
                let _ = running.wait(CLI_BOUND);
                cut
            }
            Victim::Server => {
                let cli_running = running.is_running();
                world.server.kill();
                world.server.restart();
                let output = running.wait(CLI_BOUND);
                eprintln!(
                    "  {} under a killed server exited {}",
                    point.phase.name(),
                    output.status
                );
                cli_running
            }
        };
        eprintln!(
            "  {}: SIGKILL {:?} at {}‰ ({delay:?}): {}",
            point.phase.name(),
            point.victim,
            point.permille,
            if cut {
                "cut the phase mid-flight"
            } else {
                "the phase had already finished"
            }
        );
    }

    /// The library as a fresh open sees it.
    fn library_assets(&self) -> BTreeSet<Uuid> {
        let workspace = Workspace::open(self.library.path(), PASSPHRASE, FAST_KDF)
            .expect("the library opens after every kill");
        workspace.asset_ids().into_iter().collect()
    }
}

/// The library passphrase as `--passphrase-stdin` reads it.
fn passphrase_line() -> String {
    format!(
        "{}\n",
        std::str::from_utf8(PASSPHRASE).expect("the fixture passphrase is UTF-8")
    )
}

/// Assert `phase` has converged for `run`, after its unkilled rerun exited zero with `output`.
async fn assert_converged(world: &World, run: &Run, phase: Phase, output: &CliOutput) {
    let corpus = corpus();
    let library = run.library_assets();
    assert_eq!(
        library.len(),
        corpus.len(),
        "{}: one library asset per corpus file, each imported exactly once",
        run.label
    );

    match phase {
        Phase::Import => {}
        Phase::Push => {
            let summary = output.json();
            assert_eq!(
                summary["assets"],
                corpus.len(),
                "{}: {summary:#}",
                run.label
            );
            assert_eq!(
                summary["verified_assets"],
                corpus.len(),
                "{}: every asset reaches verified custody: {summary:#}",
                run.label
            );
            assert_eq!(summary["failed_assets"], 0, "{}: {summary:#}", run.label);

            // The server lists exactly the library's assets, each with its original held.
            let session = AuthClient::new(&auth_base_of(world.server.base_url()))
                .expect("the auth base parses")
                .login(&run.email, PASSWORD)
                .await
                .expect("the account signs in")
                .into_session()
                .expect("no second factor");
            let mut latest: BTreeMap<Uuid, (u64, bool, ChangeKind)> = BTreeMap::new();
            for entry in feed_at(world.server.base_url(), session).await {
                let id = std::str::from_utf8(&entry.asset_id)
                    .ok()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .expect("a feed entry names its asset by id");
                let newer = latest
                    .get(&id)
                    .is_none_or(|(seq, ..)| *seq < entry.sync_seq);
                if newer {
                    latest.insert(id, (entry.sync_seq, entry.original_held, entry.kind));
                }
            }
            let listed: BTreeSet<Uuid> = latest.keys().copied().collect();
            assert_eq!(
                listed, library,
                "{}: the server indexes exactly the library's assets",
                run.label
            );
            for (id, (_, held, kind)) in &latest {
                assert!(*held, "{}: {id}'s original is held", run.label);
                assert_ne!(*kind, ChangeKind::Deleted, "{}: {id} is live", run.label);
            }

            assert_store_clean(world, &run.label).await;
        }
        Phase::Restore => {
            let summary = output.json();
            assert_eq!(summary["failed"], 0, "{}: {summary:#}", run.label);
            let restored: BTreeSet<Uuid> = summary["outcomes"]
                .as_array()
                .expect("outcomes is a list")
                .iter()
                .map(|outcome| {
                    Uuid::parse_str(outcome["asset_id"].as_str().expect("an asset id"))
                        .expect("a UUID")
                })
                .collect();
            assert_eq!(restored, library, "{}: every asset restores", run.label);

            let mut written = restored_files(run.out.path());
            let mut expected: Vec<Vec<u8>> = corpus.into_iter().map(|(_, bytes)| bytes).collect();
            written.sort();
            expected.sort();
            assert!(
                written == expected,
                "{}: the restored files are the corpus byte for byte ({} written, {} expected)",
                run.label,
                written.len(),
                expected.len()
            );
        }
    }
}

/// Every file `capsule restore` wrote: the output directory minus its staging directory.
fn restored_files(out: &Path) -> Vec<Vec<u8>> {
    std::fs::read_dir(out)
        .expect("the restore directory reads")
        .map(|entry| entry.expect("a directory entry").path())
        .filter(|path: &PathBuf| path.is_file())
        .map(|path| std::fs::read(path).expect("a restored file reads"))
        .collect()
}

/// Whether `finding` is #790's: a create leaves its row's `amk_version` at 0 whatever epoch the
/// manifest was sealed under, so check 5 flags every pushed asset, killed or not. Filtered by
/// its exact shape — the fact, and the 0 the write path stores — so any other mirrored-fact
/// mismatch still fails, and removed when #790 lands.
fn is_issue_790(finding: &Finding) -> bool {
    matches!(
        finding,
        Finding::MirroredFactMismatch { fact: "amk_version", index, .. } if index == "0"
    )
}

/// The scrub finds nothing but what a crash may leave for the collector.
async fn assert_store_clean(world: &World, label: &str) {
    let findings = world.scrub().await;
    let mut reclaimable: BTreeMap<&str, usize> = BTreeMap::new();
    let mut known = 0_usize;
    let faults: Vec<&Finding> = findings
        .iter()
        .filter(|finding| {
            if is_issue_790(finding) {
                known += 1;
                return false;
            }
            let class = finding.class();
            let tolerated = RECLAIMABLE.contains(&class);
            if tolerated {
                *reclaimable.entry(class).or_default() += 1;
            }
            !tolerated
        })
        .collect();
    eprintln!("  scrub after {label}: reclaimable {reclaimable:?}, #790 amk_version rows {known}");
    assert!(
        faults.is_empty(),
        "{label}: the scrub found index faults: {faults:#?}"
    );
}

/// The unkilled baseline: the pipeline once, converged, timing each phase for the kill points.
async fn baseline(world: &World) -> BTreeMap<Phase, Duration> {
    let mut run = Run::provision(world, "baseline").await;
    let mut reference = BTreeMap::new();
    for phase in PHASES {
        let output = run.run(phase).success();
        eprintln!("baseline {}: {:?}", phase.name(), output.elapsed);
        assert_converged(world, &run, phase, &output).await;
        reference.insert(phase, output.elapsed);
    }
    reference
}

#[tokio::test(flavor = "multi_thread")]
async fn every_seeded_kill_converges_to_durable_once_and_indexed_once() {
    if std::env::var(GATE).ok().as_deref() != Some("1") {
        eprintln!(
            "skipping kill-anywhere: set {GATE}=1 with a reachable container runtime \
             (DOCKER_HOST) to run it"
        );
        return;
    }
    let seeds = killpoints::seeds_from_env().unwrap_or_else(|error| panic!("{error}"));
    let mut world = World::start().await;
    let reference = baseline(&world).await;

    for seed in seeds {
        let plan = KillPlan::from_seed(seed);
        eprintln!(
            "seed {seed} (replay: {}={seed} {}=1): {:?}",
            killpoints::SEED_VAR,
            killpoints::SEEDS_VAR,
            plan.points
        );
        let mut run = Run::provision(&world, &format!("seed-{seed}")).await;
        for phase in PHASES {
            run.run_killed(&mut world, plan.point(phase), reference[&phase]);
            let output = run.run(phase).success();
            assert_converged(&world, &run, phase, &output).await;
        }
    }
}
