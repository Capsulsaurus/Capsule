//! The restore screen: download originals from the server, verified byte for byte, into a
//! directory, as `capsule restore` does.
//!
//! The scriptable twin is `capsule restore --json`; both run [`crate::remote::restore`], which
//! drives [`crate::restore::restore_entries`]: every original is fetched in ranged windows,
//! checked against the digest the signed manifest names, decrypted and written, and an
//! interrupted run resumes from what it left in the output directory.

use std::path::PathBuf;

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use uuid::Uuid;

use super::form::{Field, Form, FormKey, FormOutcome, Secret};
use super::state::{Effect, Loadable};
use crate::restore::{RestoreScope, RestoreSummary, failure_line, summary_line};

/// The most failed assets the screen lists; the rest are counted. `capsule restore --json`
/// lists every one.
pub const FAILURES_SHOWN: usize = 5;

/// One restore, as the form collected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreRequest {
    /// The library whose album keys open the originals.
    pub library: PathBuf,
    /// Its passphrase.
    pub passphrase: Secret,
    /// The directory the originals are written to.
    pub out: PathBuf,
    /// What to restore.
    pub scope: RestoreScope,
}

/// What the restore screen does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreAction {
    /// Start typing into the form.
    Edit,
    /// [`RestoreEffect::Run`] finished.
    Finished(Result<RestoreSummary, String>),
}

/// The work the restore screen asks the ports for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestoreEffect {
    /// Run the restore; answered by [`RestoreAction::Finished`].
    Run(RestoreRequest),
}

impl RestoreEffect {
    /// The action reporting this effect failing with `reason`.
    #[must_use]
    pub fn failed(self, reason: String) -> RestoreAction {
        match self {
            Self::Run(_) => RestoreAction::Finished(Err(reason)),
        }
    }
}

const LIBRARY: usize = 0;
const OUT: usize = 1;
const PASSPHRASE: usize = 2;
const ALBUM: usize = 3;
const ASSET: usize = 4;

/// The restore screen's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreScreen {
    form: Form,
    editing: bool,
    /// Why the form was refused before anything ran.
    invalid: Option<String>,
    run: Option<Loadable<RestoreSummary>>,
}

impl Default for RestoreScreen {
    fn default() -> Self {
        Self {
            form: Form::new(vec![
                Field::text("Library"),
                Field::text("Output directory"),
                Field::secret("Library passphrase"),
                Field::text("Album id (optional)"),
                Field::text("Asset id (optional)"),
            ]),
            editing: false,
            invalid: None,
            run: None,
        }
    }
}

