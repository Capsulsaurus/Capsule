//! The library browser: a local library's timeline, albums and stacks, a detail pane with each
//! asset's metadata, labels and provenance history, and an inline preview where the terminal
//! can draw images.
//!
//! The browser only reads. Opening a library unlocks it once, through the port, which takes a
//! [`Catalogue`] snapshot and closes the workspace again; every move after that is a pure
//! transition over the snapshot. The preview is the one other effect: the port decodes the
//! selected asset's original through `capsule-core::media` (Rawshift) and downscales it, one at
//! a time, latest selection wins.
//!
//! Drawing the preview needs it encoded in the terminal's graphics protocol for the pane's
//! size, which only a frame knows. A [`PreviewCanvas`] notes the encode a frame found missing,
//! the runtime runs it through the port off the UI loop and behind an unwind guard, and the
//! encoding comes back to the state; a frame never encodes. Where the terminal answered with
//! no image protocol, or the encode fails or panics, the pane says so in text.
//!
//! The scriptable twin is `capsule show ASSET --library PATH`, which prints the same rows the
//! detail pane draws.

pub mod catalogue;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use capsule_core::media::{self, Decoder};
use capsule_core::sidecar::StackRole;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect, Size};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui_image::picker::cap_parser::QueryStdioOptions;
use ratatui_image::picker::{Picker, ProtocolType};
use ratatui_image::protocol::Protocol;
use ratatui_image::{FilterType, Image, Resize};
use uuid::Uuid;

pub use self::catalogue::{AlbumEntry, AssetEntry, Catalogue, HistoryEntry, StackEntry};
use super::form::{Field, Form, FormKey, FormOutcome, Secret};
use super::state::{Effect, Loadable};

/// How long the start-up query waits for the terminal to say which image protocol it speaks.
/// A terminal answers in a few milliseconds; one that does not answer at all is treated as
/// drawing no images, and this is the most it delays the first frame.
pub const PICKER_QUERY_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// The longest edge, in pixels, the port downscales a preview to before it is kept: enough for
/// a full-screen pane on a large terminal, small enough to hold one in memory.
pub const PREVIEW_EDGE: u32 = 1024;

/// What the preview pane says where the terminal draws no images.
pub const NO_IMAGE_PROTOCOL: &str =
    "this terminal draws no inline images (Kitty, Sixel or iTerm2 graphics)";

/// A library to open, as the form collected it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRequest {
    /// The library directory.
    pub library: PathBuf,
    /// Its passphrase.
    pub passphrase: Secret,
}

/// An asset's original to decode for the preview pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewRequest {
    /// The asset.
    pub asset_id: Uuid,
    /// Its plaintext original on disk.
    pub original: PathBuf,
    /// The original's extension, which the decoder's format gate reads.
    pub ext: String,
}

/// A decoded, downscaled preview: upright, packed RGBA8.
#[derive(Clone, PartialEq, Eq)]
pub struct PreviewImage {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// `width * height * 4` bytes, shared so the state clones cheaply.
    pub rgba: Arc<[u8]>,
}

impl std::fmt::Debug for PreviewImage {
    // The pixels never reach the log: a trace of every action would carry megabytes.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreviewImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

/// Whether an original with extension `ext` is worth reading for a preview: a still format
/// this build decodes. Checked before the read, so a video or an undecodable format never has
/// its whole original loaded only to be refused.
///
/// # Errors
///
/// Why there is no preview for this format.
pub fn previewable(ext: &str) -> Result<(), String> {
    match media::StillFormat::from_extension(ext) {
        Some(format) if format.is_decodable() => Ok(()),
        Some(format) => Err(format!("this build has no decoder for {format} stills")),
        None => Err(format!(
            "a .{ext} original is not a still this build decodes"
        )),
    }
}

/// Decode `bytes` (an original with extension `ext`) into a preview no larger than
/// `max_edge` on its long edge, behind the media module's unwind guard: a codec that panics on
/// a hostile original costs the preview, never the TUI. A format with no codec in this build
/// is the reason the pane shows.
///
/// # Errors
///
/// The media module's reason the original does not decode.
#[tracing::instrument(level = "debug", skip(decoder, bytes), fields(bytes = bytes.len()))]
pub fn decode_preview(
    decoder: &dyn Decoder,
    bytes: &[u8],
    ext: &str,
    max_edge: u32,
) -> Result<PreviewImage, String> {
    let decoded = media::decode_guarded(decoder, bytes, ext).map_err(|e| e.to_string())?;
    let image = media::guarded("preview downscale", || {
        Ok(media::downscale_rgba8(&decoded.image, max_edge))
    })
    .map_err(|e| e.to_string())?;
    Ok(PreviewImage {
        width: image.width,
        height: image.height,
        rgba: image.rgba.into(),
    })
}

/// A decoded preview to encode for the terminal, fitted to a pane of `size` cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncodeRequest {
    /// Which decoded preview this is ([`LibraryScreen`] counts them), so an answer for one the
    /// selection has moved past is dropped.
    pub generation: u64,
    /// The pixels.
    pub image: PreviewImage,
    /// The pane, in terminal cells.
    pub size: Size,
}

