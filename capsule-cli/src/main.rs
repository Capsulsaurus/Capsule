//! The `capsule` binary: a thin shim over the [`capsule_cli`] library, which owns
//! the command implementations (so the real-server round-trip can drive them
//! directly). This binary only installs error reporting + tracing and dispatches. Logs go to
//! stderr: stdout carries only what a command prints, which for `--json` is one document.

use eyre::Result;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::prelude::*;

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
    // the layers send events to its log file instead (without colour), so none is drawn over a
    // frame.
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(capsule_cli::tui::log::layers())
        .init();

    capsule_cli::run().await
}
