//! The label editor: add and remove labels on the asset, album or stack selected in the library
//! browser, and on an asset, draw circle and rectangle regions over its preview and label them.
//!
//! It edits the library the browser has open. Every edit is one [`Edit`] written through the
//! port, which opens the library, applies it with [`crate::edit::apply`] (the code
//! `capsule label`, `capsule region` and `capsule edit` run), takes a fresh [`Catalogue`]
//! snapshot and closes the library again, so the import and push screens can still take its
//! lock between edits. The snapshot replaces the browser's, and the editor redraws from it.
//!
//! A region is drawn on the preview as half blocks (`▀`, two pixels a cell), which every colour
//! terminal draws, so a region's outline lands on exactly the cells it covers, whatever image
//! protocol the terminal speaks. The draft is kept in hundredths of the image, so arrow keys
//! move it in exact steps and the shape written is the one drawn. Coordinates are normalized
//! to `[0, 1]` over the display orientation; a circle's radius is a fraction of the image's
//! shorter side, `min(width, height)`, as the sidecar records it, so a circle is drawn round on
//! any aspect ratio.
//!
//! The scriptable twins are `capsule label add|remove`, `capsule region add|delete` and
//! `capsule edit`, each with `--json`.

use std::path::PathBuf;

use capsule_core::sidecar::RegionShape;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use uuid::Uuid;

use super::form::{Field, Form, FormKey, FormOutcome, Secret, Status};
use super::library::{Catalogue, LibraryScreen, PreviewImage, PreviewRequest};
use super::state::{Answer, Effect, Loadable};
use crate::edit::{Edit, LabelTarget};
use crate::utils::ids::short_id as short;

/// The longest edge, in pixels, the region preview is decoded to: half blocks draw two pixels
/// a cell, so this covers a pane far wider than any terminal.
pub const REGION_PREVIEW_EDGE: u32 = 256;

/// How far one arrow key moves or one size key resizes a draft, in hundredths of the image.
pub const STEP: i16 = 2;

/// The smallest radius, width or height a draft shrinks to, in hundredths of the image.
pub const MIN_EXTENT: i16 = 2;

/// The status while an edit asked on this subject is being written.
const WRITING: &str = "Writing the edit…";

/// The status while an edit asked on an earlier subject is being written.
const WRITING_EARLIER: &str = "Writing an edit on the earlier selection…";

/// An edit to write to the library the browser has open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditRequest {
    /// The library.
    pub library: PathBuf,
    /// Its passphrase.
    pub passphrase: Secret,
    /// The edit.
    pub edit: Edit,
}

/// A written edit: what it did in a line, and the library's snapshot afterwards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edited {
    /// The library written to.
    pub library: PathBuf,
    /// [`crate::edit::summary`] of the report.
    pub summary: String,
    /// The library afterwards.
    pub catalogue: Catalogue,
}

/// An arrow key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nudge {
    /// Up.
    Up,
    /// Down.
    Down,
    /// Left.
    Left,
    /// Right.
    Right,
}

/// What the label editor does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelAction {
    /// Type a label for the subject, or for the selected region.
    Add,
    /// Remove the selected label, or ask to delete the selected region.
    Remove,
    /// Start drawing a circle region.
    DrawCircle,
    /// Start drawing a rectangle region.
    DrawRect,
    /// An arrow key: select a row, or move the region being drawn.
    Nudge(Nudge),
    /// Resize the region being drawn by `width` and `height` steps (a circle by either).
    Resize {
        /// Steps wider (negative: narrower).
        width: i8,
        /// Steps taller (negative: shorter).
        height: i8,
    },
    /// Finish drawing: type the new region's label.
    Finish,
    /// [`LabelEffect::Apply`] finished. The root hands the new snapshot to the browser before
    /// the editor sees this.
    Applied(Result<Edited, String>),
    /// [`LabelEffect::Preview`] finished.
    PreviewLoaded {
        /// The asset the preview is of.
        asset_id: Uuid,
        /// The pixels, or why there are none.
        result: Result<PreviewImage, String>,
    },
}

/// The work the label editor asks the ports for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LabelEffect {
    /// Write an edit; answered by [`LabelAction::Applied`].
    Apply(EditRequest),
    /// Decode the asset's original for drawing regions on; answered by
    /// [`LabelAction::PreviewLoaded`].
    Preview(PreviewRequest),
}

