//! Deterministic dump of the `capsule` command tree (slice `S-Z8`).
//!
//! Serializes [`capsule_cli::cli::command_tree`] to `capsule-cli/cli-surface.json` and, with
//! `--check`, fails when the committed copy is stale. It is the CLI's half of the rule that
//! artifacts cross the toolchain boundary rather than toolchains (`design/developer-docs.md`):
//! the documentation build installs bun and nothing else, so it cannot ask cargo what
//! `capsule --help` says. It reads this file instead, and this binary is what makes a drifted
//! file fail CI.
//!
//! Deliberately the same shape as `capsule-server/src/bin/gen_openapi.rs` — `[FILE]` default,
//! `--check`, byte comparison, trailing newline — because two description artifacts that are
//! refreshed and gated differently are two things to remember instead of one.
//!
//! It needs no database, no library, no key material, no network: `command_tree()` walks a
//! `clap::Command` built from compile-time attributes. That is what lets `--check` run in the
//! Rust check gate beside `openapi-check-kynos`.
//!
//! ## Why this binary prints no prose
//!
//! `xtask i18n-guard` scans `capsule-cli/src/**` for string literals passed to
//! `print`/`println`/`eprint`/`eprintln`/`eyre`/`bail`, and `locales/i18n-guard-allowlist.txt`
//! says in as many words not to add a CLI line to make new output pass. That rule is right for
//! the `capsule` binary, which renders prose to a user in their own language. This binary is CI
//! tooling: its audience is a developer reading a task's output, and routing a build tool's
//! status line through `locales/` would put a string no user can reach into every translation
//! catalog. So it says what it has to say with a path and an exit code — success writes the
//! path it wrote, `--check` is silent on success as `cargo fmt --check` is, and the stale-file
//! message is built with `format!` and carried by the `Result` that `color_eyre` reports.
//!
//! Usage:
//! - `gen_cli_surface [FILE]` writes the document (default `capsule-cli/cli-surface.json`).
//! - `gen_cli_surface --check [FILE]` fails if the committed document is stale, writing nothing.

use std::path::PathBuf;

use clap::Parser;
use color_eyre::eyre::{Context, Report, Result};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// Output path for the command-tree document (relative to the repo root).
    #[arg(value_name = "FILE", default_value = "capsule-cli/cli-surface.json")]
    output: PathBuf,

    /// Verify the committed document is up to date instead of writing it (CI drift gate).
    #[arg(long)]
    check: bool,
}

fn main() -> Result<()> {
    color_eyre::install()?;
    let cli = Cli::parse();

    // Pretty-printed with a trailing newline: the artifact is reviewed as a diff, so a
    // one-line blob would hide exactly the change a reviewer is there to see.
    let mut json = serde_json::to_string_pretty(&capsule_cli::cli::command_tree())
        .wrap_err("serializing the command tree to JSON")?;
    json.push('\n');

    if cli.check {
        let committed = std::fs::read_to_string(&cli.output).wrap_err_with(|| {
            format!("cannot read committed document at {}", cli.output.display())
        })?;
        if committed != json {
            return Err(Report::msg(format!(
                "the command-tree document at {} is out of sync with the `capsule` argument \
                 surface; run `mise run cli-surface` and commit the result",
                cli.output.display()
            )));
        }
    } else {
        if let Some(parent) = cli.output.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&cli.output, &json)
            .wrap_err_with(|| format!("writing {}", cli.output.display()))?;
        println!("{}", cli.output.display());
    }

    Ok(())
}
