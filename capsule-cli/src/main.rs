//! The `capsule` binary: a thin shim over the [`capsule_cli`] library, which owns
//! the command implementations (so the real-server round-trip can drive them
//! directly). This binary only installs error reporting + tracing and dispatches. Logs go to
//! stderr: stdout carries only what a command prints, which for `--json` is one document.

use eyre::Result;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

#[tokio::main]
async fn main() -> Result<()> {
    color_eyre::install()?;

    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| {
            if cfg!(debug_assertions) {
                EnvFilter::try_new("debug")
            } else {
                EnvFilter::try_new("info")
            }
        })
        .expect("built-in log filter directives are valid");
    // stdout is a data channel: `--json` promises exactly one document there, so every log
    // event goes to stderr, whatever the filter lets through. While the TUI holds the terminal,
    // the writer sends events to its log file instead, so none is drawn over a frame.
    let fmt_layer = fmt::layer()
        .with_writer(capsule_cli::tui::LogWriter)
        .pretty()
        .with_thread_ids(true)
        .with_thread_names(true)
        .with_file(true)
        .with_line_number(true);
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();

    capsule_cli::run().await
}
