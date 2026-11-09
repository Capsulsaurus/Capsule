//! The push screen: push a library to the server and watch each asset reach custody — its
//! blobs uploaded, the server's durable verdict, a signed receipt per blob — as `capsule push`
//! does, with the assets that did not make it listed and retryable.
//!
//! The scriptable twin is `capsule push --library PATH --json`, and a retry is
//! `capsule push --asset ID…`; both run [`crate::remote::push_run`]. A retry pushes only the
//! assets that did not reach custody: nothing is imported again, and the assets that did are not
//! asked about again.
//!
//! # Where the rows come from
//!
//! Nothing here polls. While a run is in flight the screen keeps one [`PushEffect::Watch`] out,
//! which the ports answer with the next batch of [`PushProgress`] as it happens: the run's own
//! framing (an asset started, with its blobs; an asset finished, with its outcome) and the
//! custody steps of the SDK's transfer event stream (a blob finalized or merged, the durable
//! verdict, each verified receipt). The reducer folds each batch into the rows and asks for the
//! next, until the ports say the run's stream is closed. The run's summary, when it lands, is
//! authoritative: every asset's row is set from its outcome.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use capsule_sdk::events::TransferEvent;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use uuid::Uuid;

use super::form::{Field, Form, FormKey, FormOutcome, Secret};
use super::state::{Effect, Loadable, ResolvedLibrary};
use crate::remote::{AssetProgress, AssetPushOutcome, AssetPushStatus, PushSummary, custody_line};

/// The most asset rows the screen draws; the rest are counted.
pub const ROWS_SHOWN: usize = 8;

/// The most failed assets the screen lists with their reason; the rest are counted.
/// `capsule push --json` lists every one.
pub const FAILURES_SHOWN: usize = 5;

/// One push, as the form collected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushRequest {
    /// Which run of this screen this is, so the ports can say when its progress has all been
    /// told ([`PushProgress::Closed`]).
    pub run: u64,
    /// The library to push.
    pub library: PathBuf,
    /// Its passphrase.
    pub passphrase: Secret,
    /// The assets to push, for a retry; `None` pushes the whole library.
    pub assets: Option<Vec<Uuid>>,
}

/// One thing that happened in a push, as the ports tell it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushProgress {
    /// The run is on this asset; `blobs` are its bundle's content addresses.
    Started {
        /// The asset id.
        asset_id: String,
        /// Its blobs' content addresses.
        blobs: Vec<String>,
    },
    /// A custody step from the transfer event stream ([`is_custody_step`]).
    Transfer(TransferEvent),
    /// The run is done with this asset.
    Finished(AssetPushOutcome),
    /// The stream's subscriber fell behind and this many transfer events never arrived; the
    /// affected rows catch up when their asset finishes.
    Lagged(u64),
    /// Nothing more will be told for run `run`; `None` when the ports tell no progress at all.
    Closed {
        /// The run whose stream closed.
        run: Option<u64>,
    },
}

impl From<AssetProgress> for PushProgress {
    fn from(progress: AssetProgress) -> Self {
        match progress {
            AssetProgress::Started { asset_id, blobs } => Self::Started {
                asset_id: asset_id.to_string(),
                blobs,
            },
            AssetProgress::Finished(outcome) => Self::Finished(outcome),
        }
    }
}

/// Whether `event` moves an asset's custody: a blob reached the server (finalized, or merged
/// onto a copy it already held), the durable verdict, or a verified receipt. Chunk-level steps
/// are the transfer view's business; leaving them out keeps a push to a few actions per blob.
#[must_use]
pub fn is_custody_step(event: &TransferEvent) -> bool {
    matches!(
        event,
        TransferEvent::Finalize { .. }
            | TransferEvent::Merged { .. }
            | TransferEvent::Verify { .. }
            | TransferEvent::Receipt { .. }
    )
}

/// What the push screen does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushAction {
    /// Start typing the library and its passphrase; submitting pushes the whole library.
    Edit,
    /// Start typing the passphrase to push again just the assets that did not reach custody.
    Retry,
    /// [`PushEffect::Watch`] answered with this batch.
    Progress(Vec<PushProgress>),
    /// [`PushEffect::Run`] finished: the run's summary, or why it could not run.
    Finished(Result<PushSummary, String>),
}

