//! The render ladder across the FFI boundary: the flattened mirror of
//! [`crate::fetch::render_step`] and the embedded-preview read it is fed with.
//!
//! # Why free functions
//!
//! [`render_step`] is pure: it answers from the request, what the surface shows and what the
//! device holds, all of which the caller passes. The surface keeps the [`FfiRendition`] it shows
//! and hands it back on the next call, which is what lets a view upgrade in place without this
//! side holding a per-view object. [`embedded_preview`] reads one file and holds nothing either.
//! Neither touches a workspace, so neither is a method of
//! [`FfiWorkspace`](crate::ffi::FfiWorkspace).
//!
//! # An app keeps no ladder
//!
//! The apps render the rung [`render_step`] names and obtain the rung its `next` names. The
//! order, the stand-in rule for the embedded preview and the never-downgrade rule live only in
//! [`crate::fetch::ladder`](crate::fetch::render_step).

use std::collections::BTreeSet;
use std::path::Path;

use capsule_core::media::{EmbeddedPreview, extract_embedded_preview};

use super::FfiError;
use crate::fetch::{
    LocalHoldings, NextStep, PixelSize, RenderRequest, RenderStep, Rendition, Representation,
    RungSource,
};

/// A rung of the ladder, cheapest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRepresentation {
    /// The LQIP, embedded in the metadata blob; always present.
    Lqip,
    /// The grid thumbnail.
    Thumbnail,
    /// The single-asset view's preview.
    Preview,
    /// The original.
    Original,
}

impl From<FfiRepresentation> for Representation {
    fn from(rung: FfiRepresentation) -> Self {
        match rung {
            FfiRepresentation::Lqip => Self::Lqip,
            FfiRepresentation::Thumbnail => Self::Thumbnail,
            FfiRepresentation::Preview => Self::Preview,
            FfiRepresentation::Original => Self::Original,
        }
    }
}

impl From<Representation> for FfiRepresentation {
    fn from(rung: Representation) -> Self {
        match rung {
            Representation::Lqip => Self::Lqip,
            Representation::Thumbnail => Self::Thumbnail,
            Representation::Preview => Self::Preview,
            Representation::Original => Self::Original,
        }
    }
}

/// Where a shown rung's pixels come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRungSource {
    /// The LQIP (`render_lqip`), which paints the dominant colour where it does not decode.
    Lqip,
    /// The original's embedded preview ([`embedded_preview`]).
    Embedded,
    /// The rung's encoded tier.
    Encoded,
    /// The original, decoded.
    Original,
}

impl From<FfiRungSource> for RungSource {
    fn from(source: FfiRungSource) -> Self {
        match source {
            FfiRungSource::Lqip => Self::Lqip,
            FfiRungSource::Embedded => Self::Embedded,
            FfiRungSource::Encoded => Self::Encoded,
            FfiRungSource::Original => Self::Original,
        }
    }
}

impl From<RungSource> for FfiRungSource {
    fn from(source: RungSource) -> Self {
        match source {
            RungSource::Lqip => Self::Lqip,
            RungSource::Embedded => Self::Embedded,
            RungSource::Encoded => Self::Encoded,
            RungSource::Original => Self::Original,
        }
    }
}

/// What a surface shows: a rung and its source. The caller keeps the last one and passes it
/// back as `shown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FfiRendition {
    /// The rung shown.
    pub rung: FfiRepresentation,
    /// What fills it.
    pub source: FfiRungSource,
}

impl From<FfiRendition> for Rendition {
    fn from(shown: FfiRendition) -> Self {
        Self {
            rung: shown.rung.into(),
            source: shown.source.into(),
        }
    }
}

impl From<Rendition> for FfiRendition {
    fn from(shown: Rendition) -> Self {
        Self {
            rung: shown.rung.into(),
            source: shown.source.into(),
        }
    }
}

/// A surface's request: its intent and its size in device pixels when the view opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FfiRenderRequest {
    /// The rung the surface's intent needs: `Thumbnail` for the grid, `Preview` for the
    /// single-asset view, `Original` for a full-fidelity view.
    pub intent: FfiRepresentation,
    /// The surface's width when the view opened. A view that grows keeps passing this.
    pub surface_width: u32,
    /// The surface's height when the view opened.
    pub surface_height: u32,
}

/// What this device holds for the asset. **Flat**, like `FfiNotifyInput`: an embedded preview
/// is present when both of its dimensions are.
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct FfiLocalHoldings {
    /// The encoded tiers and the original held locally; the LQIP is implied.
    pub held: Vec<FfiRepresentation>,
    /// The embedded preview's upright width, from [`embedded_preview`].
    pub embedded_preview_width: Option<u32>,
    /// The embedded preview's upright height.
    pub embedded_preview_height: Option<u32>,
}