/// A preview encoded in the terminal's graphics protocol, ready to draw. Shared, so the state
/// holding it clones cheaply; two are equal only when they are the same encoding.
#[derive(Clone)]
pub struct EncodedPreview(Arc<Protocol>);

impl PartialEq for EncodedPreview {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for EncodedPreview {}

impl std::fmt::Debug for EncodedPreview {
    // The escape sequences never reach the log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncodedPreview").finish_non_exhaustive()
    }
}

/// Run a terminal image encode behind an unwind guard: an encoder that panics costs the
/// preview, never the TUI, and the pane falls back to text with the reason.
///
/// # Errors
///
/// The encoder's reason, or that it panicked.
pub fn encode_guarded(
    encode: impl FnOnce() -> Result<Protocol, String>,
) -> Result<EncodedPreview, String> {
    if let Ok(encoded) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(encode)) {
        return encoded.map(|protocol| EncodedPreview(Arc::new(protocol)));
    }
    tracing::warn!(target: "capsule_cli::tui", "the terminal image encoder panicked; the preview is text");
    Err("the terminal image encoder panicked".to_owned())
}

/// Encode `request`'s preview with `picker`'s protocol: resize it to the pane and encode it
/// (Sixel quantization, Kitty or iTerm2 transmission). Slow on a large pane, so the port runs
/// it off the UI loop, behind [`encode_guarded`].
///
/// # Errors
///
/// Why the preview could not be encoded.
#[tracing::instrument(level = "debug", skip_all, fields(generation = request.generation, size = ?request.size))]
pub fn encode_preview(picker: &Picker, request: EncodeRequest) -> Result<EncodedPreview, String> {
    let EncodeRequest { image, size, .. } = request;
    encode_guarded(|| {
        let buffer = image::RgbaImage::from_raw(image.width, image.height, image.rgba.to_vec())
            .ok_or_else(|| "the decoded preview has the wrong length".to_owned())?;
        picker
            .new_protocol(
                image::DynamicImage::ImageRgba8(buffer),
                size,
                Resize::Fit(Some(FilterType::Triangle)),
            )
            .map_err(|e| format!("the terminal image could not be encoded: {e}"))
    })
}

/// What the browser does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryAction {
    /// Start typing into the open form.
    Edit,
    /// Show the next view: timeline, albums, stacks, and round again.
    NextView,
    /// Select the next row.
    SelectNext,
    /// Select the previous row.
    SelectPrevious,
    /// List the selected album's or stack's assets.
    Enter,
    /// Leave the album or stack being listed.
    Leave,
    /// [`LibraryEffect::Open`] finished.
    Opened(Result<Catalogue, String>),
    /// [`LibraryEffect::Preview`] finished.
    PreviewLoaded {
        /// The asset the preview is of.
        asset_id: Uuid,
        /// The pixels, or why there are none.
        result: Result<PreviewImage, String>,
    },
    /// The canvas asked for an encode of `generation` at `size`. Only the latest ask is
    /// accepted back: an answer for any other pair (a pane since resized) is dropped.
    EncodeRequested {
        /// The decoded preview to encode.
        generation: u64,
        /// The pane to fit it to.
        size: Size,
    },
    /// [`LibraryEffect::Encode`] finished.
    PreviewEncoded {
        /// The decoded preview it encodes.
        generation: u64,
        /// The pane it was fitted to.
        size: Size,
        /// The encoding, or why there is none.
        result: Result<EncodedPreview, String>,
    },
}

/// The work the browser asks the ports for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LibraryEffect {
    /// Open a library and snapshot it; answered by [`LibraryAction::Opened`].
    Open(OpenRequest),
    /// Decode a preview; answered by [`LibraryAction::PreviewLoaded`].
    Preview(PreviewRequest),
    /// Encode a decoded preview for the terminal; answered by
    /// [`LibraryAction::PreviewEncoded`]. Asked by the [`PreviewCanvas`], which alone knows the
    /// pane's size.
    Encode(EncodeRequest),
}

impl LibraryEffect {
    /// The action reporting this effect failing with `reason`.
    #[must_use]
    pub fn failed(self, reason: String) -> LibraryAction {
        match self {
            Self::Open(_) => LibraryAction::Opened(Err(reason)),
            Self::Preview(request) => LibraryAction::PreviewLoaded {
                asset_id: request.asset_id,
                result: Err(reason),
            },
            Self::Encode(request) => LibraryAction::PreviewEncoded {
                generation: request.generation,
                size: request.size,
                result: Err(reason),
            },
        }
    }
}

