//! The `capsule-server` command line: one binary, several subcommands.
//!
//! # One binary and not four
//!
//! The Salvo tree shipped `capsule-gc`, `capsule-scrub`, `capsule-keygen` and `gen_openapi` as
//! separate `[[bin]]`s. Four executables would each need their own copy of the configuration
//! loader and the adapter seam — which is the duplication [`crate::boot`] exists to prevent —
//! and `design/filesystem/maintenance.md` calls the scrub "an operator-invoked command,
//! schedulable as a job" rather than a distinct executable. `capsule-cli` already sets the
//! one-binary-many-subcommands precedent.
//!
//! # Why the bodies live here and not in `main`
//!
//! `main.rs` installs error reporting and dispatches; everything a subcommand actually does is
//! in this module, in the library. That is what lets `tests/binary.rs` assert against the same
//! code the binary runs, and it is the shape `capsule-cli/src/main.rs` already has.
//!
//! # stdout is a data channel
//!
//! Every log line goes to **stderr**. `gen-openapi` writes a path to stdout and the operator
//! commands write a report there, and a subscriber sharing that stream is how a pipeline ends up
//! parsing a log line. `capsule-cli/tests/cull_round_trip.rs` has to set `RUST_LOG=off` to keep
//! stdout parseable, which is the failure mode being avoided here.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use color_eyre::eyre::{Context as _, Result, bail, eyre};
use kynos::server::Server;
use kynos::server::shutdown::Shutdown;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt as log_fmt};

use crate::boot::{self, Assembled, Maintenance};
use crate::config::{Config, Demands, Environment, LogFormat, Overrides, ProcessEnvironment};
use crate::gc::{CollectionReport, Mode, PurgeReport};
use crate::scrub::{Depth, ScrubReport};

/// The exit code a configuration refusal produces.
///
/// Two, and distinct from [`EXIT_FINDINGS`] on purpose: a wrapper script has to be able to tell
/// "you configured this wrongly" from "the store is not clean", and both being `1` would make a
/// misconfigured cron job look like a corrupted store. It is also the code clap itself uses for
/// a usage error, so the two kinds of "the invocation was wrong" agree.
pub const EXIT_MISCONFIGURED: u8 = 2;

/// The exit code a read-only check produces when it found something.
///
/// One. `design/filesystem/maintenance.md` requires that the scrub "exits non-zero, and mutates
/// nothing", which is what makes it usable as a monitoring probe.
pub const EXIT_FINDINGS: u8 = 1;

/// How many tombstoned assets one `purge` pass considers.
///
/// A bound rather than a policy: the pass walks the index and a retention sweep on a large
/// deployment should be a job that finishes, not one that holds a read for an hour. An operator
/// who wants more runs it again.
const DEFAULT_PURGE_LIMIT: usize = 1_000;

/// How many bytes one `scrub --deep` pass will read when no budget is given.
///
/// One gibibyte. `Depth::Deep` carries a budget precisely because re-hashing every blob is
/// heavy I/O by definition, and "a scrub that saturates the disk is a scrub an operator turns
/// off". A truncated pass says so in its report, so the default cannot silently pass a store it
/// did not finish looking at.
const DEFAULT_SCRUB_BUDGET: u64 = 1024 * 1024 * 1024;

/// The Capsule server.
#[derive(Debug, Parser)]
#[command(name = "capsule-server", author, version, about, long_about = None)]
pub struct Cli {
    /// Read settings from a configuration file.
    ///
    /// Reserved and **not implemented**: every setting is read from the environment. The flag
    /// exists so the refusal is a sentence rather than clap's "unexpected argument".
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,

    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// Where a subcommand's state lives.
///
/// Flattened into every subcommand that touches a store rather than declared once on [`Cli`],
/// because `gen-openapi` touches none and a global `--blob-root` would advertise otherwise.
#[derive(Debug, Args)]
pub struct BackendArgs {
    /// The filesystem tree ciphertext blobs are written to (`BLOB_ROOT`).
    #[arg(long, value_name = "PATH")]
    pub blob_root: Option<PathBuf>,

