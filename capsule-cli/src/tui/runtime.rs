//! The event loop: terminal events and finished effects in, frames out.
//!
//! [`event_loop`] is generic over the backend and the event source so a test drives the same
//! loop a terminal does, on a `TestBackend` with a scripted stream. Its one rule is the
//! responsiveness bound (Clients — Responsiveness): after every event it applies the action and
//! draws, and it never awaits a port on the way. Effects run as spawned tasks whose outcomes
//! arrive later as actions on a channel.
//!
//! Transfer events are the exception to drawing after every event: a fast link publishes
//! thousands a second, so each is folded into the state as it arrives and the transfer screen
//! draws on a 60 Hz frame tick instead.

use std::sync::Arc;

use eyre::{Result, WrapErr as _};
use futures::{Stream, StreamExt as _};
use ratatui::Terminal;
use ratatui::backend::Backend;
use tokio::sync::mpsc;
use tokio::time::MissedTickBehavior;

use super::keymap::action_for_event;
use super::ports::{Ports, perform};
use super::state::{Action, AppState, Effect, Screen, canonical_library};
use super::transfer::{FRAME_INTERVAL, Feed};
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
    let mut feed = Feed::new(&ports.transfer_events());
    let mut frames = tokio::time::interval(FRAME_INTERVAL);
    frames.set_missed_tick_behavior(MissedTickBehavior::Skip);

    while state.is_running() {
        let (action, redraw) = tokio::select! {
            // Input first: a key press is never queued behind an effect's outcome.
            biased;
            event = events.next() => {
                let Some(event) = event else {
                    tracing::info!(target: "capsule_cli::tui", "input closed; leaving the TUI");
                    break;
                };
                let event = event.wrap_err("reading terminal input")?;
                (action_for_event(&state, &event), true)
            },
            // `tx` lives as long as the loop, so the channel never closes under it.
            Some(action) = rx.recv() => (Some(action), true),
            // The transfer screen redraws at 60 Hz, ahead of the feed so a busy stream never
            // starves it; other screens never tick.
            _ = frames.tick(), if state.screen() == Screen::Transfers => {
                (Some(Action::Tick(feed.now_ms())), true)
            },
            // Folded at once, drawn by the next frame tick.
            action = feed.next() => (Some(action), false),
        };
        if let Some(action) = action {
            state.resolve_libraries(&action, canonical_library);
            let effects = state.update(action);
            spawn_effects(&ports, &tx, effects);
        }
        if redraw {
            draw(terminal, &state)?;
        }
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
        // The effect runs on its own task so a panic in a port is caught by the runtime and
        // arrives here as a `JoinError`. It becomes the effect's failure action, so the screen
        // leaves its loading marker and shows the error instead of waiting forever.
        let performed = effect.clone();
        let work = tokio::spawn(async move { perform(ports.as_ref(), performed).await });
        tokio::spawn(async move {
            let action = match work.await {
                Ok(action) => action,
                Err(e) => {
                    tracing::error!(target: "capsule_cli::tui", ?effect, error = %e, panicked = e.is_panic(), "effect task did not finish");
                    effect.clone().failed(if e.is_panic() {
                        "the task panicked; see the TUI log".to_owned()
                    } else {
                        format!("the task did not finish: {e}")
                    })
                }
            };
            // The loop has quit and dropped the receiver: nobody is left to show the outcome.
            if tx.send(action).is_err() {
                tracing::debug!(target: "capsule_cli::tui", ?effect, "effect outcome dropped after quit");
            }
        });
    }
}

#[cfg(test)]
mod tests;
