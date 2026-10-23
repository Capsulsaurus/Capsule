//! Drawing [`AppState`] into a frame.
//!
//! [`render`] is a pure function of the state: the same state always draws the same frame, so a
//! frame is snapshotted on ratatui's `TestBackend` exactly as a terminal would show it. The TUI
//! is English-only, so its strings are inline here.
//!
//! Every frame has three bands: a one-line header (the product and the session state), the
//! screen's body, and a one-line footer listing the keys that work on this screen.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, Wrap};

use super::keymap::{BINDINGS, Scope, available};
use super::state::{AppState, Loadable, Screen, SessionSummary};

/// Draw `state` into `frame`.
pub fn render(frame: &mut Frame, state: &AppState) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_header(frame, header, state);
    match state.screen() {
        Screen::Home => render_home(frame, body, state),
        Screen::Help => render_help(frame, body),
    }
    render_footer(frame, footer, state);
}

/// The screen's title, as the header and its block print it.
fn title(screen: Screen) -> &'static str {
    match screen {
        Screen::Home => "Home",
        Screen::Help => "Key bindings",
    }
}

/// The session state in one short phrase and its colour.
fn session_badge(session: &Loadable<SessionSummary>) -> Span<'static> {
    match session {
        Loadable::Loading => "Session: loading…".fg(Color::DarkGray),
        Loadable::Ready(SessionSummary::SignedOut) => "Signed out".fg(Color::Yellow),
        Loadable::Ready(SessionSummary::SignedIn { .. }) => "Signed in".fg(Color::Green),
        Loadable::Failed(_) => "Session unavailable".fg(Color::Red),
    }
}

fn render_header(frame: &mut Frame, area: Rect, state: &AppState) {
    let line = Line::from(vec![
        "Capsule".bold(),
        " · ".into(),
        title(state.screen()).into(),
        " · ".into(),
        session_badge(state.session()),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn render_home(frame: &mut Frame, area: Rect, state: &AppState) {
    let session: Vec<Line> = match state.session() {
        Loadable::Loading => vec![Line::from("Reading the session…".fg(Color::DarkGray))],
        Loadable::Ready(SessionSummary::SignedOut) => vec![
            Line::from("Not signed in."),
            Line::from("Run `capsule auth login` to sign in to a server."),
        ],
        Loadable::Ready(SessionSummary::SignedIn { access_token_fresh }) => vec![
            Line::from("Signed in.".fg(Color::Green)),
            Line::from(if *access_token_fresh {
                "Access token: valid."
            } else {
                "Access token: expired; it refreshes on the next network operation."
            }),
        ],
        Loadable::Failed(reason) => vec![
            Line::from("The session could not be read.".fg(Color::Red)),
            Line::from(reason.clone()),
        ],
    };
    let mut lines = session;
    lines.push(Line::default());
    lines.push(Line::from(
        "Every flow is also a `capsule <command>` subcommand; `capsule --help` lists them.",
    ));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title(Screen::Home));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn render_help(frame: &mut Frame, area: Rect) {
    let rows = BINDINGS.iter().map(|b| {
        let scope = match b.scope {
            Scope::Global => "everywhere",
            Scope::On(screen) => title(screen),
        };
        Row::new(vec![b.label, b.description, scope])
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(8),
            Constraint::Min(20),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new(vec!["Key", "Action", "Where"]).style(Style::new().add_modifier(Modifier::BOLD)),
    )
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title(title(Screen::Help)),
    );
    frame.render_widget(table, area);
}

fn render_footer(frame: &mut Frame, area: Rect, state: &AppState) {
    let mut spans = Vec::new();
    for binding in BINDINGS
        .iter()
        .filter(|b| b.in_footer && available(b, state))
    {
        if !spans.is_empty() {
            spans.push("  ".into());
        }
        spans.push(binding.label.bold());
        spans.push(" ".into());
        spans.push(binding.description.fg(Color::DarkGray));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests;
