//! The TUI's side effects, behind one port trait.
//!
//! The reducer names the work it wants as an [`Effect`]; [`perform`] runs it against a
//! [`Ports`] implementation and turns the outcome into the [`Action`] that reports it. The real
//! terminal uses [`LocalPorts`]; tests hand in a mock so the same reducer, keymap and view run
//! without a session file, a library or a network.

use std::future::Future;
use std::path::PathBuf;

use super::state::{Action, Effect, SessionSummary};
use crate::session::SessionStore;
use crate::status::AuthStatus;

/// Everything the TUI asks of the outside world.
///
/// Each method returns its failure as the English reason the screen shows: the TUI is
/// English-only, and a port error is something a person reads, not something the reducer
/// branches on.
pub trait Ports: Send + Sync + 'static {
    /// The local session summary, as `capsule auth status` reads it: never a network call.
    fn session_summary(&self) -> impl Future<Output = Result<SessionSummary, String>> + Send;
}

/// Perform `effect` through `ports` and return the action that reports its outcome.
pub async fn perform<P: Ports>(ports: &P, effect: Effect) -> Action {
    tracing::debug!(target: "capsule_cli::tui", ?effect, "performing effect");
    let action = match effect {
        Effect::LoadSession => Action::SessionLoaded(ports.session_summary().await),
    };
    tracing::debug!(target: "capsule_cli::tui", ?effect, ?action, "effect finished");
    action
}

/// The ports a real `capsule` run uses: the session file the CLI's own commands read.
#[derive(Debug, Clone)]
pub struct LocalPorts {
    session_path: Option<PathBuf>,
}

impl LocalPorts {
    /// Ports over the session file at `session_path`; `None` when no config directory could
    /// be resolved, which every read then reports.
    #[must_use]
    pub fn new(session_path: Option<PathBuf>) -> Self {
        Self { session_path }
    }
}

impl Ports for LocalPorts {
    async fn session_summary(&self) -> Result<SessionSummary, String> {
        let path = self
            .session_path
            .clone()
            .ok_or_else(|| "could not resolve the config directory".to_owned())?;
        // A file read: off the async worker so the input loop is never behind it.
        let status =
            tokio::task::spawn_blocking(move || AuthStatus::check(&SessionStore::new(path)))
                .await
                .map_err(|e| format!("the session read did not finish: {e}"))?
                .map_err(|e| format!("could not read the session: {e}"))?;
        Ok(if status.signed_in {
            SessionSummary::SignedIn {
                access_token_fresh: status.access_token_fresh,
            }
        } else {
            SessionSummary::SignedOut
        })
    }
}

#[cfg(test)]
mod tests;
