//! The full-screen terminal UI that bare `capsule` opens on a terminal.
//!
//! The TUI is a pure core with a thin shell around it, so everything but the terminal itself is
//! driven by tests:
//!
//! - [`state`] — [`AppState`], the [`Action`]s that change it, and the [`Effect`]s an action asks
//!   for. [`AppState::update`] is the reducer: pure, synchronous, no I/O.
//! - [`keymap`] — terminal input to actions through one binding table, which the help screen
//!   and the footer are drawn from too.
//! - [`ports`] — the [`Ports`] trait every side effect goes through, [`ports::perform`] which
//!   turns an effect into the action reporting it, and [`LocalPorts`] for a real run.
//! - [`view`] — [`view::render`], a pure function from state to frame.
//! - [`runtime`] — the event loop, generic over backend and event source.
//! - [`driver`] — a scripted key driver on a `TestBackend` that holds effects until the test
//!   lets them answer.
//! - [`transfer`] — the real-time transfer screen: a pure fold of the SDK's transfer event
//!   stream, the feed that stamps it, and its drawing.
//! - [`hook`] — the panic hook: restores the terminal only for a panic on the loop's thread.
//! - [`log`] — routes log events to a file while the TUI owns the terminal.
//! - [`form`] — the text forms the feature screens collect input with, with secrets kept out
//!   of the log.
//!
//! The feature screens, one module each: [`auth`] (sign in and out), [`sync`], [`restore`] and
//! [`share`] (issue and revoke share links).
//!
//! [`import`] picks sources and imports them; [`push`] pushes a library and shows each asset's
//! custody (uploaded, durable, receipted) as the run's event stream tells it, with the assets
//! that did not make it retryable.
//!
//! The CLI and TUI are English-only (ADR-0011): strings are inline English, not catalog keys.
//! Every flow the TUI offers stays reachable as a non-interactive `capsule` subcommand with
//! `--json`.

pub mod auth;
pub mod driver;
pub mod form;
pub mod hook;
pub mod import;
pub mod keymap;
pub mod log;
pub mod ports;
pub mod push;
pub mod restore;
pub mod runtime;
pub mod share;
pub mod state;
pub mod sync;
pub mod transfer;
pub mod view;

use std::io::IsTerminal as _;
use std::sync::Arc;

use eyre::{Result, WrapErr as _};

pub use self::log::LogWriter;
pub use self::ports::{LocalPorts, Ports};
pub use self::state::{Action, AppState, Effect, Screen};
use crate::utils::directories::{get_cache_dir, get_session_file_path};

/// The file name, under the CLI cache directory, that a TUI session's logs go to.
pub const LOG_FILE_NAME: &str = "tui.log";

/// Whether bare `capsule` should open the TUI: both stdin and stdout are terminals. Anything
/// else (a pipe, a redirect, a CI runner) gets the help text instead.
#[must_use]
pub fn is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// Open the TUI on the process's terminal and run it until the person quits.
///
/// The terminal is restored on every exit path, including a panic on the loop's own thread (a panicking effect task is contained instead) and an
/// error from the loop. Log events go to `tui.log` in the cache directory while it runs.
///
/// # Errors
///
/// The terminal could not be set up or restored, or the loop failed.
pub async fn run() -> Result<()> {
    // Logging to the terminal under the TUI would draw over it, so a TUI that cannot open its
    // log file does not start: the person gets the reason on stderr, and the subcommands still
    // work.
    let log_path = get_cache_dir()
        .ok_or_else(|| eyre::eyre!("could not resolve the cache directory for the TUI log"))?
        .join(LOG_FILE_NAME);
    let _redirect = log::redirect_to(&log_path)
        .wrap_err_with(|| format!("opening the TUI log at {}", log_path.display()))?;
    tracing::info!(target: "capsule_cli::tui", log = %log_path.display(), "opening the TUI");

    let ports = Arc::new(LocalPorts::new(get_session_file_path()));
    let mut terminal = match hook::init_terminal() {
        Ok(terminal) => terminal,
        Err(e) => {
            // Raw mode may already be on; leave the terminal as it was found.
            let _ = ratatui::try_restore();
            return Err(e).wrap_err("setting up the terminal");
        }
    };
    let outcome =
        runtime::event_loop(&mut terminal, crossterm::event::EventStream::new(), ports).await;
    let restored = ratatui::try_restore().wrap_err("restoring the terminal");
    tracing::info!(target: "capsule_cli::tui", ok = outcome.is_ok(), "TUI closed");
    outcome?;
    restored
}
