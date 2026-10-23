//! Metadata repair: correct the capture time and location of the asset selected in the library
//! browser, with the assets captured just before and after it shown for context.
//!
//! A wrong camera clock or a missing fix is easiest to judge against the photos taken around
//! it: the screen lists the asset's neighbours in capture order, each with how far it is from
//! the asset in time and where it was taken, and a neighbour's location can be copied onto the
//! asset in one key. Every correction goes to the user's register beside the as-imported value,
//! which is never overwritten, so the screen shows both, and a later edit can always go back.
//!
//! Edits are written exactly as the label editor writes them ([`super::label_editor`]): one
//! [`Edit`] through the port, which runs [`crate::edit::apply`] and hands back a fresh snapshot.
//! The scriptable twins are `capsule edit capture-time` and `capsule edit location`, with
//! `--json`.

use capsule_core::domain::GpsDatum;
use jiff::Timestamp;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use uuid::Uuid;

use super::form::{Field, Form, FormKey, FormOutcome, Status};
use super::label_editor::{EditRequest, Edited};
use super::library::{AssetEntry, Catalogue, LibraryScreen, Place};
use super::state::Effect;
use crate::edit::{Edit, parse_coordinate, parse_datum};
use crate::show::{gps_datum_name, gps_source_name};

/// How many assets are shown on each side of the one being repaired.
pub const SPAN: usize = 3;

/// What the metadata repair screen does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairAction {
    /// Type a corrected capture time.
    EditTime,
    /// Type a corrected location.
    EditLocation,
    /// Record that the asset has no location.
    ClearLocation,
    /// Copy the selected neighbour's location onto the asset.
    CopyLocation,
    /// Select the next row of the context.
    SelectNext,
    /// Select the previous row of the context.
    SelectPrevious,
    /// [`RepairEffect::Apply`] finished. The root hands the new snapshot to the browser before
    /// the screen sees this.
    Applied(Result<Edited, String>),
}

/// The work the metadata repair screen asks the ports for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepairEffect {
    /// Write an edit; answered by [`RepairAction::Applied`].
    Apply(EditRequest),
}

impl RepairEffect {
    /// The action reporting this effect failing with `reason`.
    #[must_use]
    pub fn failed(self, reason: String) -> RepairAction {
        match self {
            Self::Apply(_) => RepairAction::Applied(Err(reason)),
        }
    }
}

/// Which correction is being typed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The capture time.
    Time,
    /// The location.
    Location,
}

const TIME: usize = 0;
const LAT: usize = 0;
const LON: usize = 1;
const DATUM: usize = 2;

/// The status while a correction asked on this asset is being written.
const WRITING: &str = "Writing the correction…";

/// The status while a correction asked on an earlier asset is being written.
const WRITING_EARLIER: &str = "Writing a correction on the earlier asset…";

/// The metadata repair screen's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataRepair {
    asset: Option<Uuid>,
    /// The selected row of the context, the asset itself included, by id: a correction that
    /// moves the asset along the timeline reorders the context, and the selection follows
    /// the asset it was on rather than the index.
    selection: Option<Uuid>,
    time: Form,
    location: Form,
    mode: Option<Mode>,
    status: Status,
    /// The asset whose correction is being written. Kept apart from `status`, which any key
    /// may rewrite, and cleared only when the correction's answer arrives.
    in_flight: Option<Uuid>,
}

impl Default for MetadataRepair {
    fn default() -> Self {
        Self {
            asset: None,
            selection: None,
            in_flight: None,
            time: Form::new(vec![Field::text("Capture time (RFC 3339)")]),
            location: Form::new(vec![
                Field::text("Latitude"),
                Field::text("Longitude"),
                Field::text("Datum (WGS-84 or GCJ-02)"),
            ]),
            mode: None,
            status: Status::Idle,
        }
    }
}

/// The assets around `asset_id` in capture order, newest first as the timeline lists them:
/// up to `span` on each side, trashed ones left out (the asset itself is always listed). Also
/// the asset's own index in the list; `None` when the catalogue does not hold it.
#[must_use]
pub fn context(catalogue: &Catalogue, asset_id: Uuid, span: usize) -> Option<(Vec<Uuid>, usize)> {
    let listed: Vec<Uuid> = catalogue
        .assets()
        .iter()
        .filter(|a| !a.in_trash || a.asset_id == asset_id)
        .map(|a| a.asset_id)
        .collect();
    let at = listed.iter().position(|id| *id == asset_id)?;
    let start = at.saturating_sub(span);
    let end = (at + span + 1).min(listed.len());
    Some((listed[start..end].to_vec(), at - start))
}