/// The work the push screen asks the ports for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushEffect {
    /// Run the push; answered by [`PushAction::Finished`].
    Run(PushRequest),
    /// Wait for the next batch of progress; answered by [`PushAction::Progress`].
    Watch,
}

impl PushEffect {
    /// The action reporting this effect failing with `reason`. A watch that failed is a stream
    /// that will tell nothing more: the rows are set from the summary when the run lands.
    #[must_use]
    pub fn failed(self, reason: String) -> PushAction {
        match self {
            Self::Run(_) => PushAction::Finished(Err(reason)),
            Self::Watch => {
                tracing::warn!(target: "capsule_cli::tui", %reason, "push progress watch failed");
                PushAction::Progress(vec![PushProgress::Closed { run: None }])
            }
        }
    }
}

/// Where one asset stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Custody {
    /// Not reached yet: queued in a run in flight, or left behind by a run that stopped.
    Waiting,
    /// The run is on it: uploading its blobs or confirming custody.
    Sending,
    /// The server holds every blob durably and a verified receipt for each is in the library.
    Verified,
    /// It did not reach custody; why, as `capsule push` words it.
    Failed(String),
    /// Skipped: another device of the account holds its open hand-off and uploads it.
    Delegated,
    /// Skipped: the server already holds its create, which an edit here has superseded, so
    /// only the records after it are sent.
    OnServer,
}

/// One asset's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetRow {
    /// The asset id.
    pub asset_id: String,
    /// Blobs in its bundle; zero until the run tells its start.
    pub blobs: usize,
    /// Blobs on the server: finalized or merged this run, or already held.
    pub uploaded: usize,
    /// The server's durable verdict, once it gave one.
    pub durable: Option<bool>,
    /// Verified receipts.
    pub receipts: usize,
    /// Where it stands.
    pub custody: Custody,
}

impl AssetRow {
    fn new(asset_id: String) -> Self {
        Self {
            asset_id,
            blobs: 0,
            uploaded: 0,
            durable: None,
            receipts: 0,
            custody: Custody::Waiting,
        }
    }

    /// Reset for another attempt.
    fn restart(&mut self) {
        *self = Self::new(std::mem::take(&mut self.asset_id));
    }
}

const LIBRARY: usize = 0;
const PASSPHRASE: usize = 1;

/// The push screen's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushScreen {
    form: Form,
    editing: bool,
    /// The assets the form being typed will retry; `None` for a whole-library push.
    retrying: Option<Vec<Uuid>>,
    /// Why the last request was refused before anything ran, or why there was nothing to do.
    notice: Option<String>,
    run: Option<Loadable<PushSummary>>,
    /// The library the latest run pushes.
    run_library: Option<PathBuf>,
    resolved: Option<ResolvedLibrary>,
    /// The id of the latest run started.
    run_id: u64,
    /// Whether a [`PushEffect::Watch`] is out.
    watching: bool,
    /// Transfer events the stream dropped while the screen fell behind.
    lagged: u64,
    rows: Vec<AssetRow>,
    by_asset: HashMap<String, usize>,
    by_blob: HashMap<String, usize>,
}

impl Default for PushScreen {
    fn default() -> Self {
        Self {
            form: Form::new(vec![
                Field::text("Library"),
                Field::secret("Library passphrase"),
            ]),
            editing: false,
            retrying: None,
            notice: None,
            run: None,
            run_library: None,
            resolved: None,
            run_id: 0,
            watching: false,
            lagged: 0,
            rows: Vec::new(),
            by_asset: HashMap::new(),
            by_blob: HashMap::new(),
        }
    }
}

impl PushScreen {
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
    pub fn run(&self) -> Option<&Loadable<PushSummary>> {
        self.run.as_ref()
    }

    /// Every asset a run this session reached, in the order the runs reached them.
    #[must_use]
    pub fn rows(&self) -> &[AssetRow] {
        &self.rows
    }