/// How to obtain the next rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiNextStep {
    /// Fetch it from the server.
    Fetch {
        /// The representation to fetch.
        representation: FfiRepresentation,
    },
    /// Produce it on the device from the original it holds.
    Generate {
        /// The rung to produce.
        representation: FfiRepresentation,
    },
}

impl From<NextStep> for FfiNextStep {
    fn from(next: NextStep) -> Self {
        match next {
            NextStep::Fetch(rung) => Self::Fetch {
                representation: rung.into(),
            },
            NextStep::Generate(rung) => Self::Generate {
                representation: rung.into(),
            },
        }
    }
}

/// The answer to one [`render_step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FfiRenderStep {
    /// What to show now.
    pub show: FfiRendition,
    /// Whether `show` differs from the `shown` passed in.
    pub changed: bool,
    /// What to obtain next, or `None` when the intent is met or nothing can raise it.
    pub next: Option<FfiNextStep>,
}

impl From<RenderStep> for FfiRenderStep {
    fn from(step: RenderStep) -> Self {
        Self {
            show: step.show.into(),
            changed: step.changed,
            next: step.next.map(Into::into),
        }
    }
}

/// A RAW original's embedded JPEG, as [`embedded_preview`] reads it.
#[derive(Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiEmbeddedPreview {
    /// The JPEG exactly as the RAW file carries it.
    pub jpeg: Vec<u8>,
    /// Upright width in pixels, once `orientation` is applied.
    pub width: u32,
    /// Upright height in pixels.
    pub height: u32,
    /// The EXIF orientation (1..=8) to apply to `jpeg`'s stored pixels, in place of any tag the
    /// JPEG itself carries.
    pub orientation: u16,
}

impl std::fmt::Debug for FfiEmbeddedPreview {
    // The JPEG never reaches the log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FfiEmbeddedPreview")
            .field("bytes", &self.jpeg.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .field("orientation", &self.orientation)
            .finish()
    }
}

impl From<EmbeddedPreview> for FfiEmbeddedPreview {
    fn from(preview: EmbeddedPreview) -> Self {
        Self {
            jpeg: preview.jpeg,
            width: preview.width,
            height: preview.height,
            orientation: preview.orientation,
        }
    }
}

/// One step of the render ladder ([`crate::fetch::render_step`]): what to show now and what to
/// obtain next. `shown` is what the surface shows, `None` when it shows nothing yet or is falling
/// back below what it showed; `fetchable` is what the server lists for the asset.
#[uniffi::export]
#[must_use]
pub fn render_step(
    request: FfiRenderRequest,
    shown: Option<FfiRendition>,
    local: FfiLocalHoldings,
    fetchable: Vec<FfiRepresentation>,
) -> FfiRenderStep {
    let request = RenderRequest {
        intent: request.intent.into(),
        surface: PixelSize {
            width: request.surface_width,
            height: request.surface_height,
        },
    };
    let local = LocalHoldings {
        held: local.held.into_iter().map(Into::into).collect(),
        embedded_preview: local
            .embedded_preview_width
            .zip(local.embedded_preview_height)
            .map(|(width, height)| PixelSize { width, height }),
    };
    let fetchable: BTreeSet<Representation> = fetchable.into_iter().map(Into::into).collect();
    crate::fetch::render_step(&request, shown.map(Into::into), &local, &fetchable).into()
}

/// Read the embedded preview of the original at `original_path`, whose extension (without the
/// dot) is `ext`.
///
/// `None` when the original is not a RAW, carries no preview, or carries one that does not read
/// within the decode budget: each leaves the rung to the next source, so none is an error. The
/// reason is logged.
///
/// This reads the whole original into memory, because the extraction's IFD bounds walk and its
/// stated-length guard run over the full file before the RAW reader does. Call it once per asset
/// and keep the [`FfiEmbeddedPreview`] it returns, rather than once per render.
///
/// # Errors
///
/// [`FfiError::Workspace`] when the file cannot be read.
#[uniffi::export]
#[tracing::instrument(level = "debug", skip(original_path))]
pub fn embedded_preview(
    original_path: String,
    ext: String,
) -> Result<Option<FfiEmbeddedPreview>, FfiError> {
    let path = Path::new(&original_path);
    let bytes = std::fs::read(path).map_err(|e| FfiError::Workspace {
        detail: format!("reading the original {}: {e}", path.display()),
    })?;
    Ok(embedded_preview_of(&bytes, &ext))
}

/// [`embedded_preview`] over bytes already read.
fn embedded_preview_of(bytes: &[u8], ext: &str) -> Option<FfiEmbeddedPreview> {
    match extract_embedded_preview(bytes, ext) {
        Ok(preview) => preview.map(Into::into),
        Err(error) => {
            tracing::info!(%error, ext, "ffi: no embedded preview; the rung falls to the next source");
            None
        }
    }
}

#[cfg(test)]
mod tests;