/// The three ways into a library.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Every visible asset, newest first; a stack shows as its primary.
    Timeline,
    /// The albums, each opening onto its assets.
    Albums,
    /// The stacks, each opening onto its members.
    Stacks,
}

impl View {
    const ALL: [Self; 3] = [Self::Timeline, Self::Albums, Self::Stacks];

    const fn index(self) -> usize {
        match self {
            Self::Timeline => 0,
            Self::Albums => 1,
            Self::Stacks => 2,
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Timeline => Self::Albums,
            Self::Albums => Self::Stacks,
            Self::Stacks => Self::Timeline,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Timeline => "Timeline",
            Self::Albums => "Albums",
            Self::Stacks => "Stacks",
        }
    }
}

/// An album or stack whose assets are being listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// An album.
    Album(Uuid),
    /// A stack.
    Stack(Uuid),
}

/// One row of the list on the left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// An asset.
    Asset(Uuid),
    /// An album.
    Album(Uuid),
    /// A stack.
    Stack(Uuid),
}

/// The list a view or group shows, borrowed from the catalogue.
#[derive(Debug, Clone, Copy)]
enum RowSource<'a> {
    /// Assets by id: the timeline or an album.
    Assets(&'a [Uuid]),
    /// A stack's members.
    Members(&'a [(Uuid, StackRole)]),
    /// The albums.
    Albums(&'a [AlbumEntry]),
    /// The stacks.
    Stacks(&'a [StackEntry]),
}

impl RowSource<'_> {
    const fn len(self) -> usize {
        match self {
            Self::Assets(ids) => ids.len(),
            Self::Members(members) => members.len(),
            Self::Albums(albums) => albums.len(),
            Self::Stacks(stacks) => stacks.len(),
        }
    }

    fn get(self, at: usize) -> Option<Row> {
        match self {
            Self::Assets(ids) => ids.get(at).copied().map(Row::Asset),
            Self::Members(members) => members.get(at).map(|(id, _)| Row::Asset(*id)),
            Self::Albums(albums) => albums.get(at).map(|a| Row::Album(a.album_id)),
            Self::Stacks(stacks) => stacks.get(at).map(|s| Row::Stack(s.stack_id)),
        }
    }
}

/// The rows of a `len`-row list that fit a `height`-row viewport with row `cursor` selected:
/// the same window the list widget scrolls to on its own (the selection kept in view, at the
/// bottom once it passes the first screen), so only those rows are built each frame.
#[must_use]
pub fn visible_window(cursor: usize, len: usize, height: usize) -> std::ops::Range<usize> {
    if len == 0 || height == 0 {
        return 0..0;
    }
    let cursor = cursor.min(len - 1);
    let start = cursor.saturating_sub(height - 1);
    start..(start + height).min(len)
}

const LIBRARY: usize = 0;
const PASSPHRASE: usize = 1;

/// The browser's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryScreen {
    form: Form,
    editing: bool,
    /// Why the form was refused before anything ran.
    invalid: Option<String>,
    /// The library last asked for, which the title names.
    library: Option<PathBuf>,
    catalogue: Option<Loadable<Catalogue>>,
    view: View,
    /// The selected row of each view's top-level list.
    cursors: [usize; 3],
    /// The album or stack being listed, and the selected row in it.
    group: Option<(Group, usize)>,
    /// The latest preview asked for and where it stands.
    preview: Option<(Uuid, Loadable<PreviewImage>)>,
    /// A preview decode is running: the next waits for it, so a fast scroll decodes at most
    /// one original behind the selection.
    preview_in_flight: bool,
    /// Counts the decoded previews this screen has held, naming the one shown now.
    generation: u64,
    /// The latest terminal encoding: of which decoded preview, for what pane size.
    encoded: Option<(u64, Size, Result<EncodedPreview, String>)>,
    /// The latest encode asked for; an answer is kept only if it is for exactly this pair.
    requested: Option<(u64, Size)>,
}

impl Default for LibraryScreen {
    fn default() -> Self {
        Self {
            form: Form::new(vec![
                Field::text("Library"),
                Field::secret("Library passphrase"),
            ]),
            editing: false,
            invalid: None,
            library: None,
            catalogue: None,
            view: View::Timeline,
            cursors: [0; 3],
            group: None,
            preview: None,
            preview_in_flight: false,
            generation: 0,
            encoded: None,
            requested: None,
        }
    }
}

impl LibraryScreen {
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

    /// Why the form was last refused, if it was.
    #[must_use]
    pub fn invalid(&self) -> Option<&str> {
        self.invalid.as_deref()
    }

    /// The open library: `None` before the first, `Loading` while it opens.
    #[must_use]
    pub fn catalogue(&self) -> Option<&Loadable<Catalogue>> {
        self.catalogue.as_ref()
    }

    /// The view shown.
    #[must_use]
    pub fn view(&self) -> View {
        self.view
    }

    /// The album or stack being listed, if one is.
    #[must_use]
    pub fn group(&self) -> Option<Group> {
        self.group.map(|(group, _)| group)
    }

    /// The latest preview asked for and where it stands.
    #[must_use]
    pub fn preview(&self) -> Option<&(Uuid, Loadable<PreviewImage>)> {
        self.preview.as_ref()
    }

    fn ready(&self) -> Option<&Catalogue> {
        match &self.catalogue {
            Some(Loadable::Ready(catalogue)) => Some(catalogue),
            _ => None,
        }
    }

    /// The list shown now, borrowed from the catalogue: its length and any one row cost a
    /// lookup, not a pass over the library.
    fn row_source(&self) -> RowSource<'_> {
        let Some(catalogue) = self.ready() else {
            return RowSource::Assets(&[]);
        };
        match self.group {
            Some((Group::Album(album_id), _)) => {
                RowSource::Assets(catalogue.album_assets(album_id))
            }
            Some((Group::Stack(stack_id), _)) => RowSource::Members(
                catalogue
                    .stack(stack_id)
                    .map_or(&[], |stack| stack.members.as_slice()),
            ),
            None => match self.view {
                View::Timeline => RowSource::Assets(catalogue.timeline()),
                View::Albums => RowSource::Albums(catalogue.albums()),
                View::Stacks => RowSource::Stacks(catalogue.stacks()),
            },
        }
    }

    /// The rows the list shows now. Builds the whole list: drawing and moving use
    /// [`row_count`](Self::row_count) and [`selected`](Self::selected), which do not.
    #[must_use]
    pub fn rows(&self) -> Vec<Row> {
        let source = self.row_source();
        (0..source.len()).filter_map(|at| source.get(at)).collect()
    }

    /// How many rows the list shows now.
    #[must_use]
    pub fn row_count(&self) -> usize {
        self.row_source().len()
    }

    fn cursor_mut(&mut self) -> &mut usize {
        match &mut self.group {
            Some((_, cursor)) => cursor,
            None => &mut self.cursors[self.view.index()],
        }
    }

    /// The selected row's index into [`rows`](Self::rows).
    #[must_use]
    pub fn cursor(&self) -> usize {
        match self.group {
            Some((_, cursor)) => cursor,
            None => self.cursors[self.view.index()],
        }
    }

    /// The selected row, if the list has any.
    #[must_use]
    pub fn selected(&self) -> Option<Row> {
        let source = self.row_source();
        let last = source.len().checked_sub(1)?;
        source.get(self.cursor().min(last))
    }

    /// The asset the preview pane is about: the selected asset, or a selected stack's primary.
    #[must_use]
    pub fn selected_asset(&self) -> Option<Uuid> {
        match self.selected()? {
            Row::Asset(asset_id) => Some(asset_id),
            Row::Stack(stack_id) => self
                .ready()?
                .stack(stack_id)?
                .members
                .first()
                .map(|(id, _)| *id),
            Row::Album(_) => None,
        }
    }

    /// Apply `action`; pure, as [`super::state::AppState::update`] is.
    pub fn update(&mut self, action: LibraryAction) -> Vec<Effect> {
        match action {
            LibraryAction::Edit => {
                if self.catalogue != Some(Loadable::Loading) {
                    self.editing = true;
                }
                Vec::new()
            }
            LibraryAction::Opened(result) => {
                self.catalogue = Some(match result {
                    Ok(catalogue) => Loadable::Ready(catalogue),
                    Err(reason) => Loadable::Failed(reason),
                });
                self.view = View::Timeline;
                self.cursors = [0; 3];
                self.group = None;
                self.preview = None;
                self.encoded = None;
                self.requested = None;
                self.want_preview()
            }
            LibraryAction::PreviewLoaded { asset_id, result } => {
                self.preview_in_flight = false;
                if let Some((wanted, slot)) = &mut self.preview
                    && *wanted == asset_id
                {
                    *slot = match result {
                        Ok(image) => {
                            // A new image: any encoding held is of another.
                            self.generation = self.generation.wrapping_add(1);
                            self.encoded = None;
                            Loadable::Ready(image)
                        }
                        Err(reason) => Loadable::Failed(reason),
                    };
                }
                self.want_preview()
            }
            LibraryAction::EncodeRequested { generation, size } => {
                self.requested = Some((generation, size));
                Vec::new()
            }
            LibraryAction::PreviewEncoded {
                generation,
                size,
                result,
            } => {
                // Answers can land out of order: only the latest ask is kept. A stale one is
                // dropped, and the frame still wants (and has already asked for) the current pair.
                if generation == self.generation && self.requested == Some((generation, size)) {
                    self.encoded = Some((generation, size, result));
                } else {
                    tracing::debug!(target: "capsule_cli::tui", generation, ?size, requested = ?self.requested, "stale preview encode dropped");
                }
                Vec::new()
            }
            // Moving needs an open library; before one, these keys do nothing.
            _ if self.ready().is_none() => Vec::new(),
            LibraryAction::NextView => {
                self.view = self.view.next();
                self.group = None;
                self.want_preview()
            }
            LibraryAction::SelectNext => {
                let last = self.row_count().saturating_sub(1);
                let cursor = self.cursor_mut();
                *cursor = (*cursor + 1).min(last);
                self.want_preview()
            }
            LibraryAction::SelectPrevious => {
                let cursor = self.cursor_mut();
                *cursor = cursor.saturating_sub(1);
                self.want_preview()
            }
            LibraryAction::Enter => {
                let group = match self.selected() {
                    Some(Row::Album(album_id)) => Group::Album(album_id),
                    Some(Row::Stack(stack_id)) => Group::Stack(stack_id),
                    Some(Row::Asset(_)) | None => return Vec::new(),
                };
                self.group = Some((group, 0));
                self.want_preview()
            }
            LibraryAction::Leave => {
                if self.group.take().is_none() {
                    return Vec::new();
                }
                self.want_preview()
            }
        }
    }

    /// Ask for the selected asset's preview, unless it is already shown or asked for, or a
    /// decode is running (its answer asks again).
    fn want_preview(&mut self) -> Vec<Effect> {
        let Some(asset_id) = self.selected_asset() else {
            return Vec::new();
        };
        if self.preview.as_ref().is_some_and(|(id, _)| *id == asset_id) || self.preview_in_flight {
            return Vec::new();
        }
        let Some(asset) = self.ready().and_then(|c| c.asset(asset_id)) else {
            return Vec::new();
        };
        let Some(original) = asset.original.clone() else {
            self.preview = Some((
                asset_id,
                Loadable::Failed("the original is not on this device".to_owned()),
            ));
            return Vec::new();
        };
        let request = PreviewRequest {
            asset_id,
            original,
            ext: asset.ext.clone(),
        };
        self.preview = Some((asset_id, Loadable::Loading));
        self.preview_in_flight = true;
        vec![Effect::Library(LibraryEffect::Preview(request))]
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
            FormOutcome::Submitted => {
                let library = self.form.value(LIBRARY).trim().to_owned();
                let passphrase = self.form.secret(PASSPHRASE);
                if library.is_empty() || passphrase.is_empty() {
                    self.invalid = Some("Enter the library and its passphrase.".to_owned());
                    return Vec::new();
                }
                self.invalid = None;
                self.form.clear_secrets();
                self.editing = false;
                self.library = Some(PathBuf::from(&library));
                self.catalogue = Some(Loadable::Loading);
                vec![Effect::Library(LibraryEffect::Open(OpenRequest {
                    library: PathBuf::from(library),
                    passphrase,
                }))]
            }
        }
    }
}