    /// Run on the in-memory adapters instead of Postgres and Valkey.
    ///
    /// A development profile, and an explicit act: a deployment that merely forgot `VALKEY_URL`
    /// must fail closed rather than come up holding state it loses on the next restart.
    #[arg(long)]
    pub memory: bool,
}

/// The things this binary does.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Accept requests until a termination signal, then drain.
    Serve {
        /// The address to bind (`SERVER_HOST`/`SERVER_PORT`, default `0.0.0.0:3000`).
        ///
        /// Port `0` asks the operating system to choose one, and the chosen address is written
        /// to stdout — see [`serve`].
        #[arg(long, value_name = "HOST:PORT")]
        listen: Option<SocketAddr>,

        /// Where state lives.
        #[command(flatten)]
        backend: BackendArgs,
    },

    /// Sweep blobs nothing references any more.
    ///
    /// Two passes, by design: a blob that reaches zero references is *marked*, and a later pass
    /// sweeps it once the grace window has passed and the count is still zero. That is what
    /// gives an in-flight finalization retry time to re-reference it.
    Gc {
        /// Carry it out. Without this nothing is marked, unmarked or swept.
        ///
        /// Dry run is the default for the two subcommands that write, because the first thing an
        /// operator does with a collector is find out what it thinks.
        #[arg(long)]
        apply: bool,

        /// How long a blob must sit at zero references before it may be swept
        /// (`GC_GRACE_WINDOW_HOURS`, default 24).
        #[arg(long, value_name = "HOURS")]
        grace_window_hours: Option<u64>,

        /// Where state lives.
        #[command(flatten)]
        backend: BackendArgs,
    },

    /// Drop the blob references of tombstoned assets whose retention window has passed.
    ///
    /// The tombstone itself stays: a client that has not synced since the delete still has to
    /// learn about it, and removing the row would make the deletion invisible rather than final.
    Purge {
        /// Carry it out. Without this nothing is dropped.
        #[arg(long)]
        apply: bool,

        /// How many tombstoned assets to consider in this pass.
        #[arg(long, value_name = "N", default_value_t = DEFAULT_PURGE_LIMIT)]
        limit: usize,

        /// Where state lives.
        #[command(flatten)]
        backend: BackendArgs,
    },

    /// Compare the index against the store and report every disagreement.
    ///
    /// Mutates nothing, by construction, and exits non-zero on a non-empty report — which is
    /// what makes it usable as a monitoring probe.
    Scrub {
        /// Also re-hash every blob's bytes: the bit-rot check.
        #[arg(long)]
        deep: bool,

        /// The most bytes a deep pass will read. Blobs past it are left for the next run.
        #[arg(long, value_name = "BYTES", default_value_t = DEFAULT_SCRUB_BUDGET)]
        budget: u64,

        /// Where state lives.
        #[command(flatten)]
        backend: BackendArgs,
    },

    /// Emit the OpenAPI 3.2 document the SDK's client is generated from.
    ///
    /// Needs no database, no Valkey, no key material, no disk and no network: the router is
    /// built purely to describe it, which is what lets `--check` run in the Rust check gate.
    GenOpenapi {
        /// Output path for the document, relative to the repo root.
        #[arg(value_name = "FILE", default_value = "capsule-server/openapi.json")]
        output: PathBuf,

        /// Verify the committed document is up to date instead of writing it (CI drift gate).
        #[arg(long)]
        check: bool,
    },
}

impl Command {
    /// What this subcommand needs from the configuration.
    fn demands(&self) -> Demands {
        match self {
            Self::Serve { .. } => Demands::Serve,
            // No key material. A maintenance host that had to hold the production
            // token-signing key to sweep a directory would be a reason to put the key there.
            Self::Gc { .. } | Self::Purge { .. } | Self::Scrub { .. } => Demands::Maintenance,
            Self::GenOpenapi { .. } => Demands::Nothing,
        }
    }