/// How far `other` was captured from `from`, signed and in the two largest units: `+2d 3h`,
/// `-45m`, `+12s`, `same time`; `?` where either is undated.
#[must_use]
pub fn delta(from: Option<Timestamp>, other: Option<Timestamp>) -> String {
    let (Some(from), Some(other)) = (from, other) else {
        return "?".to_owned();
    };
    let seconds = other.as_second() - from.as_second();
    if seconds == 0 {
        return "same time".to_owned();
    }
    let sign = if seconds < 0 { '-' } else { '+' };
    let s = seconds.unsigned_abs();
    let (days, hours, minutes, secs) = (s / 86_400, s % 86_400 / 3_600, s % 3_600 / 60, s % 60);
    let text = if days > 0 {
        format!("{days}d {hours}h")
    } else if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m")
    } else {
        format!("{secs}s")
    };
    format!("{sign}{text}")
}

/// A location as one short phrase.
#[must_use]
pub fn place_text(place: Option<Place>) -> String {
    place.map_or_else(
        || "no location".to_owned(),
        |p| {
            format!(
                "{:.5}, {:.5} ({}, {})",
                p.lat,
                p.lon,
                gps_datum_name(p.datum),
                gps_source_name(p.source)
            )
        },
    )
}

impl MetadataRepair {
    /// Whether keys go into a correction form.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.mode.is_some()
    }

    /// Whether an edit is being written.
    #[must_use]
    pub fn is_working(&self) -> bool {
        self.in_flight.is_some()
    }

    /// The asset being repaired.
    #[must_use]
    pub fn asset(&self) -> Option<Uuid> {
        self.asset
    }

    /// Which correction is being typed.
    #[must_use]
    pub fn mode(&self) -> Option<Mode> {
        self.mode
    }

    /// Where the last edit stands.
    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// The form being typed into, if one is.
    #[must_use]
    pub fn form(&self) -> Option<&Form> {
        match self.mode? {
            Mode::Time => Some(&self.time),
            Mode::Location => Some(&self.location),
        }
    }

    /// Repair `asset_id` in the library `library` has open; the asset itself is selected.
    ///
    /// A correction still being written stays in flight, so a second one cannot start, and
    /// its answer lands here; one asked on another asset says so when it lands.
    pub fn begin(&mut self, asset_id: Uuid) {
        tracing::debug!(target: "capsule_cli::tui", %asset_id, "metadata repair opened");
        let in_flight = self.in_flight;
        let status = match in_flight {
            Some(asked) if asked == asset_id => Status::Working(WRITING),
            Some(_) => Status::Working(WRITING_EARLIER),
            None => Status::Idle,
        };
        *self = Self {
            asset: Some(asset_id),
            selection: Some(asset_id),
            status,
            in_flight,
            ..Self::default()
        };
    }

    /// The context and the selected row's index in it: the row selected by id, or the asset
    /// itself when that row has left the context.
    fn rows(&self, catalogue: &Catalogue) -> Option<(Vec<Uuid>, usize)> {
        let (rows, at) = context(catalogue, self.asset?, SPAN)?;
        let index = self
            .selection
            .and_then(|id| rows.iter().position(|row| *row == id))
            .unwrap_or(at);
        Some((rows, index))
    }

    /// The selected row of the context, kept inside it.
    #[must_use]
    pub fn selected(&self, catalogue: &Catalogue) -> Option<Uuid> {
        let (rows, index) = self.rows(catalogue)?;
        rows.get(index).copied()
    }

    /// An edit finished: what it did, or why it was not written. An answer for another asset
    /// than the one now open names it.
    pub fn finish(&mut self, result: Result<String, String>) {
        let elsewhere = self
            .in_flight
            .take()
            .filter(|asked| Some(*asked) != self.asset);
        let text = |text: String| match elsewhere {
            Some(asked) => format!("Earlier correction on asset {}: {text}", short(asked)),
            None => text,
        };
        self.status = match result {
            Ok(summary) => Status::Done(text(summary)),
            Err(reason) => Status::Failed(text(reason)),
        };
    }

    /// Apply `action`; pure. `library` is the browser whose open library is edited, and `busy`
    /// says another screen's edit is being written.
    pub fn update(
        &mut self,
        action: RepairAction,
        library: &LibraryScreen,
        busy: bool,
    ) -> Vec<Effect> {
        if let RepairAction::Applied(result) = action {
            self.finish(result.map(|edited| edited.summary));
            return Vec::new();
        }
        let (Some(asset_id), Some(catalogue)) = (self.asset, library.ready()) else {
            return Vec::new();
        };
        let Some(asset) = catalogue.asset(asset_id) else {
            return Vec::new();
        };
        match action {
            RepairAction::EditTime => {
                self.time.set(TIME, &asset.capture_text);
                self.time.focus_on(TIME);
                self.mode = Some(Mode::Time);
                Vec::new()
            }
            RepairAction::EditLocation => {
                let (lat, lon, datum) = asset.location.map_or_else(
                    || (String::new(), String::new(), GpsDatum::Wgs84),
                    |p| (p.lat.to_string(), p.lon.to_string(), p.datum),
                );
                self.location.set(LAT, &lat);
                self.location.set(LON, &lon);
                self.location.set(DATUM, gps_datum_name(datum));
                self.location.focus_on(LAT);
                self.mode = Some(Mode::Location);
                Vec::new()
            }
            RepairAction::ClearLocation => {
                self.request(Edit::ClearLocation { asset_id }, library, busy)
            }
            RepairAction::CopyLocation => {
                let neighbour = self
                    .selected(catalogue)
                    .filter(|id| *id != asset_id)
                    .and_then(|id| catalogue.asset(id));
                match neighbour.map(|n| n.location) {
                    None => {
                        self.status = Status::Failed(
                            "Select a neighbour (Up and Down) to copy its location.".to_owned(),
                        );
                        Vec::new()
                    }
                    Some(None) => {
                        self.status = Status::Failed("That neighbour has no location.".to_owned());
                        Vec::new()
                    }
                    Some(Some(place)) => self.request(
                        Edit::SetLocation {
                            asset_id,
                            lat: place.lat,
                            lon: place.lon,
                            datum: place.datum,
                        },
                        library,
                        busy,
                    ),
                }
            }
            RepairAction::SelectNext | RepairAction::SelectPrevious => {
                if let Some((rows, index)) = self.rows(catalogue) {
                    let index = if action == RepairAction::SelectNext {
                        (index + 1).min(rows.len().saturating_sub(1))
                    } else {
                        index.saturating_sub(1)
                    };
                    self.selection = rows.get(index).copied();
                }
                Vec::new()
            }
            RepairAction::Applied(_) => Vec::new(),
        }
    }

    /// Write `edit` to the open library, unless an edit is already being written.
    fn request(&mut self, edit: Edit, library: &LibraryScreen, busy: bool) -> Vec<Effect> {
        let Some(asset_id) = self.asset else {
            return Vec::new();
        };
        if self.is_working() {
            // Ignored, and the status shows again that the earlier one is still running.
            self.status = Status::Working(WRITING);
            return Vec::new();
        }
        if busy {
            self.status = Status::Failed(
                "Another edit is still being written; try again when it is done.".to_owned(),
            );
            return Vec::new();
        }
        let Some(credentials) = library.credentials() else {
            self.status = Status::Failed("Open the library in the browser first.".to_owned());
            return Vec::new();
        };
        tracing::debug!(target: "capsule_cli::tui", edit = edit.name(), "metadata repair requested");
        self.status = Status::Working(WRITING);
        self.in_flight = Some(asset_id);
        vec![Effect::Repair(RepairEffect::Apply(EditRequest {
            library: credentials.library.clone(),
            passphrase: credentials.passphrase.clone(),
            edit,
        }))]
    }

    /// Apply a key typed into the correction form.
    pub fn on_form_key(
        &mut self,
        key: FormKey,
        library: &LibraryScreen,
        busy: bool,
    ) -> Vec<Effect> {
        let (Some(mode), Some(asset_id)) = (self.mode, self.asset) else {
            return Vec::new();
        };
        let form = match mode {
            Mode::Time => &mut self.time,
            Mode::Location => &mut self.location,
        };
        match form.apply(key) {
            FormOutcome::Editing => Vec::new(),
            FormOutcome::Cancelled => {
                self.mode = None;
                Vec::new()
            }
            FormOutcome::Submitted => {
                let edit = match mode {
                    Mode::Time => self.time.value(TIME).parse::<Timestamp>().map(|capture| {
                        Edit::SetCaptureTime { asset_id, capture }
                    }).map_err(|_| {
                        "Enter an RFC 3339 instant with an offset, such as 2021-07-04T18:30:00Z."
                            .to_owned()
                    }),
                    Mode::Location => self.parse_location(asset_id),
                };
                match edit {
                    Ok(edit) => {
                        let effects = self.request(edit, library, busy);
                        if !effects.is_empty() {
                            self.mode = None;
                        }
                        effects
                    }
                    Err(reason) => {
                        self.status = Status::Failed(reason);
                        Vec::new()
                    }
                }
            }
        }
    }

    fn parse_location(&self, asset_id: Uuid) -> Result<Edit, String> {
        let lat =
            parse_coordinate(self.location.value(LAT)).map_err(|e| format!("Latitude: {e}."))?;
        let lon =
            parse_coordinate(self.location.value(LON)).map_err(|e| format!("Longitude: {e}."))?;
        let datum = parse_datum(self.location.value(DATUM)).map_err(|e| format!("Datum: {e}."))?;
        Ok(Edit::SetLocation {
            asset_id,
            lat,
            lon,
            datum,
        })
    }
}