    /// The row of `asset_id`, if a run reached it.
    #[must_use]
    pub fn row(&self, asset_id: &str) -> Option<&AssetRow> {
        self.by_asset.get(asset_id).map(|&index| &self.rows[index])
    }

    /// Whether a [`PushEffect::Watch`] is out.
    #[must_use]
    pub fn is_watching(&self) -> bool {
        self.watching
    }

    /// Why the last request was refused, or why there was nothing to do.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Transfer events the stream dropped.
    #[must_use]
    pub fn lagged(&self) -> u64 {
        self.lagged
    }

    /// The assets a retry would push: every row that did not reach custody and is not handed
    /// to another device.
    #[must_use]
    pub fn retryable(&self) -> Vec<&AssetRow> {
        self.rows
            .iter()
            .filter(|row| {
                !matches!(
                    row.custody,
                    Custody::Verified | Custody::Delegated | Custody::OnServer
                )
            })
            .collect()
    }

    /// Fill the library field with `library` when it is empty and not being typed into: an
    /// import into a library offers that library here.
    pub fn offer_library(&mut self, library: &str) {
        if !self.editing && self.form.value(LIBRARY).is_empty() {
            self.form.set(LIBRARY, library);
        }
    }

    /// Record the canonical form of the library typed now, made by the caller outside the
    /// reducer; the next submit compares it, not the typed text.
    pub(super) fn resolve_library(&mut self, canonicalize: impl Fn(&Path) -> PathBuf) {
        let typed = self.form.value(LIBRARY);
        if !typed.is_empty() {
            self.resolved = Some(ResolvedLibrary::new(typed, canonicalize(Path::new(typed))));
        }
    }

    fn running(&self) -> bool {
        self.run == Some(Loadable::Loading)
    }

    /// The library a push in flight is writing, or `None` when none runs.
    #[must_use]
    pub fn running_library(&self) -> Option<&Path> {
        self.running()
            .then_some(self.run_library.as_deref())
            .flatten()
    }