    /// What this subcommand overrides on the command line.
    fn overrides(&self, config_file: Option<PathBuf>) -> Overrides {
        let mut overrides = Overrides {
            config_file,
            ..Overrides::default()
        };
        match self {
            Self::Serve { listen, backend } => {
                overrides.listen = *listen;
                overrides.blob_root.clone_from(&backend.blob_root);
                overrides.memory = backend.memory;
            }
            Self::Gc {
                grace_window_hours,
                backend,
                ..
            } => {
                overrides.grace_window_hours = *grace_window_hours;
                overrides.blob_root.clone_from(&backend.blob_root);
                overrides.memory = backend.memory;
            }
            Self::Purge { backend, .. } | Self::Scrub { backend, .. } => {
                overrides.blob_root.clone_from(&backend.blob_root);
                overrides.memory = backend.memory;
            }
            Self::GenOpenapi { .. } => {}
        }
        overrides
    }
}

/// Parse the command line, install the log stream, and do what was asked.
///
/// # Errors
///
/// Returns whatever the subcommand could not finish. A configuration refusal is **not** an
/// error here — it is [`EXIT_MISCONFIGURED`] with its own report on stderr, because
/// `ConfigError` already renders the full list of faults and an error chain around it would bury
/// them.
pub async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let environment = ProcessEnvironment;
    let overrides = cli.command.overrides(cli.config.clone());

    install_tracing(&environment, &overrides);

    let config = match Config::load(&environment, &overrides, cli.command.demands()) {
        Ok(config) => config,
        Err(error) => {
            // Straight to stderr rather than through `tracing`: a startup refusal has to be
            // visible whatever `RUST_LOG` says, and this is the one message an operator who
            // mis-typed a variable needs to read.
            eprintln!("capsule-server: {error}");
            return Ok(ExitCode::from(EXIT_MISCONFIGURED));
        }
    };

    match cli.command {
        Command::Serve { .. } => serve(&config).await,
        Command::Gc { apply, .. } => collect(&config, mode(apply)).await,
        Command::Purge { apply, limit, .. } => purge(&config, mode(apply), limit).await,
        Command::Scrub { deep, budget, .. } => {
            let depth = if deep {
                Depth::Deep { budget }
            } else {
                Depth::Structural
            };
            scrub(&config, depth).await
        }
        // The document is a property of the router's types, so the configuration is loaded only
        // to refuse `--config` and is deliberately not logged: `mise run openapi-check-kynos` is
        // a check gate, and a settings dump on its stderr is noise in every CI log that runs it.
        Command::GenOpenapi { output, check } => {
            drop(config);
            gen_openapi(&output, check)
        }
    }
}

/// Assemble the server from `config`, logging what it came up on.
///
/// Shared by every subcommand that needs a store, so the settings dump an operator reads after
/// an incident is written once and says the same thing whichever command produced it.
async fn assemble(config: &Config) -> Result<Assembled> {
    tracing::debug!(?config, "loaded the configuration");
    Ok(boot::assemble(config).await?)
}

/// Assemble only what the operator workers read.
///
/// A separate entry point rather than reaching into [`Assembled`], because `gc`, `purge` and
/// `scrub` need **no key material** and the way to make that true is for the assembly they use
/// to have none in scope — not for it to build a token signer and then not use it.
async fn maintenance(config: &Config) -> Result<Maintenance> {
    tracing::debug!(?config, "loaded the configuration");
    Ok(boot::assemble_maintenance(config).await?)
}