fn short(id: Uuid) -> String {
    let simple = id.simple().to_string();
    simple[simple.len() - 8..].to_owned()
}

fn asset_lines(asset: &AssetEntry) -> Vec<Line<'static>> {
    let row = |label: &str, value: String| Line::from(format!("{label:<18}{value}"));
    let mut lines = vec![
        Line::from(format!("Asset {} · {}", short(asset.asset_id), asset.ext).bold()),
        row("Captured:", asset.capture_text.clone()),
    ];
    if let Some(imported) = &asset.capture_imported {
        lines.push(row("  as imported:", imported.clone()));
    }
    lines.push(row("Location:", place_text(asset.location)));
    if asset.location_edited {
        lines.push(row("  as imported:", place_text(asset.location_imported)));
    }
    lines
}

/// Draw the metadata repair screen into `area`.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    repair: &MetadataRepair,
    library: &LibraryScreen,
    title: &'static str,
) {
    let block = Block::default().borders(Borders::ALL).title(title);
    let catalogue = library.ready();
    let asset = repair
        .asset
        .zip(catalogue)
        .and_then(|(id, c)| c.asset(id).map(|a| (a, c)));
    let Some((asset, catalogue)) = asset else {
        frame.render_widget(
            Paragraph::new(
                "Open a library in the browser and select an asset; `m` there opens its capture \
                 time and location here."
                    .fg(Color::DarkGray),
            )
            .block(block)
            .wrap(Wrap { trim: false }),
            area,
        );
        return;
    };
    let mut lines = asset_lines(asset);
    lines.push(Line::default());
    lines.push(Line::from("Captured around it, newest first".bold()));
    let selected = repair.selected(catalogue);
    if let Some((rows, _)) = context(catalogue, asset.asset_id, SPAN) {
        for id in rows {
            let Some(other) = catalogue.asset(id) else {
                continue;
            };
            let this = id == asset.asset_id;
            let text = format!(
                "{} {:<26} {:>10}  {}  {}",
                if this { "●" } else { " " },
                other.capture_text,
                if this {
                    "this".to_owned()
                } else {
                    delta(asset.captured, other.captured)
                },
                other.ext,
                place_text(other.location),
            );
            let line = Line::from(text);
            lines.push(if Some(id) == selected {
                line.style(Style::new().add_modifier(Modifier::REVERSED))
            } else {
                line
            });
        }
    }
    lines.push(Line::default());
    if let Some(form) = repair.form() {
        lines.extend(form.lines(true));
    }
    if let Some(status) = repair.status.line() {
        lines.push(status);
    }
    lines.push(Line::from(
        "The original's values are kept beside every correction. Scriptable: `capsule edit \
         capture-time` and `capsule edit location` with --json."
            .fg(Color::DarkGray),
    ));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

#[cfg(test)]
mod tests;
