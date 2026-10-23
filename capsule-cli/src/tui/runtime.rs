//! The event loop: terminal events and finished effects in, frames out.
//!
//! [`event_loop`] is generic over the backend and the event source so a test drives the same
//! loop a terminal does, on a `TestBackend` with a scripted stream. Its one rule is the
//! responsiveness bound (Clients — Responsiveness): after every event it applies the action and
//! draws, and it never awaits a port on the way. Effects run as spawned tasks whose outcomes
//! arrive later as actions on a channel.

use std::sync::Arc;

use eyre::{Result, WrapErr as _};
use futures::{Stream, StreamExt as _};
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::mpsc;

use super::keymap::action_for_event;
use super::ports::{Ports, perform};
use super::state::{Action, AppState, Effect};
use super::view::render;

/// Run the TUI until it quits or `events` ends, drawing on `terminal`. Returns the final state.
///
/// # Errors
///
/// The event source or the backend failed; the terminal is left for the caller to restore.
pub async fn event_loop<B, S, P>(
    terminal: &mut Terminal<B>,
    mut events: S,
    ports: Arc<P>,
) -> Result<AppState>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
    S: Stream<Item = std::io::Result<crossterm::event::Event>> + Unpin,
    P: Ports,
{
    let (mut state, effects) = AppState::init();
    let (tx, mut rx) = mpsc::unbounded_channel::<Action>();
    spawn_effects(&ports, &tx, effects);
    draw(terminal, &state)?;

    while state.is_running() {
        let action = tokio::select! {
            // Input first: a key press is never queued behind an effect's outcome.
            biased;
            event = events.next() => match event {
                Some(event) => {
                    let event = event.wrap_err("reading terminal input")?;
                    action_for_event(&state, &event)
                }
                None => {
                    tracing::info!(target: "capsule_cli::tui", "input closed; leaving the TUI");
                    break;
                }
            },
            // `tx` lives as long as the loop, so the channel never closes under it.
            Some(action) = rx.recv() => Some(action),
        };
        if let Some(action) = action {
            let effects = state.update(action);
            spawn_effects(&ports, &tx, effects);
        }
        draw(terminal, &state)?;
    }
    Ok(state)
}

/// Draw one frame of `state`.
fn draw<B>(terminal: &mut Terminal<B>, state: &AppState) -> Result<()>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    terminal
        .draw(|frame| render(frame, state))
        .wrap_err("drawing a frame")?;
    Ok(())
}

/// Start each effect on its own task; its outcome comes back on `tx` as an action.
fn spawn_effects<P: Ports>(
    ports: &Arc<P>,
    tx: &mpsc::UnboundedSender<Action>,
    effects: Vec<Effect>,
) {
    for effect in effects {
        let ports = Arc::clone(ports);
        let tx = tx.clone();
        tokio::spawn(async move {
            let action = perform(ports.as_ref(), effect).await;
            // The loop has quit and dropped the receiver: nobody is left to show the outcome.
            if tx.send(action).is_err() {
                tracing::debug!(target: "capsule_cli::tui", ?effect, "effect outcome dropped after quit");
            }
        });
    }
}

#[cfg(test)]
mod tests;