/// Accept requests until a termination signal, then drain.
///
/// # The bound address goes to stdout
///
/// It is logged at `INFO` **and** written to stdout as one `listening on <url>` line. That is
/// not a duplicate: `--listen 127.0.0.1:0` is a request for the operating system to choose a
/// port, and a caller that asked for that has no other way to learn which one it got. Making
/// them parse a log format — which `LOG_FORMAT` can change under them — would be a contract
/// nobody wrote down. Rust's stdout is line-buffered, so the line is readable the moment it is
/// written.
///
/// # No TLS
///
/// `design/cryptography/failure-modes.md` is explicit that "application servers do not terminate
/// TLS", and scopes in-code TLS to the SDK client, LAN peering and server-to-server egress —
/// none of which is this listener. Kynos's `tls` feature stays off, so a certificate cannot be
/// configured by accident.
async fn serve(config: &Config) -> Result<ExitCode> {
    let assembled = assemble(config).await?;
    let bound = Server::new(assembled.service()?)
        .bind(config.listen)
        // SIGINT and SIGTERM, with a second one forcing. Kynos keeps its listeners alive
        // through the drain, so an impatient operator's second Ctrl-C is honoured rather than
        // ignored.
        .graceful_shutdown(Shutdown::signals())
        .shutdown_timeout(config.shutdown_timeout)
        .max_connections(config.max_connections)
        .prepare()
        .await
        .map_err(|error| eyre!("binding {}: {error}", config.listen))?;

    for address in bound.local_addrs() {
        tracing::info!(%address, "listening");
        println!("listening on http://{address}");
    }

    bound
        .serve()
        .await
        .map_err(|error| eyre!("serving: {error}"))?;
    tracing::info!("drained and stopped");
    Ok(ExitCode::SUCCESS)
}

/// Install the log stream, on stderr.
///
/// The format is read best-effort — [`Demands::Nothing`] never fails on a missing setting, and a
/// malformed one falls back to the build profile's default — because the configuration error
/// this cannot read has to be *reported*, and reporting it needs a subscriber.
fn install_tracing(environment: &dyn Environment, overrides: &Overrides) {
    let format = Config::load(environment, overrides, Demands::Nothing).map_or_else(
        |_| {
            if cfg!(debug_assertions) {
                LogFormat::Pretty
            } else {
                LogFormat::Json
            }
        },
        |config| config.log_format,
    );

    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| {
            if cfg!(debug_assertions) {
                EnvFilter::try_new("debug")
            } else {
                EnvFilter::try_new("info")
            }
        })
        .expect("built-in log filter directives are valid");

    let registry = tracing_subscriber::registry().with(filter);
    match format {
        LogFormat::Json => registry
            .with(
                log_fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_writer(std::io::stderr),
            )
            .init(),
        LogFormat::Pretty => registry
            .with(
                log_fmt::layer()
                    .pretty()
                    .with_file(true)
                    .with_line_number(true)
                    .with_writer(std::io::stderr),
            )
            .init(),
    }
}

/// Whether `--apply` was passed.
///
/// A free function rather than a `From<bool>` on [`Mode`]: the boolean is a command-line flag,
/// and a blanket conversion would let any `bool` in the crate become a write mode.
fn mode(apply: bool) -> Mode {
    if apply { Mode::Apply } else { Mode::DryRun }
}

/// Sweep blobs nothing references any more.
///
/// # What an interrupted pass leaves, and what the operator is told
///
/// `gc::collect` returns `Result<CollectionReport, StoreError>`, so a pass that fails part-way
/// returns **no report at all** — the work it did before the failure is not recoverable from
/// here, and stdout stays empty. What the operator sees is the store error on stderr and a
/// non-zero exit; what they have to do to find out how far it got is read the `INFO` lines the
/// collector logs as it marks and sweeps.
///
/// That is a real gap and it is the library's to close: the report would have to come back
/// alongside the error (`Result<(CollectionReport, Option<StoreError>), _>` or equivalent), and
/// `gc/mod.rs` is not this change's to edit. The state left behind is safe either way, by the
/// module's own argument — a mark is reversible, and a sweep only ever removed a blob confirmed
/// unreferenced twice — so re-running the pass is the correct response to one that stopped.
async fn collect(config: &Config, mode: Mode) -> Result<ExitCode> {
    let maintenance = maintenance(config).await?;
    let report = crate::gc::collect(&maintenance.collection, mode)
        .await
        .map_err(|error| eyre!("the collection pass could not finish: {error}"))?;
    print!("{}", render_collection(&report, mode));
    Ok(ExitCode::SUCCESS)
}

