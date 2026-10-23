//! The import screen: pick the sources, then import them into a library, as `capsule import`
//! does.
//!
//! The picker holds a list of source paths and what kind of source they are — plain files and
//! folders, or the parts of one Google Takeout export — and the form asks for the library and
//! its passphrase. The scriptable twin is `capsule import PATH… --library PATH --json`; both run
//! the same scan or source adapter, planner and executor ([`crate::import_run`]), and end in the
//! same report. The import itself never touches the network: pushing is the next screen.

use std::path::{Path, PathBuf};

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

use super::form::{Field, Form, FormKey, FormOutcome, Secret};
use super::state::{Effect, Loadable};
use crate::cli::ImportProviderArg;
use crate::import_report::{FileImportStatus, ImportReport, summary_line};

/// The most failed files the screen lists; the rest are counted. `capsule import --json` lists
/// every one.
pub const FAILURES_SHOWN: usize = 5;

/// What the picked sources are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceKind {
    /// Files and folders, read as they are.
    #[default]
    Files,
    /// The extracted parts of one Google Takeout export, whose sidecars carry capture times,
    /// places, captions and albums (`capsule import --provider takeout`).
    Takeout,
}

impl SourceKind {
    /// The kind as the screen names it.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Files => "Files and folders",
            Self::Takeout => "Google Takeout export",
        }
    }

    /// The other kind.
    #[must_use]
    pub fn switched(self) -> Self {
        match self {
            Self::Files => Self::Takeout,
            Self::Takeout => Self::Files,
        }
    }

    /// The `--provider` the kind is, if any.
    #[must_use]
    pub(crate) fn provider(self) -> Option<ImportProviderArg> {
        match self {
            Self::Files => None,
            Self::Takeout => Some(ImportProviderArg::Takeout),
        }
    }
}

/// One import, as the picker and the form collected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRequest {
    /// The library to import into.
    pub library: PathBuf,
    /// Its passphrase.
    pub passphrase: Secret,
    /// What the sources are.
    pub kind: SourceKind,
    /// The sources, in the order they were picked.
    pub sources: Vec<PathBuf>,
}

/// What the import screen does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportAction {
    /// Start typing a source path to add to the picker.
    AddSource,
    /// Drop the selected source from the picker.
    RemoveSource,
    /// Select the next source.
    SelectNext,
    /// Select the previous source.
    SelectPrevious,
    /// Switch what the sources are read as.
    SwitchKind,
    /// Start typing the library and its passphrase; submitting runs the import.
    Edit,
    /// [`ImportEffect::Run`] finished.
    Finished(Result<ImportReport, String>),
}

/// The work the import screen asks the ports for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportEffect {
    /// Run the import; answered by [`ImportAction::Finished`].
    Run(ImportRequest),
}

impl ImportEffect {
    /// The action reporting this effect failing with `reason`.
    #[must_use]
    pub fn failed(self, reason: String) -> ImportAction {
        match self {
            Self::Run(_) => ImportAction::Finished(Err(reason)),
        }
    }
}

/// Which form keys go into, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Editing {
    /// The one-field form adding a source.
    Source,
    /// The library form.
    Library,
}

const LIBRARY: usize = 0;
const PASSPHRASE: usize = 1;
const SOURCE: usize = 0;

/// The import screen's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportScreen {
    kind: SourceKind,
    sources: Vec<PathBuf>,
    selected: usize,
    source: Form,
    form: Form,
    editing: Option<Editing>,
    /// Why the last submission was refused before anything ran.
    invalid: Option<String>,
    run: Option<Loadable<ImportReport>>,
    /// The library the latest run imports into.
    run_library: Option<PathBuf>,
}

impl Default for ImportScreen {
    fn default() -> Self {
        Self {
            kind: SourceKind::default(),
            sources: Vec::new(),
            selected: 0,
            source: Form::new(vec![Field::text("Source path")]),
            form: Form::new(vec![
                Field::text("Library"),
                Field::secret("Library passphrase"),
            ]),
            editing: None,
            invalid: None,
            run: None,
            run_library: None,
        }
    }
}