/// Where the preview is drawn: whether the terminal has a graphics protocol, and the encode the
/// last frame found missing. Drawing never encodes: a frame that finds no encoding of the shown
/// preview for its pane draws text and leaves an [`EncodeRequest`] here, which the runtime
/// takes after the frame ([`take_requests`](Self::take_requests)) and runs as an effect, off the
/// UI loop. Its answer reaches the state as [`LibraryAction::PreviewEncoded`] and the next frame
/// draws it.
#[derive(Debug)]
pub struct PreviewCanvas {
    protocol: Option<ProtocolType>,
    /// The last encode asked for, so a frame drawn while it runs does not ask again.
    requested: Option<(u64, Size)>,
    /// An encode asked for by the last frame and not yet taken.
    pending: Option<EncodeRequest>,
}

impl PreviewCanvas {
    /// A canvas drawing through `picker`'s protocol, or text only with `None`.
    #[must_use]
    pub fn new(picker: Option<Picker>) -> Self {
        Self {
            protocol: picker.as_ref().map(Picker::protocol_type),
            requested: None,
            pending: None,
        }
    }

    /// A canvas that draws no images.
    #[must_use]
    pub fn text_only() -> Self {
        Self::new(None)
    }

    /// Whether this canvas draws images.
    #[must_use]
    pub fn draws_images(&self) -> bool {
        self.protocol.is_some()
    }