/// Drop the blob references of tombstoned assets past their retention window.
///
/// An interrupted pass reports nothing, for the reason [`collect`] records.
async fn purge(config: &Config, mode: Mode, limit: usize) -> Result<ExitCode> {
    let maintenance = maintenance(config).await?;
    let report = crate::gc::purge_expired(&maintenance.collection, mode, limit)
        .await
        .map_err(|error| eyre!("the retention purge could not finish: {error}"))?;
    print!("{}", render_purge(&report, mode));
    Ok(ExitCode::SUCCESS)
}

/// Compare the index against the store.
///
/// Exits [`EXIT_FINDINGS`] on a non-empty report, which `design/filesystem/maintenance.md`
/// requires of it — and a **truncated** deep pass is not clean even with no findings, because it
/// did not finish looking. `ScrubReport::is_clean` already draws that distinction; this only has
/// to honour it.
async fn scrub(config: &Config, depth: Depth) -> Result<ExitCode> {
    let maintenance = maintenance(config).await?;
    let report = crate::scrub::scrub(&maintenance.scrub, depth)
        .await
        .map_err(|error| eyre!("the integrity scrub could not finish: {error}"))?;
    print!("{}", render_scrub(&report));
    Ok(if report.is_clean() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(EXIT_FINDINGS)
    })
}

/// One pass's report, rendered for a person.
///
/// A [`std::fmt::Display`] wrapper rather than a `-> String` helper, so every line is one `writeln!`
/// into the caller's formatter: building the whole report in a `String` first meant an
/// allocation per line and a clippy lint saying so.
struct Rendered<'a, T>(&'a T, Mode);

/// What a dry run prints above its report, so nobody reads one as an action.
fn posture(mode: Mode) -> &'static str {
    match mode {
        Mode::Apply => "applied",
        Mode::DryRun => "dry run — nothing was changed",
    }
}

/// Render a collection pass.
///
/// Every class names its blobs rather than counting them. [`CollectionReport`]'s own docs say
/// why: "a count tells an operator that something happened without telling them what to look
/// at." Empty classes are omitted, so a quiet pass is one short line rather than six zeroes.
fn render_collection(report: &CollectionReport, mode: Mode) -> Rendered<'_, CollectionReport> {
    Rendered(report, mode)
}

impl std::fmt::Display for Rendered<'_, CollectionReport> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self(report, mode) = *self;
        writeln!(f, "garbage collection ({})", posture(mode))?;
        let mut quiet = true;
        for (class, addresses) in [
            ("marked", &report.marked),
            ("unmarked", &report.unmarked),
            ("swept", &report.swept),
            ("reprieved", &report.reprieved),
            ("dangling", &report.dangling),
        ] {
            if addresses.is_empty() {
                continue;
            }
            quiet = false;
            writeln!(f, "  {class} ({})", addresses.len())?;
            for address in addresses {
                writeln!(f, "    {address}")?;
            }
        }
        if !report.credited.is_empty() {
            quiet = false;
            let total: u64 = report.credited.iter().map(|(_, bytes)| *bytes).sum();
            writeln!(
                f,
                "  credited ({} accounts, {total} bytes)",
                report.credited.len()
            )?;
            for (user, bytes) in &report.credited {
                writeln!(f, "    {user} {bytes}")?;
            }
        }
        if quiet {
            writeln!(f, "  nothing to do")?;
        }
        Ok(())
    }
}

/// Render a retention purge.
///
/// `retained` is reported as well as `purged`, because "why has this not gone yet" is exactly
/// the question a dry run is run to answer.
fn render_purge(report: &PurgeReport, mode: Mode) -> Rendered<'_, PurgeReport> {
    Rendered(report, mode)
}

