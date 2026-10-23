//! The panic hook the TUI runs under.
//!
//! ratatui's own hook restores the terminal on any panic in the process. An effect runs on a
//! spawned task whose panic the runtime catches and the event loop turns into a failure action
//! (see [`super::runtime`]), so the loop carries on: a hook that restored the terminal for that
//! panic would leave the loop drawing on the main screen. [`init_terminal`] therefore replaces
//! ratatui's hook with one that restores the terminal only for a panic on the thread that owns
//! it, and only logs the rest, to the TUI log rather than over the frame.

use std::io;
use std::panic::{self, PanicHookInfo};
use std::thread::{self, ThreadId};

use ratatui::DefaultTerminal;

type Hook = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

/// Set up the terminal for the calling thread, which becomes the one a panic restores it for.
///
/// # Errors
///
/// The terminal could not be set up.
pub fn init_terminal() -> io::Result<DefaultTerminal> {
    // Taken before ratatui wraps it, so the hook installed first (colour-eyre's) still runs.
    let prior = panic::take_hook();
    let terminal = ratatui::try_init()?;
    // Drop ratatui's hook, which restores the terminal for any thread's panic.
    drop(panic::take_hook());
    panic::set_hook(owner_thread_hook(
        thread::current().id(),
        || {
            let _ = ratatui::try_restore();
        },
        prior,
    ));
    Ok(terminal)
}

/// A hook that runs `restore` then `prior` for a panic on `owner`, and only logs a panic on any
/// other thread: a contained task panic must not touch the terminal.
fn owner_thread_hook(
    owner: ThreadId,
    restore: impl Fn() + Send + Sync + 'static,
    prior: Hook,
) -> Hook {
    Box::new(move |info| {
        if thread::current().id() == owner {
            restore();
            prior(info);
        } else {
            tracing::error!(target: "capsule_cli::tui", panic = %info, "a task panicked; the TUI carries on");
        }
    })
}

#[cfg(test)]
mod tests;