    /// The encode the last frame asked for, as the effect that runs it; at most one.
    pub fn take_requests(&mut self) -> Vec<Effect> {
        self.pending
            .take()
            .map(|request| Effect::Library(LibraryEffect::Encode(request)))
            .into_iter()
            .collect()
    }

    /// Draw the screen's decoded preview into `area` from its encoding for this pane: `Ok(true)`
    /// where it was drawn, `Ok(false)` where the encoding is still to come (asked for here once),
    /// and `Err` with why it cannot be drawn.
    fn draw(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        screen: &LibraryScreen,
        image: &PreviewImage,
    ) -> Result<bool, String> {
        if self.protocol.is_none() {
            return Err(NO_IMAGE_PROTOCOL.to_owned());
        }
        let generation = screen.generation;
        let size = Size::new(area.width, area.height);
        match &screen.encoded {
            Some((g, s, Ok(encoded))) if *g == generation && *s == size => {
                frame.render_widget(Image::new(&encoded.0), area);
                Ok(true)
            }
            Some((g, s, Err(reason))) if *g == generation && *s == size => Err(reason.clone()),
            _ => {
                if self.requested != Some((generation, size)) {
                    tracing::debug!(target: "capsule_cli::tui", generation, ?size, "preview encode requested");
                    self.requested = Some((generation, size));
                    self.pending = Some(EncodeRequest {
                        generation,
                        image: image.clone(),
                        size,
                    });
                }
                Ok(false)
            }
        }
    }
}

