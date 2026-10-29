//! A scripted key-event driver: the TUI without a terminal, stepped one input at a time.
//!
//! The driver runs the same keymap, reducer, ports and view the event loop does, on a
//! `TestBackend`, but it holds every requested effect until the test calls [`Driver::settle`].
//! That makes the order of input and effect outcomes the test's to choose, so a test can assert
//! what a frame shows *before* a port answers (the responsiveness bound: the press is visible
//! without waiting on the effect) and again after.

use std::collections::VecDeque;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;

use super::keymap::action_for_key;
use super::ports::{Ports, perform};
use super::state::{Action, AppState, Effect, canonical_library};
use super::view::render;

/// The TUI driven by a script instead of a terminal.
pub struct Driver<P: Ports> {
    state: AppState,
    ports: P,
    pending: VecDeque<Effect>,
    terminal: Terminal<TestBackend>,
}

impl<P: Ports> Driver<P> {
    /// A driver over `ports`, drawing on a `width`×`height` test terminal. The initial effects
    /// are pending, not performed: call [`settle`](Self::settle) to let them answer.
    #[must_use]
    pub fn new(ports: P, width: u16, height: u16) -> Self {
        let (state, effects) = AppState::init();
        let terminal = match Terminal::new(TestBackend::new(width, height)) {
            Ok(terminal) => terminal,
            Err(never) => match never {},
        };
        let mut driver = Self {
            state,
            ports,
            pending: effects.into(),
            terminal,
        };
        driver.draw();
        driver
    }

    /// The current state.
    #[must_use]
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// The effects requested and not yet performed, oldest first.
    #[must_use]
    pub fn pending(&self) -> Vec<Effect> {
        self.pending.iter().cloned().collect()
    }

    /// Press `key` and draw. Returns the action it sent, or `None` where no binding matched.
    pub fn press(&mut self, key: KeyEvent) -> Option<Action> {
        let action = action_for_key(&self.state, key);
        if let Some(action) = &action {
            self.apply(action.clone());
        }
        self.draw();
        action
    }

    /// Press each character of `keys` as an unmodified key, in order.
    pub fn type_keys(&mut self, keys: &str) {
        for c in keys.chars() {
            self.press(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
    }

    /// Apply `action` directly, as a finished effect would, and draw.
    pub fn dispatch(&mut self, action: Action) {
        self.apply(action);
        self.draw();
    }

    /// Perform every pending effect through the ports, oldest first, applying each outcome and
    /// drawing after it, until none is left (an outcome may request more).
    pub async fn settle(&mut self) {
        while let Some(effect) = self.pending.pop_front() {
            let action = perform(&self.ports, effect).await;
            self.apply(action);
            self.draw();
        }
    }

    /// Perform only the oldest pending effect, apply its outcome and draw; whatever the outcome
    /// asks for stays pending. Returns whether there was one. A test steps a two-effect flow
    /// with it, asserting the frame between the steps.
    pub async fn settle_one(&mut self) -> bool {
        let Some(effect) = self.pending.pop_front() else {
            return false;
        };
        let action = perform(&self.ports, effect).await;
        self.apply(action);
        self.draw();
        true
    }

    /// The last drawn frame as plain text: one line per row, trailing blanks trimmed.
    #[must_use]
    pub fn frame(&self) -> String {
        frame_text(self.terminal.backend().buffer())
    }

    fn apply(&mut self, action: Action) {
        self.state.resolve_libraries(&action, canonical_library);
        let effects = self.state.update(action);
        self.pending.extend(effects);
    }

    fn draw(&mut self) {
        let state = &self.state;
        match self.terminal.draw(|frame| render(frame, state)) {
            Ok(_) => {}
            Err(never) => match never {},
        }
    }
}

/// `buffer` as plain text: one line per row, trailing blanks trimmed, styles dropped. The form
/// frame snapshots are stored in.
#[must_use]
pub fn frame_text(buffer: &Buffer) -> String {
    let width = usize::from(buffer.area.width);
    let mut out = String::new();
    for row in buffer.content.chunks(width.max(1)) {
        let line: String = row.iter().map(ratatui::buffer::Cell::symbol).collect();
        out.push_str(line.trim_end());
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests;