impl LabelEffect {
    /// The action reporting this effect failing with `reason`.
    #[must_use]
    pub fn failed(self, reason: String) -> LabelAction {
        match self {
            Self::Apply(_) => LabelAction::Applied(Err(reason)),
            Self::Preview(request) => LabelAction::PreviewLoaded {
                asset_id: request.asset_id,
                result: Err(reason),
            },
        }
    }
}

/// A region being drawn, in hundredths of the image so every step is exact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Draft {
    /// A circle: centre (of width across, of height down) and radius (of the shorter side).
    Circle {
        /// Centre, horizontal.
        cx: i16,
        /// Centre, vertical.
        cy: i16,
        /// Radius.
        r: i16,
    },
    /// A rectangle: top-left corner, width and height.
    Rect {
        /// Left edge.
        x: i16,
        /// Top edge.
        y: i16,
        /// Width.
        w: i16,
        /// Height.
        h: i16,
    },
}

impl Draft {
    /// A new circle in the middle of the image.
    pub const CIRCLE: Self = Self::Circle {
        cx: 50,
        cy: 50,
        r: 16,
    };

    /// A new rectangle in the middle of the image.
    pub const RECT: Self = Self::Rect {
        x: 34,
        y: 34,
        w: 32,
        h: 32,
    };

    /// Move by `(dx, dy)` hundredths, keeping the whole shape inside the image. A circle keeps
    /// `r` from each edge on both axes, since either may be the shorter side (see
    /// [`RegionShape::validate`]).
    #[must_use]
    pub fn moved(self, dx: i16, dy: i16) -> Self {
        match self {
            Self::Circle { cx, cy, r } => {
                // `resized` keeps `r` at most 50; the bound only keeps `clamp` well-formed.
                let edge = r.min(50);
                Self::Circle {
                    cx: (cx + dx).clamp(edge, 100 - edge),
                    cy: (cy + dy).clamp(edge, 100 - edge),
                    r,
                }
            }
            Self::Rect { x, y, w, h } => Self::Rect {
                x: (x + dx).clamp(0, 100 - w),
                y: (y + dy).clamp(0, 100 - h),
                w,
                h,
            },
        }
    }

    /// Resize by `(dw, dh)` hundredths, never below [`MIN_EXTENT`] and never past the image:
    /// a circle's radius changes by the larger step, and grows only until it meets the nearest
    /// edge, on either axis.
    #[must_use]
    pub fn resized(self, dw: i16, dh: i16) -> Self {
        match self {
            Self::Circle { cx, cy, r } => {
                let step = if dw.abs() >= dh.abs() { dw } else { dh };
                let room = cx.min(100 - cx).min(cy).min(100 - cy).max(MIN_EXTENT);
                Self::Circle {
                    cx,
                    cy,
                    r: (r + step).clamp(MIN_EXTENT, room),
                }
            }
            Self::Rect { x, y, w, h } => Self::Rect {
                x,
                y,
                w: (w + dw).clamp(MIN_EXTENT, 100 - x),
                h: (h + dh).clamp(MIN_EXTENT, 100 - y),
            },
        }
    }

    /// The shape to write, in fractions of the image.
    #[must_use]
    pub fn shape(self) -> RegionShape {
        let unit = |v: i16| f32::from(v) / 100.0;
        match self {
            Self::Circle { cx, cy, r } => {
                let (cx, cy) = (unit(cx), unit(cy));
                // As for a rectangle: a centre and radius summing to 100 hundredths can round
                // past 1.0 in f32, and the workspace refuses a circle past the edge.
                let mut r = unit(r);
                while cx < r || cy < r || cx + r > 1.0 || cy + r > 1.0 {
                    r = f32::from_bits(r.to_bits() - 1);
                }
                RegionShape::Circle { cx, cy, r }
            }
            Self::Rect { x, y, w, h } => {
                let (x, y) = (unit(x), unit(y));
                // Two fractions summing to 100 hundredths can round past 1.0 in f32; the
                // workspace refuses a rectangle past the edge, so shave the last bit.
                let fit = |start: f32, extent: f32| {
                    let mut extent = extent;
                    while start + extent > 1.0 {
                        extent = f32::from_bits(extent.to_bits() - 1);
                    }
                    extent
                };
                RegionShape::Rect {
                    x,
                    y,
                    w: fit(x, unit(w)),
                    h: fit(y, unit(h)),
                }
            }
        }
    }
}

