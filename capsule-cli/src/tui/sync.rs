//! The sync screen: drain the server's feed into the local store, as `capsule sync` does.
//!
//! The scriptable twin is `capsule sync --json`; both run [`crate::remote::sync`] over the same
//! session and local store, so a sync started here and one started from a script cannot differ.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::state::{Effect, Loadable};
use crate::remote::SyncSummary;

/// What the sync screen does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncAction {
    /// Start a sync; `from_start` discards the saved cursor (`capsule sync --force`).
    Run {
        /// Re-drain the feed from the start.
        from_start: bool,
    },
    /// [`SyncEffect::Run`] finished.
    Finished(Result<SyncSummary, String>),
}

/// The work the sync screen asks the ports for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncEffect {
    /// Drain the feed; answered by [`SyncAction::Finished`].
    Run {
        /// Re-drain the feed from the start.
        from_start: bool,
    },
}

impl SyncEffect {
    /// The action reporting this effect failing with `reason`.
    #[must_use]
    pub fn failed(self, reason: String) -> SyncAction {
        match self {
            Self::Run { .. } => SyncAction::Finished(Err(reason)),
        }
    }
}

/// The sync screen's state: the last run this session, if any.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SyncScreen {
    run: Option<Loadable<SyncSummary>>,
}

impl SyncScreen {
    /// The last run: `None` before the first, `Loading` while one is in flight.
    #[must_use]
    pub fn run(&self) -> Option<&Loadable<SyncSummary>> {
        self.run.as_ref()
    }

    /// Apply `action`; pure, as [`super::state::AppState::update`] is.
    pub fn update(&mut self, action: SyncAction) -> Vec<Effect> {
        match action {
            SyncAction::Run { from_start } => {
                // One run at a time: a second press while the first drains is not a second
                // drain racing the first over the same cursor.
                if self.run == Some(Loadable::Loading) {
                    return Vec::new();
                }
                self.run = Some(Loadable::Loading);
                vec![Effect::Sync(SyncEffect::Run { from_start })]
            }
            SyncAction::Finished(result) => {
                self.run = Some(match result {
                    Ok(summary) => Loadable::Ready(summary),
                    Err(reason) => Loadable::Failed(reason),
                });
                Vec::new()
            }
        }
    }
}

/// What a finished sync did, as the lines the screen shows: the same sentences `capsule sync`
/// prints.
#[must_use]
pub fn summary_lines(summary: &SyncSummary) -> Vec<Line<'static>> {
    let mut lines = vec![if summary.applied == 0 {
        Line::from("Already up to date.".fg(Color::Green))
    } else {
        Line::from(
            format!(
                "Sync complete: applied {} change(s) across {} album(s) in {} page(s).",
                summary.applied, summary.albums, summary.pages
            )
            .fg(Color::Green),
        )
    }];
    if summary.dry_run {
        lines.push(Line::from("Dry run: nothing was persisted."));
    }
    if summary.resynced {
        lines.push(Line::from(
            "The server rebuilt this library's index; resynced from the start.".fg(Color::Yellow),
        ));
        lines.push(Line::from(
            "Run `capsule sync reannounce --library PATH` to give it back what your library holds.",
        ));
    }
    lines
}

/// Draw the sync screen into `area`.
pub fn render(frame: &mut Frame, area: Rect, screen: &SyncScreen, title: &'static str) {
    let mut lines = vec![
        Line::from("Bring the albums and assets the server lists into this device's local store."),
        Line::default(),
    ];
    match screen.run() {
        None => lines.push(Line::from("Not synced yet in this session.")),
        Some(Loadable::Loading) => lines.push(Line::from("Syncing…".fg(Color::DarkGray))),
        Some(Loadable::Ready(summary)) => lines.extend(summary_lines(summary)),
        Some(Loadable::Failed(reason)) => {
            lines.push(Line::from("The sync did not finish.".fg(Color::Red)));
            lines.push(Line::from(reason.clone()));
        }
    }
    lines.push(Line::default());
    lines.push(Line::from(
        "Scriptable: `capsule sync --json` (add `--force` to start over).".fg(Color::DarkGray),
    ));
    let block = Block::default().borders(Borders::ALL).title(title);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests;