impl ImportScreen {
    /// Whether keys go into a form.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.editing.is_some()
    }

    /// What the sources are read as.
    #[must_use]
    pub fn kind(&self) -> SourceKind {
        self.kind
    }

    /// The picked sources, in order.
    #[must_use]
    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }

    /// The index of the selected source; meaningless while there is none.
    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The library form.
    #[must_use]
    pub fn form(&self) -> &Form {
        &self.form
    }

    /// The last run: `None` before the first, `Loading` while one is in flight.
    #[must_use]
    pub fn run(&self) -> Option<&Loadable<ImportReport>> {
        self.run.as_ref()
    }

    /// Why the last submission was refused, if it was.
    #[must_use]
    pub fn invalid(&self) -> Option<&str> {
        self.invalid.as_deref()
    }

    /// The library the form names, as typed: what the push screen is offered once an import
    /// into it finished.
    #[must_use]
    pub fn library(&self) -> &str {
        self.form.value(LIBRARY)
    }

    fn running(&self) -> bool {
        self.run == Some(Loadable::Loading)
    }

    /// The library an import in flight is writing, or `None` when none runs.
    #[must_use]
    pub fn running_library(&self) -> Option<&Path> {
        self.running()
            .then_some(self.run_library.as_deref())
            .flatten()
    }

    /// Apply `action`; pure, as [`super::state::AppState::update`] is.
    pub fn update(&mut self, action: ImportAction) -> Vec<Effect> {
        match action {
            ImportAction::AddSource => {
                self.source.set(SOURCE, "");
                self.invalid = None;
                self.editing = Some(Editing::Source);
            }
            ImportAction::RemoveSource => {
                if self.selected < self.sources.len() {
                    let removed = self.sources.remove(self.selected);
                    tracing::debug!(target: "capsule_cli::tui", source = %removed.display(), "import source removed");
                    self.selected = self.selected.min(self.sources.len().saturating_sub(1));
                }
            }
            ImportAction::SelectNext => {
                if !self.sources.is_empty() {
                    self.selected = (self.selected + 1) % self.sources.len();
                }
            }
            ImportAction::SelectPrevious => {
                if !self.sources.is_empty() {
                    let count = self.sources.len();
                    self.selected = (self.selected + count - 1) % count;
                }
            }
            ImportAction::SwitchKind => self.kind = self.kind.switched(),
            ImportAction::Edit => {
                // One import at a time: the library is open for writing while one runs.
                if !self.running() {
                    self.invalid = None;
                    self.editing = Some(Editing::Library);
                }
            }
            ImportAction::Finished(result) => {
                tracing::debug!(target: "capsule_cli::tui", ok = result.is_ok(), "import finished");
                self.run = Some(match result {
                    Ok(report) => Loadable::Ready(report),
                    Err(reason) => Loadable::Failed(reason),
                });
            }
        }
        Vec::new()
    }

    /// Apply a key typed into the form being edited.
    pub fn on_form_key(&mut self, key: FormKey) -> Vec<Effect> {
        self.on_form_key_guarded(key, None)
    }

    /// Apply a key typed into the form while a push to `pushing` is in flight: an import into
    /// that same library is refused with a reason, because the workspace has no lock.
    pub fn on_form_key_guarded(&mut self, key: FormKey, pushing: Option<&Path>) -> Vec<Effect> {
        match self.editing {
            None => Vec::new(),
            Some(Editing::Source) => {
                match self.source.apply(key) {
                    FormOutcome::Editing => {}
                    FormOutcome::Cancelled => self.editing = None,
                    FormOutcome::Submitted => self.add_source(),
                }
                Vec::new()
            }
            Some(Editing::Library) => match self.form.apply(key) {
                FormOutcome::Editing => Vec::new(),
                FormOutcome::Cancelled => {
                    self.editing = None;
                    Vec::new()
                }
                FormOutcome::Submitted => match self.request() {
                    Ok(request) if pushing.is_some_and(|pushing| pushing == request.library) => {
                        tracing::warn!(
                            target: "capsule_cli::tui",
                            library = %request.library.display(),
                            "import refused: a push to the same library is in flight"
                        );
                        self.invalid = Some(
                            "A push to this library is still running: wait for it to finish, then import."
                                .to_owned(),
                        );
                        Vec::new()
                    }
                    Ok(request) => {
                        tracing::info!(
                            target: "capsule_cli::tui",
                            sources = request.sources.len(),
                            kind = ?request.kind,
                            "import requested"
                        );
                        self.invalid = None;
                        self.form.clear_secrets();
                        self.editing = None;
                        self.run = Some(Loadable::Loading);
                        self.run_library = Some(request.library.clone());
                        vec![Effect::Import(ImportEffect::Run(request))]
                    }
                    Err(reason) => {
                        self.invalid = Some(reason);
                        Vec::new()
                    }
                },
            },
        }
    }

    /// Add the typed source path to the picker and select it. A blank path or one already
    /// picked is refused with the reason; editing stops either way.
    fn add_source(&mut self) {
        self.editing = None;
        let path = self.source.value(SOURCE);
        if path.is_empty() {
            self.invalid = Some("Type a file or folder path to add.".to_owned());
            return;
        }
        let path = PathBuf::from(path);
        if let Some(index) = self.sources.iter().position(|p| *p == path) {
            self.selected = index;
            self.invalid = Some(format!("{} is already picked.", path.display()));
            return;
        }
        tracing::debug!(target: "capsule_cli::tui", source = %path.display(), "import source added");
        self.sources.push(path);
        self.selected = self.sources.len() - 1;
        self.invalid = None;
    }

    /// The picker and the form as a request, or why they are not one.
    fn request(&self) -> Result<ImportRequest, String> {
        if self.sources.is_empty() {
            return Err("Pick at least one source first: press a to add one.".to_owned());
        }
        let library = self.form.value(LIBRARY);
        if library.is_empty() {
            return Err("Enter the library.".to_owned());
        }
        let passphrase = self.form.secret(PASSPHRASE);
        if passphrase.is_empty() {
            return Err("Enter the library passphrase.".to_owned());
        }
        Ok(ImportRequest {
            library: PathBuf::from(library),
            passphrase,
            kind: self.kind,
            sources: self.sources.clone(),
        })
    }
}