/// One row of the editor's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditorRow {
    /// A label of the subject.
    Label(String),
    /// A region of the asset.
    Region(Uuid),
    /// A label of a region.
    RegionLabel(Uuid, String),
}

impl EditorRow {
    /// The region this row belongs to, if any.
    #[must_use]
    pub fn region(&self) -> Option<Uuid> {
        match self {
            Self::Label(_) => None,
            Self::Region(id) | Self::RegionLabel(id, _) => Some(*id),
        }
    }
}

/// What the text being typed labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Typing {
    /// The subject, or one of its regions.
    Label(LabelTarget),
    /// The region being drawn, which is written with it.
    Draft,
}

/// An edit being written: the subject it was asked on, and the draft it writes, if it writes
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct InFlight {
    subject: LabelTarget,
    draft: Option<Draft>,
}

/// The label editor's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabelEditor {
    /// The asset, album or stack being labelled; never a region.
    subject: Option<LabelTarget>,
    cursor: usize,
    form: Form,
    typing: Option<Typing>,
    draft: Option<Draft>,
    /// The region whose deletion waits for a yes or no.
    deleting: Option<Uuid>,
    status: Status,
    /// The edit being written. Kept apart from `status`, which any key may rewrite, and
    /// cleared only when the edit's answer arrives.
    in_flight: Option<InFlight>,
    /// The subject asset's decoded preview, for drawing regions on.
    preview: Option<(Uuid, Loadable<PreviewImage>)>,
}

impl Default for LabelEditor {
    fn default() -> Self {
        Self {
            subject: None,
            cursor: 0,
            form: Form::new(vec![Field::text("Label")]),
            typing: None,
            draft: None,
            deleting: None,
            status: Status::Idle,
            in_flight: None,
            preview: None,
        }
    }
}

/// The subject's labels in `catalogue`, or why they cannot be read.
fn subject_labels(catalogue: &Catalogue, subject: LabelTarget) -> Result<Vec<String>, String> {
    match subject {
        LabelTarget::Asset(id) => catalogue
            .asset(id)
            .map(|a| a.labels.clone())
            .ok_or_else(|| "the asset is no longer in this library".to_owned()),
        LabelTarget::Album(id) => catalogue
            .album(id)
            .ok_or_else(|| "the album is no longer in this library".to_owned())
            .and_then(|a| a.labels.clone()),
        LabelTarget::Stack(id) => catalogue
            .stack(id)
            .ok_or_else(|| "the stack is no longer in this library".to_owned())
            .and_then(|s| s.labels.clone()),
        LabelTarget::Region {
            asset_id,
            region_id,
        } => catalogue
            .asset(asset_id)
            .and_then(|a| a.regions.iter().find(|r| r.region_id == region_id))
            .map(|r| r.labels.clone())
            .ok_or_else(|| "the region is no longer on this asset".to_owned()),
    }
}

