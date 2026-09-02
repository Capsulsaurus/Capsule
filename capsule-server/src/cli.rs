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

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use color_eyre::eyre::{Context as _, Result, bail};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

use crate::config::{Config, Demands, Environment, LogFormat, Overrides, ProcessEnvironment};

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

/// The things this binary does.
#[derive(Debug, Subcommand)]
pub enum Command {
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
            Self::GenOpenapi { .. } => Demands::Nothing,
        }
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
    let overrides = Overrides {
        config_file: cli.config.clone(),
        ..Overrides::default()
    };

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
        // The document is a property of the router's types, so the configuration is loaded only
        // to refuse `--config` and is deliberately not logged: `mise run openapi-check-kynos` is
        // a check gate, and a settings dump on its stderr is noise in every CI log that runs it.
        Command::GenOpenapi { output, check } => {
            drop(config);
            gen_openapi(&output, check)
        }
    }
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
                fmt::layer()
                    .json()
                    .flatten_event(true)
                    .with_writer(std::io::stderr),
            )
            .init(),
        LogFormat::Pretty => registry
            .with(
                fmt::layer()
                    .pretty()
                    .with_file(true)
                    .with_line_number(true)
                    .with_writer(std::io::stderr),
            )
            .init(),
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

    use super::{Cli, Command};

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
        let Command::GenOpenapi { output, check } = cli.command;
        assert_eq!(output, std::path::Path::new("capsule-server/openapi.json"));
        assert!(!check, "writing is the default; checking is opt-in");
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
}