    /// Apply `action`; pure, as [`super::state::AppState::update`] is.
    pub fn update(&mut self, action: PushAction) -> Vec<Effect> {
        match action {
            PushAction::Edit => {
                if !self.running() {
                    self.notice = None;
                    self.retrying = None;
                    self.form.focus_on(LIBRARY);
                    self.editing = true;
                }
                Vec::new()
            }
            PushAction::Retry => {
                if self.running() {
                    return Vec::new();
                }
                let assets: Vec<Uuid> = self
                    .retryable()
                    .iter()
                    .filter_map(|row| row.asset_id.parse().ok())
                    .collect();
                if assets.is_empty() {
                    self.notice = Some(if self.rows.is_empty() {
                        "Nothing to retry yet: press e to push a library.".to_owned()
                    } else {
                        "Nothing to retry: every asset reached custody.".to_owned()
                    });
                    return Vec::new();
                }
                self.notice = None;
                self.retrying = Some(assets);
                self.form.focus_on(PASSPHRASE);
                self.editing = true;
                Vec::new()
            }
            PushAction::Progress(batch) => self.on_progress(batch),
            PushAction::Finished(result) => {
                match &result {
                    Ok(summary) => {
                        tracing::info!(
                            target: "capsule_cli::tui",
                            assets = summary.assets,
                            verified = summary.verified_assets,
                            failed = summary.failed_assets,
                            "push finished"
                        );
                        for outcome in &summary.outcomes {
                            self.apply_outcome(outcome);
                        }
                    }
                    Err(reason) => {
                        tracing::warn!(target: "capsule_cli::tui", %reason, "push did not run");
                    }
                }
                // An asset the run never finished was not reached; a retry pushes it.
                for row in &mut self.rows {
                    if row.custody == Custody::Sending {
                        row.custody = Custody::Waiting;
                    }
                }
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
        self.on_form_key_guarded(key, None)
    }

    /// Apply a key typed into the form while an import into `importing` is in flight: a push
    /// to that same library is refused with a notice, because the workspace has no lock.
    pub fn on_form_key_guarded(&mut self, key: FormKey, importing: Option<&Path>) -> Vec<Effect> {
        if !self.editing {
            return Vec::new();
        }
        match self.form.apply(key) {
            FormOutcome::Editing => Vec::new(),
            FormOutcome::Cancelled => {
                self.editing = false;
                self.retrying = None;
                Vec::new()
            }
            FormOutcome::Submitted => self.submit(importing),
        }
    }

    /// Start the run the form describes, or say why not.
    fn submit(&mut self, importing: Option<&Path>) -> Vec<Effect> {
        let library = self.form.value(LIBRARY);
        let passphrase = self.form.secret(PASSPHRASE);
        if library.is_empty() {
            self.notice = Some("Enter the library.".to_owned());
            return Vec::new();
        }
        if passphrase.is_empty() {
            self.notice = Some("Enter the library passphrase.".to_owned());
            return Vec::new();
        }
        let identity = ResolvedLibrary::identity(self.resolved.as_ref(), library);
        if importing.is_some_and(|importing| importing == identity) {
            tracing::warn!(
                target: "capsule_cli::tui",
                library = %library,
                "push refused: an import into the same library is in flight"
            );
            self.notice = Some(
                "An import into this library is still running: wait for it to finish, then push."
                    .to_owned(),
            );
            return Vec::new();
        }
        self.run_library = Some(identity);
        self.run_id += 1;
        let assets = self.retrying.take();
        let request = PushRequest {
            run: self.run_id,
            library: PathBuf::from(library),
            passphrase,
            assets,
        };
        match &request.assets {
            // A whole-library run starts the table again; a retry keeps the assets that
            // already reached custody and resets the ones it pushes again.
            None => {
                self.rows.clear();
                self.by_asset.clear();
                self.by_blob.clear();
            }
            Some(assets) => {
                for asset in assets {
                    self.row_mut(&asset.to_string()).restart();
                }
            }
        }
        tracing::info!(
            target: "capsule_cli::tui",
            run = request.run,
            retry = request.assets.as_ref().map(Vec::len),
            "push requested"
        );
        self.notice = None;
        self.lagged = 0;
        self.form.clear_secrets();
        self.editing = false;
        self.run = Some(Loadable::Loading);
        // The watch goes out first: it waits on the run, never the other way round.
        let mut effects = Vec::with_capacity(2);
        if !self.watching {
            self.watching = true;
            effects.push(Effect::Push(PushEffect::Watch));
        }
        effects.push(Effect::Push(PushEffect::Run(request)));
        effects
    }

    /// Fold a batch of progress, and ask for the next unless the run's stream closed.
    fn on_progress(&mut self, batch: Vec<PushProgress>) -> Vec<Effect> {
        let mut closed = false;
        for progress in batch {
            match progress {
                PushProgress::Closed { run } => {
                    // A close left over from an earlier run says nothing about this one.
                    if run.is_none_or(|run| run == self.run_id) {
                        closed = true;
                    }
                }
                PushProgress::Lagged(missed) => self.lagged += missed,
                // Once the summary landed it is the truth; a step it overtook changes nothing.
                _ if !self.running() => {}
                PushProgress::Started { asset_id, blobs } => {
                    let index = self.row_index(&asset_id);
                    let row = &mut self.rows[index];
                    row.restart();
                    row.blobs = blobs.len();
                    row.custody = Custody::Sending;
                    for blob in blobs {
                        self.by_blob.insert(blob, index);
                    }
                }
                PushProgress::Transfer(event) => self.on_transfer(&event),
                PushProgress::Finished(outcome) => self.apply_outcome(&outcome),
            }
        }
        if closed {
            self.watching = false;
            Vec::new()
        } else {
            vec![Effect::Push(PushEffect::Watch)]
        }
    }

    /// Fold one custody step into the row it belongs to. A step for an asset the run already
    /// finished arrives late and is dropped: the outcome holds it.
    fn on_transfer(&mut self, event: &TransferEvent) {
        let index = match event {
            TransferEvent::Finalize { blob, .. } | TransferEvent::Merged { blob, .. } => {
                self.by_blob.get(blob).copied()
            }
            TransferEvent::Verify { asset_id, .. } | TransferEvent::Receipt { asset_id, .. } => {
                self.by_asset.get(asset_id).copied()
            }
            _ => None,
        };
        let Some(row) = index.map(|index| &mut self.rows[index]) else {
            return;
        };
        if row.custody != Custody::Sending {
            return;
        }
        match event {
            TransferEvent::Finalize { .. } | TransferEvent::Merged { .. } => {
                row.uploaded = (row.uploaded + 1).min(row.blobs);
            }
            // The SDK refuses a verdict that counts a blob not durable whatever its flag says.
            TransferEvent::Verify {
                durable,
                not_durable,
                ..
            } => row.durable = Some(*durable && *not_durable == 0),
            TransferEvent::Receipt { .. } => row.receipts += 1,
            _ => {}
        }
    }

    /// Set a row from the run's outcome for its asset.
    fn apply_outcome(&mut self, outcome: &AssetPushOutcome) {
        let row = self.row_mut(&outcome.asset_id);
        row.uploaded = outcome.uploaded + outcome.merged + outcome.already_held;
        // A row whose start never arrived does not know its blob count; it shows the count
        // uploaded alone rather than a guess.
        if row.blobs > 0 {
            row.blobs = row.blobs.max(row.uploaded);
        }
        row.receipts = outcome.receipts;
        match outcome.status {
            AssetPushStatus::Verified => {
                row.durable = Some(true);
                row.custody = Custody::Verified;
            }
            AssetPushStatus::Failed => {
                let error = outcome.error.as_ref();
                if error.is_some_and(|e| e.kind == "not_durable") {
                    row.durable = Some(false);
                }
                row.custody = Custody::Failed(
                    error.map_or_else(|| "no detail".to_owned(), |e| e.message.clone()),
                );
            }
            // The screen does not offer a dry run; a planned outcome moves nothing.
            AssetPushStatus::Planned => row.custody = Custody::Waiting,
            AssetPushStatus::Delegated => row.custody = Custody::Delegated,
            AssetPushStatus::Indexed => row.custody = Custody::OnServer,
        }
    }

    fn row_index(&mut self, asset_id: &str) -> usize {
        if let Some(&index) = self.by_asset.get(asset_id) {
            return index;
        }
        self.rows.push(AssetRow::new(asset_id.to_owned()));
        let index = self.rows.len() - 1;
        self.by_asset.insert(asset_id.to_owned(), index);
        index
    }

    fn row_mut(&mut self, asset_id: &str) -> &mut AssetRow {
        let index = self.row_index(asset_id);
        &mut self.rows[index]
    }
}

/// The rows worth drawing first: the asset the run is on, then the failures, then the ones
/// waiting, then the verified ones; library order within each.
fn shown_rows(screen: &PushScreen) -> Vec<&AssetRow> {
    let rank = |row: &AssetRow| match row.custody {
        Custody::Sending => 0,
        Custody::Failed(_) => 1,
        Custody::Waiting => 2,
        Custody::Delegated => 3,
        Custody::OnServer | Custody::Verified => 4,
    };
    let mut rows: Vec<&AssetRow> = screen.rows.iter().collect();
    rows.sort_by_key(|row| rank(row));
    rows
}

fn row_line(row: &AssetRow, running: bool) -> Line<'static> {
    let uploaded = if row.blobs == 0 {
        row.uploaded.to_string()
    } else {
        format!("{}/{}", row.uploaded, row.blobs)
    };
    let durable = match row.durable {
        None => "–",
        Some(true) => "yes",
        Some(false) => "no",
    };
    let (state, colour) = match &row.custody {
        Custody::Waiting if running => ("waiting", Color::DarkGray),
        Custody::Waiting => ("not reached", Color::Yellow),
        Custody::Sending => ("sending", Color::Cyan),
        Custody::Verified => ("verified", Color::Green),
        Custody::Failed(_) => ("failed", Color::Red),
        Custody::Delegated => ("handed off", Color::Magenta),
        Custody::OnServer => ("on server", Color::Green),
    };
    Line::from(vec![
        Span::raw(format!(
            "{:<36}  {uploaded:>8}  {durable:>7}  {:>8}  ",
            row.asset_id, row.receipts
        )),
        state.fg(colour),
    ])
}

/// The asset table and the failure list.
fn custody_lines(screen: &PushScreen) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if screen.rows.is_empty() {
        return lines;
    }
    let verified = screen
        .rows
        .iter()
        .filter(|r| r.custody == Custody::Verified)
        .count();
    let failed: Vec<&AssetRow> = screen
        .rows
        .iter()
        .filter(|r| matches!(r.custody, Custody::Failed(_)))
        .collect();
    lines.push(Line::from(format!(
        "{verified} of {} assets in custody, {} failed.",
        screen.rows.len(),
        failed.len()
    )));
    lines.push(Line::from(Span::styled(
        format!(
            "{:<36}  {:>8}  {:>7}  {:>8}  State",
            "Asset", "Uploaded", "Durable", "Receipts"
        ),
        Style::new().add_modifier(Modifier::BOLD),
    )));
    let running = screen.running();
    let shown = shown_rows(screen);
    for row in shown.iter().take(ROWS_SHOWN) {
        lines.push(row_line(row, running));
    }
    let more = shown.len().saturating_sub(ROWS_SHOWN);
    if more > 0 {
        lines.push(Line::from(format!("…and {more} more.").fg(Color::DarkGray)));
    }
    if !failed.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(
            "Did not reach custody (r retries every asset not in custody):".fg(Color::Red),
        ));
        for row in failed.iter().take(FAILURES_SHOWN) {
            if let Custody::Failed(reason) = &row.custody {
                lines.push(Line::from(format!("{}: {reason}", row.asset_id)));
            }
        }
        let more = failed.len().saturating_sub(FAILURES_SHOWN);
        if more > 0 {
            lines.push(Line::from(format!(
                "…and {more} more; `capsule push --json` lists every one."
            )));
        }
    }
    lines
}