impl LabelEditor {
    /// Whether keys go into the label form.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.typing.is_some()
    }

    /// Whether a region deletion waits for a yes or no.
    #[must_use]
    pub fn is_confirming(&self) -> bool {
        self.deleting.is_some()
    }

    /// Whether an edit is being written.
    #[must_use]
    pub fn is_working(&self) -> bool {
        self.in_flight.is_some()
    }

    /// The asset, album or stack being labelled.
    #[must_use]
    pub fn subject(&self) -> Option<LabelTarget> {
        self.subject
    }

    /// The region being drawn.
    #[must_use]
    pub fn draft(&self) -> Option<Draft> {
        self.draft
    }

    /// Where the last edit stands.
    #[must_use]
    pub fn status(&self) -> &Status {
        &self.status
    }

    /// The label form.
    #[must_use]
    pub fn form(&self) -> &Form {
        &self.form
    }

    /// The decoded preview regions are drawn on, and where it stands.
    #[must_use]
    pub fn preview(&self) -> Option<&(Uuid, Loadable<PreviewImage>)> {
        self.preview.as_ref()
    }

    /// Label `subject` in the library `library` has open: the editor starts afresh on it, and
    /// an asset's original is decoded for drawing regions on.
    pub fn begin(&mut self, subject: LabelTarget, library: &LibraryScreen) -> Vec<Effect> {
        tracing::debug!(target: "capsule_cli::tui", ?subject, "label editor opened");
        // An edit still being written stays in flight, so a second one cannot start, and its
        // answer lands here; one asked on another subject says so when it lands.
        let in_flight = self.in_flight;
        let status = match in_flight {
            Some(flight) if flight.subject == subject => Status::Working(WRITING),
            Some(_) => Status::Working(WRITING_EARLIER),
            None => Status::Idle,
        };
        let preview = self.preview.take();
        *self = Self {
            subject: Some(subject),
            status,
            in_flight,
            ..Self::default()
        };
        let LabelTarget::Asset(asset_id) = subject else {
            return Vec::new();
        };
        if let Some(kept) = preview.filter(|(id, _)| *id == asset_id) {
            self.preview = Some(kept);
            return Vec::new();
        }
        let Some(asset) = library.ready().and_then(|c| c.asset(asset_id)) else {
            return Vec::new();
        };
        let Some(original) = asset.original.clone() else {
            self.preview = Some((
                asset_id,
                Loadable::Failed("the original is not on this device".to_owned()),
            ));
            return Vec::new();
        };
        self.preview = Some((asset_id, Loadable::Loading));
        vec![Effect::Labels(LabelEffect::Preview(PreviewRequest {
            asset_id,
            original,
            ext: asset.ext.clone(),
        }))]
    }

    /// The rows the list shows: the subject's labels, then (for an asset) each region and its
    /// labels.
    #[must_use]
    pub fn rows(&self, catalogue: &Catalogue) -> Vec<EditorRow> {
        let Some(subject) = self.subject else {
            return Vec::new();
        };
        let mut rows: Vec<EditorRow> = subject_labels(catalogue, subject)
            .unwrap_or_default()
            .into_iter()
            .map(EditorRow::Label)
            .collect();
        if let LabelTarget::Asset(asset_id) = subject
            && let Some(asset) = catalogue.asset(asset_id)
        {
            for region in &asset.regions {
                rows.push(EditorRow::Region(region.region_id));
                rows.extend(
                    region
                        .labels
                        .iter()
                        .map(|l| EditorRow::RegionLabel(region.region_id, l.clone())),
                );
            }
        }
        rows
    }

    /// The selected row's index, kept inside the list.
    #[must_use]
    pub fn cursor(&self, catalogue: &Catalogue) -> usize {
        self.cursor
            .min(self.rows(catalogue).len().saturating_sub(1))
    }

    /// The selected row, if the list has any.
    #[must_use]
    pub fn selected(&self, catalogue: &Catalogue) -> Option<EditorRow> {
        let rows = self.rows(catalogue);
        let last = rows.len().checked_sub(1)?;
        rows.get(self.cursor.min(last)).cloned()
    }

    /// Esc while a region is being drawn throws the draft away instead of leaving the screen:
    /// whether it did.
    pub fn cancel_draft(&mut self) -> bool {
        if self.typing.is_none() && self.draft.take().is_some() {
            if !self.is_working() {
                self.status = Status::Idle;
            }
            return true;
        }
        false
    }

    /// An edit finished: what it did, or why it was not written. The new snapshot has already
    /// reached the browser.
    ///
    /// The edit is no longer in flight. An answer for another subject than the one now open
    /// names it; a region written from the draft drops the draft, and one that was not keeps
    /// it, so a failed write loses nothing drawn.
    pub fn finish(&mut self, result: Result<String, String>) {
        let flight = self.in_flight.take();
        let elsewhere = flight
            .map(|f| f.subject)
            .filter(|asked| Some(*asked) != self.subject);
        if result.is_ok()
            && elsewhere.is_none()
            && let Some(written) = flight.and_then(|f| f.draft)
            && self.draft == Some(written)
        {
            self.draft = None;
        }
        let text = |text: String| match elsewhere {
            Some(asked) => format!("Earlier edit on {}: {text}", target_name(asked)),
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
        action: LabelAction,
        library: &LibraryScreen,
        busy: bool,
    ) -> Vec<Effect> {
        match action {
            LabelAction::Applied(result) => {
                self.finish(result.map(|edited| edited.summary));
                return Vec::new();
            }
            LabelAction::PreviewLoaded { asset_id, result } => {
                if let Some((wanted, slot)) = &mut self.preview
                    && *wanted == asset_id
                {
                    *slot = match result {
                        Ok(image) => Loadable::Ready(image),
                        Err(reason) => Loadable::Failed(reason),
                    };
                }
                return Vec::new();
            }
            _ => {}
        }
        let (Some(subject), Some(catalogue)) = (self.subject, library.ready()) else {
            return Vec::new();
        };
        match action {
            LabelAction::Add => {
                let target = match (subject, self.selected(catalogue).and_then(|r| r.region())) {
                    (LabelTarget::Asset(asset_id), Some(region_id)) => LabelTarget::Region {
                        asset_id,
                        region_id,
                    },
                    _ => subject,
                };
                self.start_typing(Typing::Label(target));
                Vec::new()
            }
            LabelAction::Remove => match (subject, self.selected(catalogue)) {
                (_, Some(EditorRow::Label(label))) => self.request(
                    Edit::RemoveLabel {
                        target: subject,
                        label,
                    },
                    None,
                    library,
                    busy,
                ),
                (LabelTarget::Asset(asset_id), Some(EditorRow::RegionLabel(region_id, label))) => {
                    self.request(
                        Edit::RemoveLabel {
                            target: LabelTarget::Region {
                                asset_id,
                                region_id,
                            },
                            label,
                        },
                        None,
                        library,
                        busy,
                    )
                }
                (_, Some(EditorRow::Region(region_id))) => {
                    self.deleting = Some(region_id);
                    self.status = Status::Asking(format!(
                        "Delete region {}? Deleting a region is final. y/n",
                        short(region_id)
                    ));
                    Vec::new()
                }
                _ => Vec::new(),
            },
            LabelAction::DrawCircle | LabelAction::DrawRect => {
                if matches!(subject, LabelTarget::Asset(_)) {
                    self.draft = Some(if action == LabelAction::DrawCircle {
                        Draft::CIRCLE
                    } else {
                        Draft::RECT
                    });
                    if !self.is_working() {
                        self.status = Status::Idle;
                    }
                }
                Vec::new()
            }
            LabelAction::Nudge(nudge) => {
                if let Some(draft) = self.draft {
                    let (dx, dy) = match nudge {
                        Nudge::Up => (0, -STEP),
                        Nudge::Down => (0, STEP),
                        Nudge::Left => (-STEP, 0),
                        Nudge::Right => (STEP, 0),
                    };
                    self.draft = Some(draft.moved(dx, dy));
                } else {
                    let last = self.rows(catalogue).len().saturating_sub(1);
                    let cursor = self.cursor.min(last);
                    self.cursor = match nudge {
                        Nudge::Up => cursor.saturating_sub(1),
                        Nudge::Down => (cursor + 1).min(last),
                        Nudge::Left | Nudge::Right => cursor,
                    };
                }
                Vec::new()
            }
            LabelAction::Resize { width, height } => {
                if let Some(draft) = self.draft {
                    self.draft =
                        Some(draft.resized(i16::from(width) * STEP, i16::from(height) * STEP));
                }
                Vec::new()
            }
            LabelAction::Finish => {
                if self.draft.is_some() {
                    self.start_typing(Typing::Draft);
                }
                Vec::new()
            }
            LabelAction::Applied(_) | LabelAction::PreviewLoaded { .. } => Vec::new(),
        }
    }

    fn start_typing(&mut self, typing: Typing) {
        self.form.set(0, "");
        self.form.focus_on(0);
        self.typing = Some(typing);
    }

    /// Write `edit` to the open library, unless an edit is already being written. `draft` is
    /// the drawn region `edit` writes, which is kept until the write succeeds.
    fn request(
        &mut self,
        edit: Edit,
        draft: Option<Draft>,
        library: &LibraryScreen,
        busy: bool,
    ) -> Vec<Effect> {
        let Some(subject) = self.subject else {
            return Vec::new();
        };
        if self.is_working() {
            // Ignored, and the status shows again that the earlier edit is still running.
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
        tracing::debug!(target: "capsule_cli::tui", edit = edit.name(), "label edit requested");
        self.status = Status::Working(WRITING);
        self.in_flight = Some(InFlight { subject, draft });
        vec![Effect::Labels(LabelEffect::Apply(EditRequest {
            library: credentials.library.clone(),
            passphrase: credentials.passphrase.clone(),
            edit,
        }))]
    }

    /// Apply a key typed into the label form.
    pub fn on_form_key(
        &mut self,
        key: FormKey,
        library: &LibraryScreen,
        busy: bool,
    ) -> Vec<Effect> {
        let Some(typing) = self.typing else {
            return Vec::new();
        };
        match self.form.apply(key) {
            FormOutcome::Editing => Vec::new(),
            FormOutcome::Cancelled => {
                self.typing = None;
                Vec::new()
            }
            FormOutcome::Submitted => {
                let label = self.form.value(0).to_owned();
                let edit = match typing {
                    Typing::Label(_) if label.is_empty() => {
                        self.status = Status::Failed("Type a label first.".to_owned());
                        return Vec::new();
                    }
                    Typing::Label(target) => Edit::AddLabel { target, label },
                    Typing::Draft => {
                        let (Some(LabelTarget::Asset(asset_id)), Some(draft)) =
                            (self.subject, self.draft)
                        else {
                            self.typing = None;
                            return Vec::new();
                        };
                        Edit::AddRegion {
                            asset_id,
                            shape: draft.shape(),
                            labels: if label.is_empty() {
                                Vec::new()
                            } else {
                                vec![label]
                            },
                        }
                    }
                };
                // The draft stays until the region is written, so a failed write loses nothing.
                let draft = (typing == Typing::Draft).then_some(self.draft).flatten();
                let effects = self.request(edit, draft, library, busy);
                if !effects.is_empty() {
                    self.typing = None;
                }
                effects
            }
        }
    }

    /// The answer to "delete this region?".
    pub fn on_answer(
        &mut self,
        answer: Answer,
        library: &LibraryScreen,
        busy: bool,
    ) -> Vec<Effect> {
        let (Some(region_id), Some(LabelTarget::Asset(asset_id))) =
            (self.deleting.take(), self.subject)
        else {
            return Vec::new();
        };
        self.status = if self.is_working() {
            Status::Working(WRITING)
        } else {
            Status::Idle
        };
        match answer {
            Answer::Yes => self.request(
                Edit::DeleteRegion {
                    asset_id,
                    region_id,
                },
                None,
                library,
                busy,
            ),
            Answer::No => Vec::new(),
        }
    }
}

/// What a label target is called in a status line: `asset 1a2b3c4d`.
fn target_name(target: LabelTarget) -> String {
    match target {
        LabelTarget::Asset(id) => format!("asset {}", short(id)),
        LabelTarget::Album(id) => format!("album {}", short(id)),
        LabelTarget::Stack(id) => format!("stack {}", short(id)),
        LabelTarget::Region { region_id, .. } => format!("region {}", short(region_id)),
    }
}

/// How an outline is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Highlight {
    /// A region at rest.
    Region,
    /// The selected region.
    Selected,
    /// The region being drawn.
    Draft,
}