impl std::fmt::Display for Rendered<'_, PurgeReport> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Self(report, mode) = *self;
        writeln!(f, "retention purge ({})", posture(mode))?;
        let mut quiet = true;
        for (class, assets) in [("purged", &report.purged), ("retained", &report.retained)] {
            if assets.is_empty() {
                continue;
            }
            quiet = false;
            writeln!(f, "  {class} ({})", assets.len())?;
            for asset in assets {
                writeln!(f, "    {asset}")?;
            }
        }
        if quiet {
            writeln!(f, "  nothing to do")?;
        }
        Ok(())
    }
}

/// Render an integrity scrub.
///
/// A scrub never writes, so it has no posture; the mode is carried and ignored.
fn render_scrub(report: &ScrubReport) -> Rendered<'_, ScrubReport> {
    Rendered(report, Mode::DryRun)
}

/// Grouped by the class an operator alerts on, and each finding printed through its **own**
/// `Debug`. Not a hand-written line per variant: every `Finding` variant already carries both
/// sides' evidence, a second rendering would be a second place for the two to disagree, and a
/// variant added later would otherwise render as nothing at all — which is the one failure mode
/// a report must not have.
impl std::fmt::Display for Rendered<'_, ScrubReport> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let report = self.0;
        writeln!(
            f,
            "integrity scrub ({} finding{}, {} bytes hashed{})",
            report.findings.len(),
            if report.findings.len() == 1 { "" } else { "s" },
            report.bytes_hashed,
            if report.budget_exhausted {
                ", budget exhausted — the pass did not finish looking"
            } else {
                ""
            }
        )?;
        for (class, count) in report.counts() {
            writeln!(f, "  {class} ({count})")?;
            for finding in report
                .findings
                .iter()
                .filter(|finding| finding.class() == class)
            {
                writeln!(f, "    {finding:?}")?;
            }
        }
        if report.is_clean() {
            writeln!(f, "  the index and the store agree")?;
        }
        Ok(())
    }
}