/// What a finished import did, as lines: the summary `capsule import` prints, the first
/// [`FAILURES_SHOWN`] files that failed, and the way on to pushing.
#[must_use]
pub fn report_lines(report: &ImportReport) -> Vec<Line<'static>> {
    let colour = if report.errors == 0 {
        Color::Green
    } else {
        Color::Yellow
    };
    let mut lines = vec![Line::from(summary_line(report).fg(colour))];
    let failed = report
        .outcomes
        .iter()
        .filter(|o| o.status == FileImportStatus::Failed);
    for outcome in failed.clone().take(FAILURES_SHOWN) {
        let reason = outcome.error.as_deref().unwrap_or("no detail");
        lines.push(Line::from(
            format!("✗ {}: {reason}", outcome.path).fg(Color::Red),
        ));
    }
    let more = failed.count().saturating_sub(FAILURES_SHOWN);
    if more > 0 {
        lines.push(Line::from(format!(
            "…and {more} more; `capsule import --json` lists every one."
        )));
    }
    lines.push(Line::from(
        "Imported assets stay on this device until they are pushed: press p.",
    ));
    lines
}

/// Draw the import screen into `area`.
pub fn render(frame: &mut Frame, area: Rect, screen: &ImportScreen, title: &'static str) {
    let mut lines = vec![
        Line::from(
            "Pick what to import, then import it into a library. Nothing leaves this device \
             until you push.",
        ),
        Line::default(),
        Line::from(vec![
            "Sources are: ".into(),
            screen.kind.label().bold(),
            format!("  (k: read as {})", screen.kind.switched().label()).fg(Color::DarkGray),
        ]),
    ];
    if screen.sources.is_empty() {
        lines.push(Line::from(
            "  No sources yet: press a to add a file or folder.".fg(Color::DarkGray),
        ));
    }
    for (index, source) in screen.sources.iter().enumerate() {
        let marker = if index == screen.selected {
            "› "
        } else {
            "  "
        };
        lines.push(Line::from(format!("{marker}{}", source.display())));
    }
    if screen.editing == Some(Editing::Source) {
        lines.extend(screen.source.lines(true));
    }
    lines.push(Line::default());
    lines.extend(screen.form.lines(screen.editing == Some(Editing::Library)));
    if let Some(reason) = &screen.invalid {
        lines.push(Line::from(reason.clone().fg(Color::Red)));
    }
    lines.push(Line::default());
    match &screen.run {
        None => {}
        Some(Loadable::Loading) => lines.push(Line::from(
            "Importing… each file is hashed, sealed and signed into the library."
                .fg(Color::DarkGray),
        )),
        Some(Loadable::Ready(report)) => lines.extend(report_lines(report)),
        Some(Loadable::Failed(reason)) => {
            lines.push(Line::from("The import did not run.".fg(Color::Red)));
            lines.push(Line::from(reason.clone()));
        }
    }
    if screen.run.is_some() {
        lines.push(Line::default());
    }
    lines.push(Line::from(
        "Scriptable: `capsule import PATH… --library PATH [--provider takeout] --json`."
            .fg(Color::DarkGray),
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