/// Ask the terminal which image protocol it speaks. Call it after the terminal is set up and
/// before input is read. `None` where it answered with none of Kitty, Sixel or iTerm2: half
/// blocks are not a preview worth the space, so the pane says so in text instead.
///
/// A terminal that never answers holds the TUI's first frame back by [`PICKER_QUERY_TIMEOUT`].
#[must_use]
pub fn query_picker() -> Option<Picker> {
    let options = QueryStdioOptions {
        timeout: PICKER_QUERY_TIMEOUT,
        ..QueryStdioOptions::default()
    };
    match Picker::from_query_stdio_with_options(options) {
        Ok(picker) if picker.protocol_type() != ProtocolType::Halfblocks => {
            let protocol = picker.protocol_type();
            tracing::info!(target: "capsule_cli::tui", ?protocol, "terminal draws inline images");
            Some(picker)
        }
        Ok(_) => {
            tracing::info!(target: "capsule_cli::tui", "terminal draws no inline images; previews are text");
            None
        }
        Err(e) => {
            tracing::warn!(target: "capsule_cli::tui", error = %e, "terminal image query failed; previews are text");
            None
        }
    }
}

/// Short form of an id: its last eight hex digits, since a UUIDv7 leads with its creation time
/// and ids minted together share their first eight.
fn short(id: Uuid) -> String {
    let simple = id.simple().to_string();
    simple[simple.len() - 8..].to_owned()
}

/// A capture instant as a list row prints it, in UTC.
fn when(asset: &AssetEntry) -> String {
    asset.captured.map_or_else(
        || "undated         ".to_owned(),
        |t| t.strftime("%Y-%m-%d %H:%M").to_string(),
    )
}

const fn role_name(role: StackRole) -> &'static str {
    match role {
        StackRole::Primary => "primary",
        StackRole::Member => "member",
        StackRole::Proxy => "proxy",
    }
}

fn labels_text(labels: &Result<Vec<String>, String>) -> String {
    match labels {
        Ok(labels) if labels.is_empty() => "(none)".to_owned(),
        Ok(labels) => labels.join(", "),
        Err(reason) => format!("(unreadable: {reason})"),
    }
}

fn row_item(catalogue: &Catalogue, row: Row) -> ListItem<'static> {
    let text = match row {
        Row::Asset(asset_id) => {
            let Some(asset) = catalogue.asset(asset_id) else {
                return ListItem::new(short(asset_id));
            };
            // The id is in the detail pane; the row keeps to what fits beside it.
            let mut text = format!("{}  {}", when(asset), asset.ext);
            if let Some((stack_id, StackRole::Primary)) = asset.stack {
                let count = catalogue.stack(stack_id).map_or(1, |s| s.members.len());
                let _ = write!(text, "  stack of {count}");
            }
            if asset.hidden {
                text.push_str("  hidden");
            }
            if asset.in_trash {
                text.push_str("  in trash");
            }
            text
        }
        Row::Album(album_id) => {
            let name = catalogue.album(album_id).map_or("", |a| a.name.as_str());
            format!("{name}  ({})", catalogue.album_assets(album_id).len())
        }
        Row::Stack(stack_id) => {
            let stack = catalogue.stack(stack_id);
            let primary = stack
                .and_then(|s| s.members.first())
                .and_then(|(id, _)| catalogue.asset(*id));
            format!(
                "{}  {} items  {}",
                primary.map_or_else(|| "undated         ".to_owned(), when),
                stack.map_or(0, |s| s.members.len()),
                short(stack_id)
            )
        }
    };
    ListItem::new(text)
}