impl RestoreScreen {
    /// Whether keys go into the form.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.editing
    }

    /// The form.
    #[must_use]
    pub fn form(&self) -> &Form {
        &self.form
    }

    /// The last run: `None` before the first, `Loading` while one is in flight.
    #[must_use]
    pub fn run(&self) -> Option<&Loadable<RestoreSummary>> {
        self.run.as_ref()
    }

    /// Why the form was last refused, if it was.
    #[must_use]
    pub fn invalid(&self) -> Option<&str> {
        self.invalid.as_deref()
    }

    /// Apply `action`; pure, as [`super::state::AppState::update`] is.
    pub fn update(&mut self, action: RestoreAction) -> Vec<Effect> {
        match action {
            RestoreAction::Edit => {
                if self.run != Some(Loadable::Loading) {
                    self.editing = true;
                }
                Vec::new()
            }
            RestoreAction::Finished(result) => {
                self.run = Some(match result {
                    Ok(summary) => Loadable::Ready(summary),
                    Err(reason) => Loadable::Failed(reason),
                });
                Vec::new()
            }
        }
    }

    /// Apply a key typed into the form.
    pub fn on_form_key(&mut self, key: FormKey) -> Vec<Effect> {
        if !self.editing {
            return Vec::new();
        }
        match self.form.apply(key) {
            FormOutcome::Editing => Vec::new(),
            FormOutcome::Cancelled => {
                self.editing = false;
                Vec::new()
            }
            FormOutcome::Submitted => match self.request() {
                Ok(request) => {
                    self.invalid = None;
                    self.form.clear_secrets();
                    self.editing = false;
                    self.run = Some(Loadable::Loading);
                    vec![Effect::Restore(RestoreEffect::Run(request))]
                }
                Err(reason) => {
                    self.invalid = Some(reason);
                    Vec::new()
                }
            },
        }
    }

    /// The form as a request, or why it is not one.
    fn request(&self) -> Result<RestoreRequest, String> {
        let library = self.form.value(LIBRARY);
        let out = self.form.value(OUT);
        let passphrase = self.form.secret(PASSPHRASE);
        if library.is_empty() || out.is_empty() {
            return Err("Enter the library and the output directory.".to_owned());
        }
        if passphrase.is_empty() {
            return Err("Enter the library passphrase.".to_owned());
        }
        let album = parse_id(self.form.value(ALBUM), "album")?;
        let asset = parse_id(self.form.value(ASSET), "asset")?;
        let scope = match (album, asset) {
            (Some(_), Some(_)) => {
                return Err("Give an album id or an asset id, not both.".to_owned());
            }
            (Some(album), None) => RestoreScope::Album(album),
            (None, Some(asset)) => RestoreScope::Asset(asset),
            (None, None) => RestoreScope::Library,
        };
        Ok(RestoreRequest {
            library: PathBuf::from(library),
            passphrase,
            out: PathBuf::from(out),
            scope,
        })
    }
}

/// An optional id field: empty is `None`, anything else must be a UUID.
fn parse_id(value: &str, what: &str) -> Result<Option<Uuid>, String> {
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse()
        .map(Some)
        .map_err(|_| format!("The {what} id {value:?} is not a UUID."))
}

/// What a finished restore did, as lines: the summary `capsule restore` prints, and the first
/// [`FAILURES_SHOWN`] failures.
#[must_use]
pub fn summary_lines(summary: &RestoreSummary) -> Vec<Line<'static>> {
    let colour = if summary.failed == 0 {
        Color::Green
    } else {
        Color::Yellow
    };
    let mut lines = vec![Line::from(summary_line(summary).fg(colour))];
    for failed in summary.failures().take(FAILURES_SHOWN) {
        lines.push(Line::from(failure_line(failed).fg(Color::Red)));
    }
    let more = summary.failed.saturating_sub(FAILURES_SHOWN);
    if more > 0 {
        lines.push(Line::from(format!(
            "…and {more} more; `capsule restore --json` lists every one."
        )));
    }
    lines
}

/// Draw the restore screen into `area`.
pub fn render(frame: &mut Frame, area: Rect, screen: &RestoreScreen, title: &'static str) {
    let mut lines = vec![
        Line::from(
            "Download originals from the server, verified byte for byte, into a directory. An \
             interrupted restore resumes where it stopped.",
        ),
        Line::default(),
    ];
    lines.extend(screen.form.lines(screen.editing));
    if let Some(reason) = &screen.invalid {
        lines.push(Line::from(reason.clone().fg(Color::Red)));
    }
    lines.push(Line::default());
    match &screen.run {
        None => {}
        Some(Loadable::Loading) => lines.push(Line::from(
            "Restoring… the originals are fetched, verified and written.".fg(Color::DarkGray),
        )),
        Some(Loadable::Ready(summary)) => lines.extend(summary_lines(summary)),
        Some(Loadable::Failed(reason)) => {
            lines.push(Line::from("The restore did not run.".fg(Color::Red)));
            lines.push(Line::from(reason.clone()));
        }
    }
    if screen.run.is_some() {
        lines.push(Line::default());
    }
    lines.push(Line::from(
        "Scriptable: `capsule restore --library PATH --out DIR --json`.".fg(Color::DarkGray),
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