/// Draw the push screen into `area`.
pub fn render(frame: &mut Frame, area: Rect, screen: &PushScreen, title: &'static str) {
    let mut lines = vec![
        Line::from(
            "Push a library to the server. An asset counts once the server holds every blob \
             durably and a signed receipt for each is stored in the library.",
        ),
        Line::default(),
    ];
    if screen.editing
        && let Some(assets) = &screen.retrying
    {
        lines.push(Line::from(
            format!(
                "Retrying {} asset(s) that did not reach custody.",
                assets.len()
            )
            .fg(Color::Yellow),
        ));
    }
    lines.extend(screen.form.lines(screen.editing));
    if let Some(notice) = &screen.notice {
        lines.push(Line::from(notice.clone().fg(Color::Red)));
    }
    lines.push(Line::default());
    match &screen.run {
        None => {}
        Some(Loadable::Loading) => lines.push(Line::from(
            "Pushing… each asset is uploaded, checked durable and receipted.".fg(Color::DarkGray),
        )),
        Some(Loadable::Ready(summary)) => {
            let colour = if summary.failed_assets == 0 {
                Color::Green
            } else {
                Color::Yellow
            };
            lines.push(Line::from(custody_line(summary).fg(colour)));
        }
        Some(Loadable::Failed(reason)) => {
            lines.push(Line::from("The push did not run.".fg(Color::Red)));
            lines.push(Line::from(reason.clone()));
        }
    }
    if screen.lagged > 0 {
        lines.push(Line::from(
            format!(
                "{} transfer events were missed; each row catches up when its asset finishes.",
                screen.lagged
            )
            .fg(Color::DarkGray),
        ));
    }
    lines.extend(custody_lines(screen));
    if screen.run.is_some() || !screen.rows.is_empty() {
        lines.push(Line::default());
    }
    lines.push(Line::from(
        "Scriptable: `capsule push --library PATH --json`; `--asset ID` pushes one asset again."
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