/// The list items for the rows a `height`-row viewport shows ([`visible_window`]), and the
/// selection's index among them. Only those rows are built, so a frame costs the viewport, not
/// the library.
fn list_window(
    screen: &LibraryScreen,
    catalogue: &Catalogue,
    height: usize,
) -> (Vec<ListItem<'static>>, Option<usize>) {
    let source = screen.row_source();
    let window = visible_window(screen.cursor(), source.len(), height);
    let selected = (!window.is_empty()).then(|| screen.cursor().min(window.end - 1) - window.start);
    let items = window
        .filter_map(|at| source.get(at))
        .map(|row| row_item(catalogue, row))
        .collect();
    (items, selected)
}

/// The detail pane's lines for an asset:the `capsule show` rows, its album's and stack's
/// labels, and its provenance history.
fn asset_lines(catalogue: &Catalogue, asset: &AssetEntry) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(format!("Asset {}", asset.asset_id).bold())];
    let row = |label: &str, value: String| Line::from(format!("{label:<17}{value}"));
    for (label, value) in &asset.rows {
        lines.push(row(label, value.clone()));
    }
    let album = catalogue.album(asset.album_id);
    lines.push(row(
        "Album name:",
        album.map_or_else(|| "(no key in this library)".to_owned(), |a| a.name.clone()),
    ));
    lines.push(row(
        "Album labels:",
        album.map_or_else(|| "(none)".to_owned(), |a| labels_text(&a.labels)),
    ));
    if let Some((stack_id, _)) = asset.stack {
        lines.push(row(
            "Stack labels:",
            catalogue
                .stack(stack_id)
                .map_or_else(|| "(none)".to_owned(), |s| labels_text(&s.labels)),
        ));
    }
    lines.push(Line::default());
    lines.push(Line::from("History".bold()));
    for record in &asset.history {
        let mut text = format!(
            "{}  {:<18} device {}  {}",
            record.timestamp,
            record.action,
            short(record.device),
            record.client_version
        );
        if let Some(until) = &record.retention_until {
            let _ = write!(text, "  until {until}");
        }
        lines.push(Line::from(text));
    }
    lines
}

fn album_lines(catalogue: &Catalogue, album: &AlbumEntry) -> Vec<Line<'static>> {
    vec![
        Line::from(format!("Album {}", album.name).bold()),
        Line::from(format!("{:<17}{}", "Id:", album.album_id)),
        Line::from(format!("{:<17}{}", "Labels:", labels_text(&album.labels))),
        Line::from(format!(
            "{:<17}{}",
            "Assets:",
            catalogue.album_assets(album.album_id).len()
        )),
        Line::default(),
        Line::from("Enter lists its assets.".fg(Color::DarkGray)),
    ]
}

fn stack_lines(stack: &StackEntry) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(format!("Stack {}", stack.stack_id).bold()),
        Line::from(format!("{:<17}{}", "Labels:", labels_text(&stack.labels))),
        Line::from(format!("{:<17}{}", "Members:", stack.members.len())),
    ];
    for (asset_id, role) in &stack.members {
        lines.push(Line::from(format!("  {asset_id} ({})", role_name(*role))));
    }
    lines.push(Line::default());
    lines.push(Line::from("Enter lists its members.".fg(Color::DarkGray)));
    lines
}

/// Draw the browser into `area`.
pub fn render(
    frame: &mut Frame,
    area: Rect,
    screen: &LibraryScreen,
    title: &'static str,
    canvas: &mut PreviewCanvas,
) {
    match (&screen.catalogue, screen.editing) {
        (Some(Loadable::Ready(catalogue)), false) => {
            render_browser(frame, area, screen, catalogue, title, canvas);
        }
        _ => render_open(frame, area, screen, title),
    }
}