impl Highlight {
    const fn color(self) -> Color {
        match self {
            Self::Region => Color::Cyan,
            Self::Selected => Color::Yellow,
            Self::Draft => Color::Magenta,
        }
    }
}

/// Whether the point `(u, v)`, in fractions of the image's width and height, is inside
/// `shape`, on an image `width`×`height` (any unit). A circle's radius is a fraction of the
/// shorter side, as [`RegionShape::Circle`] records it, so it covers a round disc on any aspect
/// ratio.
fn inside(shape: RegionShape, u: f32, v: f32, width: f32, height: f32) -> bool {
    match shape {
        RegionShape::Circle { cx, cy, r } => {
            let radius = r * width.min(height);
            let (dx, dy) = ((u - cx) * width / radius, (v - cy) * height / radius);
            dx * dx + dy * dy <= 1.0
        }
        RegionShape::Rect { x, y, w, h } => u >= x && u <= x + w && v >= y && v <= y + h,
    }
}

/// The preview fitted into `cols`×`rows` cells as half blocks (two pixels a cell), with each
/// shape's outline drawn over it in its highlight's colour: one line per row. With no image the
/// frame is blank and fills the pane; the outlines are drawn all the same.
///
/// Pure, so the pixels a region covers are tested without a terminal. A pixel is on an outline
/// when it is inside the shape and a neighbour is not.
#[must_use]
pub fn compose(
    image: Option<&PreviewImage>,
    cols: u16,
    rows: u16,
    shapes: &[(RegionShape, Highlight)],
) -> Vec<Line<'static>> {
    let (cols, rows) = (usize::from(cols), usize::from(rows));
    if cols == 0 || rows == 0 {
        return Vec::new();
    }
    // The pixel grid: `cols` wide, `rows * 2` tall, fitted to the image's aspect ratio.
    let (grid_w, grid_h) = (cols, rows * 2);
    let (fit_w, fit_h) = match image {
        Some(image) if image.width > 0 && image.height > 0 => {
            let (iw, ih) = (image.width as usize, image.height as usize);
            if grid_w * ih <= grid_h * iw {
                (grid_w, (grid_w * ih / iw).max(1))
            } else {
                ((grid_h * iw / ih).max(1), grid_h)
            }
        }
        _ => (grid_w, grid_h),
    };
    let (left, top) = ((grid_w - fit_w) / 2, (grid_h - fit_h) / 2);
    let (fw, fh) = (fit_w as f32, fit_h as f32);
    let inside_at = |shape: RegionShape, px: isize, py: isize| {
        let (u, v) = ((px as f32 + 0.5) / fw, (py as f32 + 0.5) / fh);
        px >= 0
            && py >= 0
            && px < fit_w.cast_signed()
            && py < fit_h.cast_signed()
            && inside(shape, u, v, fw, fh)
    };
    let pixel = |gx: usize, gy: usize| -> Option<Color> {
        if gx < left || gy < top || gx >= left + fit_w || gy >= top + fit_h {
            return None;
        }
        let (px, py) = ((gx - left).cast_signed(), (gy - top).cast_signed());
        // The last shape listed is drawn on top: the draft and the selection come last.
        for (shape, highlight) in shapes.iter().rev() {
            let on = inside_at(*shape, px, py)
                && [(-1, 0), (1, 0), (0, -1), (0, 1)]
                    .iter()
                    .any(|(dx, dy)| !inside_at(*shape, px + dx, py + dy));
            if on {
                return Some(highlight.color());
            }
        }
        let image = image?;
        let ix = ((gx - left) * image.width as usize / fit_w).min(image.width as usize - 1);
        let iy = ((gy - top) * image.height as usize / fit_h).min(image.height as usize - 1);
        let at = (iy * image.width as usize + ix) * 4;
        image
            .rgba
            .get(at..at + 3)
            .map(|rgb| Color::Rgb(rgb[0], rgb[1], rgb[2]))
    };
    (0..rows)
        .map(|row| {
            let spans: Vec<Span<'static>> = (0..cols)
                .map(|col| match (pixel(col, row * 2), pixel(col, row * 2 + 1)) {
                    (None, None) => Span::raw(" "),
                    (top, bottom) => Span::styled(
                        "▀",
                        Style::new()
                            .fg(top.unwrap_or(Color::Reset))
                            .bg(bottom.unwrap_or(Color::Reset)),
                    ),
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

/// What the subject is called in the heading.
fn subject_name(catalogue: &Catalogue, subject: LabelTarget) -> String {
    match subject {
        LabelTarget::Asset(id) => catalogue.asset(id).map_or_else(
            || format!("Asset {}", short(id)),
            |a| format!("Asset {} · {} · {}", short(id), a.capture_text, a.ext),
        ),
        LabelTarget::Album(id) => catalogue.album(id).map_or_else(
            || format!("Album {}", short(id)),
            |a| format!("Album {}", a.name),
        ),
        LabelTarget::Stack(id) => format!("Stack {}", short(id)),
        LabelTarget::Region { region_id, .. } => format!("Region {}", short(region_id)),
    }
}

/// Draw the label editor into `area`.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    editor: &LabelEditor,
    library: &LibraryScreen,
    title: &'static str,
) {
    let block = Block::default().borders(Borders::ALL).title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let (Some(subject), Some(catalogue)) = (editor.subject, library.ready()) else {
        frame.render_widget(
            Paragraph::new(
                "Open a library in the browser and select an asset, album or stack; `e` there \
                 opens its labels here."
                    .fg(Color::DarkGray),
            )
            .wrap(Wrap { trim: false }),
            inner,
        );
        return;
    };
    let is_asset = matches!(subject, LabelTarget::Asset(_));
    let [list_area, canvas_area] = if is_asset {
        Layout::horizontal([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(inner)
    } else {
        [inner, Rect::default()]
    };

    let mut lines = vec![Line::from(subject_name(catalogue, subject).bold())];
    if let Err(reason) = subject_labels(catalogue, subject) {
        lines.push(Line::from(
            format!("Labels unreadable: {reason}").fg(Color::Red),
        ));
    }
    let rows = editor.rows(catalogue);
    let cursor = editor.cursor(catalogue);
    if rows.is_empty() {
        lines.push(Line::from(
            "No labels yet. `a` adds one.".fg(Color::DarkGray),
        ));
    }
    let regions = catalogue
        .asset(match subject {
            LabelTarget::Asset(id) => id,
            _ => Uuid::nil(),
        })
        .map_or(&[][..], |a| a.regions.as_slice());
    for (at, row) in rows.iter().enumerate() {
        let text = match row {
            EditorRow::Label(label) => format!("label   {label}"),
            EditorRow::Region(id) => {
                let shape =
                    regions
                        .iter()
                        .find(|r| r.region_id == *id)
                        .map_or("", |r| match r.outline.0 {
                            RegionShape::Circle { .. } => "circle",
                            RegionShape::Rect { .. } => "rectangle",
                        });
                format!("region  {} {shape}", short(*id))
            }
            EditorRow::RegionLabel(_, label) => format!("          {label}"),
        };
        let selected = at == cursor && editor.draft.is_none();
        lines.push(if selected {
            Line::from(format!("› {text}")).style(Style::new().add_modifier(Modifier::REVERSED))
        } else {
            Line::from(format!("  {text}"))
        });
    }
    lines.push(Line::default());
    if let Some(typing) = editor.typing {
        lines.push(Line::from(match typing {
            Typing::Label(LabelTarget::Region { region_id, .. }) => {
                format!("A label for region {}:", short(region_id))
            }
            Typing::Label(_) => "A label:".to_owned(),
            Typing::Draft => {
                "The new region's label (Enter with none leaves it unlabelled):".to_owned()
            }
        }));
        lines.extend(editor.form.lines(true));
    } else if editor.draft.is_some() {
        lines.push(Line::from(
            "Drawing: arrows move it, + and - resize it, ] [ widen and narrow, } { heighten and \
             lower a rectangle; Enter labels and writes it, Esc drops it."
                .fg(Color::DarkGray),
        ));
    }
    if let Some(status) = editor.status.line() {
        lines.push(status);
    }
    lines.push(Line::from(
        "Scriptable: `capsule label`, `capsule region` and `capsule edit` with --json."
            .fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), list_area);

    if is_asset {
        render_canvas(frame, canvas_area, editor, &rows, cursor, regions);
    }
}

fn render_canvas(
    frame: &mut Frame,
    area: Rect,
    editor: &LabelEditor,
    rows: &[EditorRow],
    cursor: usize,
    regions: &[super::library::RegionEntry],
) {
    let block = Block::default().borders(Borders::ALL).title("Regions");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [canvas, note] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    let selected = if editor.draft.is_none() {
        rows.get(cursor).and_then(EditorRow::region)
    } else {
        None
    };
    let mut shapes: Vec<(RegionShape, Highlight)> = regions
        .iter()
        .filter(|r| Some(r.region_id) != selected)
        .map(|r| (r.outline.0, Highlight::Region))
        .collect();
    shapes.extend(
        regions
            .iter()
            .filter(|r| Some(r.region_id) == selected)
            .map(|r| (r.outline.0, Highlight::Selected)),
    );
    if let Some(draft) = editor.draft {
        shapes.push((draft.shape(), Highlight::Draft));
    }
    let (image, note_text) = match &editor.preview {
        Some((_, Loadable::Ready(image))) => (Some(image), None),
        Some((_, Loadable::Failed(reason))) => (
            None,
            Some(format!(
                "No preview: {reason}. Regions are drawn on a blank frame."
            )),
        ),
        _ => (None, Some("Loading the preview…".to_owned())),
    };
    frame.render_widget(
        Paragraph::new(compose(image, canvas.width, canvas.height, &shapes)),
        canvas,
    );
    if let Some(text) = note_text {
        frame.render_widget(Paragraph::new(text.fg(Color::DarkGray)), note);
    }
}

#[cfg(test)]
mod tests;