/// Write, or verify, the committed OpenAPI 3.2 document (slice `S-C34`).
///
/// The drift guard for the rebuild's central claim: that the description is derived from the
/// types and cannot disagree with them. That claim is already enforced *inside* the crate —
/// `assert_conformance` catches a response the document did not predict, and
/// `assert_declared_responses_covered` catches a promise no test produced. Neither helps a
/// **client**: a surface can be ported, the emitted document can change shape, and nothing
/// outside the crate notices until somebody regenerates by hand. This is what makes such a
/// change fail.
fn gen_openapi(output: &PathBuf, check: bool) -> Result<ExitCode> {
    let document =
        crate::openapi().map_err(|e| color_eyre::eyre::eyre!("describing the router: {e}"))?;
    // `to_json` is already pretty-printed; the trailing newline keeps the committed document an
    // ordinary text file rather than a one-line blob in a diff.
    let mut json = document
        .to_json()
        .wrap_err("serializing the OpenAPI document to JSON")?;
    json.push('\n');

    if check {
        let committed = std::fs::read_to_string(output)
            .wrap_err_with(|| format!("cannot read committed document at {}", output.display()))?;
        if committed != json {
            bail!(
                "OpenAPI document at {} is out of sync with the server; run \
                 `mise run openapi-kynos` and commit the result",
                output.display()
            );
        }
        println!("OpenAPI document is up to date: {}", output.display());
    } else {
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(output, &json).wrap_err_with(|| format!("writing {}", output.display()))?;
        println!("Wrote {}", output.display());
    }

    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use clap::{CommandFactory as _, Parser as _};

    use super::{
        Cli, CollectionReport, Command, Mode, PurgeReport, ScrubReport, mode, render_collection,
        render_purge, render_scrub,
    };

    #[test]
    fn the_command_line_is_well_formed() {
        // clap's own consistency check: a duplicate long flag, a subcommand with two positional
        // arguments in the wrong order, or an argument whose value name collides is a panic
        // here rather than a report from the first operator to run `--help`.
        Cli::command().debug_assert();
    }

    #[test]
    fn gen_openapi_defaults_to_the_committed_document() {
        let cli = Cli::parse_from(["capsule-server", "gen-openapi"]);
        let Command::GenOpenapi { output, check } = cli.command else {
            panic!("that is the subcommand that was parsed")
        };
        assert_eq!(output, std::path::Path::new("capsule-server/openapi.json"));
        assert!(!check, "writing is the default; checking is opt-in");
    }

    #[test]
    fn serve_carries_its_flags_into_the_overrides_the_loader_reads() {
        // The command line's half of the precedence table. A flag that parsed but never reached
        // `Config::load` would be a flag that silently does nothing.
        let cli = Cli::parse_from([
            "capsule-server",
            "serve",
            "--memory",
            "--listen",
            "127.0.0.1:6000",
            "--blob-root",
            "/var/lib/capsule/blobs",
        ]);
        let overrides = cli.command.overrides(None);
        assert!(overrides.memory);
        assert_eq!(
            overrides.listen,
            Some("127.0.0.1:6000".parse().expect("a literal address parses"))
        );
        assert_eq!(
            overrides.blob_root.as_deref(),
            Some(std::path::Path::new("/var/lib/capsule/blobs"))
        );
    }

    #[test]
    fn serving_demands_a_key_and_describing_the_router_demands_nothing() {
        assert_eq!(
            Cli::parse_from(["capsule-server", "serve"])
                .command
                .demands(),
            crate::config::Demands::Serve
        );
        assert_eq!(
            Cli::parse_from(["capsule-server", "gen-openapi"])
                .command
                .demands(),
            crate::config::Demands::Nothing
        );
    }

    #[test]
    fn a_config_path_is_accepted_by_the_parser_so_it_can_be_refused_by_the_loader() {
        let cli = Cli::parse_from([
            "capsule-server",
            "--config",
            "/etc/capsule.toml",
            "gen-openapi",
        ]);
        assert_eq!(
            cli.config.as_deref(),
            Some(std::path::Path::new("/etc/capsule.toml"))
        );
    }

    /// A well-formed content address, distinguished by `seed`.
    fn address(seed: u8) -> crate::blob::ContentAddress {
        let hex: String = std::iter::repeat_n(format!("{seed:02x}"), 32).collect();
        crate::blob::ContentAddress::parse(&hex).expect("an address")
    }

    #[test]
    fn a_quiet_collection_pass_is_one_short_line() {
        // Six zeroes would be six lines an operator learns to skip, and the whole point of the
        // report is that they read it.
        let rendered = render_collection(&CollectionReport::default(), Mode::DryRun).to_string();
        assert!(rendered.contains("dry run"), "{rendered}");
        assert!(rendered.contains("nothing to do"), "{rendered}");
    }

    #[test]
    fn a_collection_pass_names_the_blobs_rather_than_counting_them() {
        // `CollectionReport`'s own docs: "a count tells an operator that something happened
        // without telling them what to look at."
        let report = CollectionReport {
            marked: vec![address(0xAA), address(0xBB)],
            swept: vec![address(0xCC)],
            credited: vec![(crate::store::UserId::new("a-user"), 4096)],
            ..CollectionReport::default()
        };
        let rendered = render_collection(&report, Mode::Apply).to_string();
        assert!(rendered.contains("applied"), "{rendered}");
        assert!(rendered.contains("marked (2)"), "{rendered}");
        assert!(rendered.contains(&address(0xAA).to_string()), "{rendered}");
        assert!(rendered.contains(&address(0xBB).to_string()), "{rendered}");
        assert!(rendered.contains("swept (1)"), "{rendered}");
        assert!(
            rendered.contains("credited (1 accounts, 4096 bytes)"),
            "{rendered}"
        );
        // Classes with nothing in them are omitted rather than printed as zero.
        assert!(!rendered.contains("unmarked"), "{rendered}");
        assert!(!rendered.contains("nothing to do"), "{rendered}");
    }

    #[test]
    fn a_purge_reports_what_is_still_waiting() {
        // "Why has this not gone yet" is exactly the question a dry run is run to answer.
        let report = PurgeReport {
            purged: vec![crate::store::AssetId::new("gone")],
            retained: vec![crate::store::AssetId::new("waiting")],
        };
        let rendered = render_purge(&report, Mode::DryRun).to_string();
        assert!(rendered.contains("purged (1)"), "{rendered}");
        assert!(rendered.contains("gone"), "{rendered}");
        assert!(rendered.contains("retained (1)"), "{rendered}");
        assert!(rendered.contains("waiting"), "{rendered}");
    }

    #[test]
    fn a_clean_scrub_says_the_two_sides_agree() {
        let rendered = render_scrub(&ScrubReport::default()).to_string();
        assert!(rendered.contains("0 findings"), "{rendered}");
        assert!(rendered.contains("agree"), "{rendered}");
    }

    #[test]
    fn a_scrub_groups_findings_by_the_class_an_operator_alerts_on() {
        let report = ScrubReport {
            findings: vec![
                crate::scrub::Finding::Orphan {
                    address: address(0xAA),
                },
                crate::scrub::Finding::Orphan {
                    address: address(0xBB),
                },
                crate::scrub::Finding::Debris {
                    path: "blobs/aa/not-a-blob".to_owned(),
                },
            ],
            bytes_hashed: 0,
            budget_exhausted: false,
        };
        let rendered = render_scrub(&report).to_string();
        assert!(rendered.contains("3 findings"), "{rendered}");
        assert!(rendered.contains("orphan (2)"), "{rendered}");
        assert!(rendered.contains("debris (1)"), "{rendered}");
        assert!(rendered.contains("not-a-blob"), "{rendered}");
        assert!(!rendered.contains("agree"), "{rendered}");
    }

    #[test]
    fn a_truncated_deep_pass_is_not_a_clean_one() {
        // A clean report from a pass that stopped early is the one answer a scrub must never
        // give, so the rendering says so out loud as well.
        let report = ScrubReport {
            findings: Vec::new(),
            bytes_hashed: 1024,
            budget_exhausted: true,
        };
        let rendered = render_scrub(&report).to_string();
        assert!(rendered.contains("budget exhausted"), "{rendered}");
        assert!(!rendered.contains("agree"), "{rendered}");
    }

    #[test]
    fn dry_run_is_the_default_for_the_two_subcommands_that_write() {
        // The first thing an operator does with a collector is find out what it thinks.
        let gc = Cli::parse_from(["capsule-server", "gc"]).command;
        assert!(matches!(gc, Command::Gc { apply: false, .. }));
        let purge = Cli::parse_from(["capsule-server", "purge"]).command;
        assert!(matches!(purge, Command::Purge { apply: false, .. }));
        assert_eq!(mode(false), Mode::DryRun);
        assert_eq!(mode(true), Mode::Apply);
    }

    #[test]
    fn a_structural_scrub_is_the_default_and_deep_carries_a_budget() {
        let shallow = Cli::parse_from(["capsule-server", "scrub"]).command;
        assert!(matches!(shallow, Command::Scrub { deep: false, .. }));
        let deep = Cli::parse_from(["capsule-server", "scrub", "--deep"]).command;
        let Command::Scrub { deep, budget, .. } = deep else {
            panic!("that is the subcommand that was parsed")
        };
        assert!(deep);
        assert_eq!(budget, super::DEFAULT_SCRUB_BUDGET);
    }

    #[test]
    fn the_operator_commands_demand_a_blob_root_and_no_key_material() {
        for argv in [
            ["capsule-server", "gc"],
            ["capsule-server", "purge"],
            ["capsule-server", "scrub"],
        ] {
            assert_eq!(
                Cli::parse_from(argv).command.demands(),
                crate::config::Demands::Maintenance,
                "{argv:?}"
            );
        }
    }
}