/// The open form, and where the last open stands.
fn render_open(frame: &mut Frame, area: Rect, screen: &LibraryScreen, title: &'static str) {
    let mut lines = vec![
        Line::from(
            "Browse a local library: its timeline, albums and stacks, each asset's metadata, \
             labels and provenance history, and a preview where the terminal draws images.",
        ),
        Line::default(),
    ];
    lines.extend(screen.form.lines(screen.editing));
    if let Some(reason) = &screen.invalid {
        lines.push(Line::from(reason.clone().fg(Color::Red)));
    }
    lines.push(Line::default());
    match &screen.catalogue {
        None | Some(Loadable::Ready(_)) => {}
        Some(Loadable::Loading) => lines.push(Line::from(
            "Opening the library… it is unlocked and read once.".fg(Color::DarkGray),
        )),
        Some(Loadable::Failed(reason)) => {
            lines.push(Line::from("The library did not open.".fg(Color::Red)));
            lines.push(Line::from(reason.clone()));
            lines.push(Line::default());
        }
    }
    lines.push(Line::from(
        "Scriptable: `capsule show ASSET --library PATH` prints one asset's record."
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

fn render_browser(
    frame: &mut Frame,
    area: Rect,
    screen: &LibraryScreen,
    catalogue: &Catalogue,
    title: &'static str,
    canvas: &mut PreviewCanvas,
) {
    let library = screen
        .library
        .as_ref()
        .map_or_else(String::new, |p| format!(" · {}", p.display()));
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("{title}{library}"));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [tabs, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(inner);
    let mut spans: Vec<Span> = Vec::new();
    for view in View::ALL {
        if !spans.is_empty() {
            spans.push("  ".into());
        }
        if view == screen.view {
            spans.push(Span::styled(
                view.name(),
                Style::new().add_modifier(Modifier::BOLD | Modifier::REVERSED),
            ));
        } else {
            spans.push(view.name().fg(Color::DarkGray));
        }
    }
    match screen.group {
        Some((Group::Album(album_id), _)) => {
            let name = catalogue.album(album_id).map_or("", |a| a.name.as_str());
            spans.push(format!("  › album {name}").into());
        }
        Some((Group::Stack(stack_id), _)) => {
            spans.push(format!("  › stack {}", short(stack_id)).into());
        }
        None => {}
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), tabs);

    let [list_area, side] =
        Layout::horizontal([Constraint::Percentage(40), Constraint::Percentage(60)]).areas(body);
    let (items, selected) = list_window(screen, catalogue, usize::from(list_area.height));
    if screen.row_count() == 0 {
        let empty = match (screen.group, screen.view) {
            (Some(_), _) => "Nothing here.",
            (None, View::Timeline) => {
                "No assets to show. Import some from Home with `i`, or `capsule import`."
            }
            (None, View::Albums) => "This library holds no albums.",
            (None, View::Stacks) => "No asset in this library is stacked.",
        };
        frame.render_widget(
            Paragraph::new(empty.fg(Color::DarkGray)).wrap(Wrap { trim: false }),
            list_area,
        );
    } else {
        let list = List::new(items)
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("› ");
        let mut state = ListState::default().with_selected(selected);
        frame.render_stateful_widget(list, list_area, &mut state);
    }

    let detail = match screen.selected() {
        Some(Row::Asset(asset_id)) => catalogue
            .asset(asset_id)
            .map(|asset| asset_lines(catalogue, asset)),
        Some(Row::Album(album_id)) => catalogue
            .album(album_id)
            .map(|album| album_lines(catalogue, album)),
        Some(Row::Stack(stack_id)) => catalogue.stack(stack_id).map(stack_lines),
        None => None,
    }
    .unwrap_or_default();

    let detail_area = match screen.selected_asset() {
        Some(asset_id) => {
            // A text preview is a line or two: the rest of the side goes to the detail.
            let preview_height = if canvas.draws_images() {
                Constraint::Percentage(50)
            } else {
                Constraint::Length(4)
            };
            let [preview_area, detail_area] =
                Layout::vertical([preview_height, Constraint::Min(0)]).areas(side);
            render_preview(frame, preview_area, screen, asset_id, canvas);
            detail_area
        }
        None => side,
    };
    frame.render_widget(
        Paragraph::new(detail).wrap(Wrap { trim: false }),
        detail_area,
    );
}

fn render_preview(
    frame: &mut Frame,
    area: Rect,
    screen: &LibraryScreen,
    asset_id: Uuid,
    canvas: &mut PreviewCanvas,
) {
    let block = Block::default().borders(Borders::ALL).title("Preview");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let text = match &screen.preview {
        Some((id, Loadable::Ready(image))) if *id == asset_id => {
            match canvas.draw(frame, inner, screen, image) {
                Ok(true) => return,
                Ok(false) => "Preparing the preview for this terminal…".to_owned(),
                Err(reason) => format!(
                    "No preview: {reason}. {}×{} decoded.",
                    image.width, image.height
                ),
            }
        }
        Some((id, Loadable::Failed(reason))) if *id == asset_id => {
            format!("No preview: {reason}.")
        }
        _ => "Loading the preview…".to_owned(),
    };
    frame.render_widget(
        Paragraph::new(text.fg(Color::DarkGray)).wrap(Wrap { trim: false }),
        inner,
    );
}

#[cfg(test)]
mod tests;
