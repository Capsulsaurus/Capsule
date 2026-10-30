//! The **render ladder**: which representation a surface shows now, and what to obtain next
//! (SSoT: [Download — Render Ladder]).
//!
//! The order is computed here, once, and every native app reaches it through the FFI
//! (`capsule_sdk::ffi::render_step`); an app renders the [`Rendition`] it names and keeps no ladder
//! of its own. It extends the [`Representation`] degrade ladder with what is already on the
//! device: a RAW original's [embedded preview], which stands in for a rung until that rung's
//! encoded tier is held.
//!
//! # One pure function
//!
//! [`render_step`] takes the surface's request, what it already shows, what the device holds and
//! what the server lists, and answers what to show and what to obtain next. It holds no state:
//! the surface keeps the [`Rendition`] it shows and passes it back on the next call, which is how
//! a view **upgrades in place** as a better rung arrives and never downgrades one it has shown.
//! Falling back below what is shown (a held rung that was evicted or would not open — the
//! [fallback ladder]) is the same call with nothing shown.
//!
//! # Where the halves live
//!
//! The local half is a value: [`LocalHoldings`] says which encoded tiers and whether the original
//! are held, and the embedded preview's size, which the caller reads from the original through
//! `capsule_core::media::extract_embedded_preview`. The fetched half is the set of
//! representations the server lists ([`AssetBlobs::blob_for`](super::AssetBlobs::blob_for)).
//! Neither needs `capsule-library` state, so the ladder sits beside [`Representation`] rather than
//! in the library.
//!
//! [Download — Render Ladder]: https://docs/design/transfer/download/#render-ladder
//! [embedded preview]: https://docs/design/thumbnails/#embedded-previews
//! [fallback ladder]: https://docs/design/transfer/download/#fallback-ladder

use std::cmp::Ordering;
use std::collections::BTreeSet;

use capsule_core::media::DerivativeTier;

use super::Representation;

/// A size in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PixelSize {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl PixelSize {
    /// The longer of the two edges.
    #[must_use]
    pub const fn long_edge(self) -> u32 {
        if self.width >= self.height {
            self.width
        } else {
            self.height
        }
    }

    /// Whether a frame of this size covers `surface`: fitting it inside the surface, aspect
    /// preserved, needs no upscaling. That is the case exactly when it reaches the surface's
    /// width or its height, because the fit scale is the smaller of the two ratios. An empty
    /// surface is covered by anything.
    #[must_use]
    pub const fn covers(self, surface: Self) -> bool {
        self.width >= surface.width || self.height >= surface.height
    }
}

/// Where the pixels of a shown rung come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RungSource {
    /// The LQIP from the metadata blob, or its dominant colour where it does not decode.
    Lqip,
    /// A RAW original's embedded preview, standing in for the rung's encoded tier.
    Embedded,
    /// The rung's encoded tier (thumbnail or preview derivative).
    Encoded,
    /// The original, decoded.
    Original,
}

/// What a surface shows: a rung, and the source that fills it.
///
/// Ordered by fidelity: by rung, and within a rung the encoded tier above the embedded preview
/// that stands in for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rendition {
    /// The rung shown.
    pub rung: Representation,
    /// What fills it.
    pub source: RungSource,
}

impl Rendition {
    /// The bottom of the ladder, always present.
    pub const LQIP: Self = Self {
        rung: Representation::Lqip,
        source: RungSource::Lqip,
    };
}

impl Ord for Rendition {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.rung, self.source).cmp(&(other.rung, other.source))
    }
}

impl PartialOrd for Rendition {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// What a surface asks the ladder for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderRequest {
    /// The rung the surface's intent needs: the grid asks for
    /// [`Thumbnail`](Representation::Thumbnail), the single-asset view for
    /// [`Preview`](Representation::Preview), a full-fidelity view for
    /// [`Original`](Representation::Original).
    pub intent: Representation,
    /// The surface's size in device pixels **when the view opened**. An embedded preview fills
    /// Preview only if it covers this; a view that later grows keeps passing the size it opened
    /// at, so it keeps the embedded preview rather than dropping a rung.
    pub surface: PixelSize,
}

/// What this device holds for one asset.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocalHoldings {
    /// The representations held locally: the encoded [`Thumbnail`](Representation::Thumbnail)
    /// and [`Preview`](Representation::Preview) tiers and the
    /// [`Original`](Representation::Original). The LQIP is always held and need not be listed.
    pub held: BTreeSet<Representation>,
    /// The size of the original's embedded preview, upright, where this device holds a RAW
    /// original that carries one which extracted.
    pub embedded_preview: Option<PixelSize>,
}

/// How to obtain the next rung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NextStep {
    /// Fetch this representation from the server ([`super::open_representation`]).
    Fetch(Representation),
    /// Produce this rung on the device from the original it holds.
    Generate(Representation),
}

/// The answer to one [`render_step`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderStep {
    /// What to show now.
    pub show: Rendition,
    /// Whether `show` differs from what the surface already showed: a better rung arrived, or
    /// nothing was shown yet.
    pub changed: bool,
    /// What to obtain next to reach the intent, or `None` when the intent is met or nothing can
    /// raise it.
    pub next: Option<NextStep>,
}

/// The long edge an embedded preview must reach to fill the Thumbnail rung: the Thumbnail
/// tier's own.
#[must_use]
pub fn thumbnail_long_edge() -> u32 {
    DerivativeTier::Thumbnail
        .max_long_edge()
        .unwrap_or(u32::MAX)
}

/// The best rendition held at or below `request.intent`.
fn best_held(request: &RenderRequest, local: &LocalHoldings) -> Rendition {
    let intent = request.intent;
    let encoded = |rung: Representation| {
        (rung <= intent && local.held.contains(&rung)).then_some(Rendition {
            rung,
            source: if rung == Representation::Original {
                RungSource::Original
            } else {
                RungSource::Encoded
            },
        })
    };
    let embedded = local.embedded_preview.and_then(|size| {
        let preview = Representation::Preview;
        let thumbnail = Representation::Thumbnail;
        let rung = if preview <= intent && size.covers(request.surface) {
            preview
        } else if thumbnail <= intent && size.long_edge() >= thumbnail_long_edge() {
            thumbnail
        } else {
            return None;
        };
        Some(Rendition {
            rung,
            source: RungSource::Embedded,
        })
    });
    [
        encoded(Representation::Thumbnail),
        encoded(Representation::Preview),
        encoded(Representation::Original),
        embedded,
    ]
    .into_iter()
    .flatten()
    .fold(Rendition::LQIP, Ord::max)
}

/// What to obtain next so `show` reaches the intent.
///
/// - The intent's encoded tier (or, for Original, the original) is shown, or a rung above the
///   intent is shown from any source: nothing.
/// - The intent rung is filled by the embedded preview: fetch the encoded tier where the server
///   lists it, since it replaces the camera's rendering; never generate it here, which is the
///   import's work.
/// - Otherwise fetch it where the server lists it, else generate it from a held original. The
///   fetch is preferred: an encoded tier is small, and generating one can mean developing a RAW.
///   The original itself is only ever fetched, and only when it is the intent.
fn next_step(
    request: &RenderRequest,
    show: Rendition,
    local: &LocalHoldings,
    fetchable: &BTreeSet<Representation>,
) -> Option<NextStep> {
    let intent = request.intent;
    if intent == Representation::Lqip {
        return None;
    }
    // A rung above the intent (a surface passing back what a larger view showed) already
    // outranks the intent's encoded tier, whatever fills it: obtaining that tier could never
    // change what is shown, so only an embedded preview exactly at the intent asks for it.
    if show.rung > intent {
        return None;
    }
    let filled = show.rung == intent;
    if filled && show.source != RungSource::Embedded {
        return None;
    }
    if fetchable.contains(&intent) {
        return Some(NextStep::Fetch(intent));
    }
    let generatable = intent != Representation::Original
        && local.held.contains(&Representation::Original)
        && !filled;
    generatable.then_some(NextStep::Generate(intent))
}

/// One step of the render ladder: what to show, and what to obtain next.
///
/// `shown` is what the surface shows now, `None` when it shows nothing yet or is falling back.
/// The answer never shows less than `shown`: a rung below it is kept off screen until the
/// surface itself falls back, by calling with `None`.
///
/// `fetchable` is the representations the server lists for the asset
/// ([`AssetBlobs::blob_for`](super::AssetBlobs::blob_for) returns `Some`) and this device can
/// still fetch.
#[must_use]
#[tracing::instrument(level = "trace", skip(local, fetchable), ret)]
pub fn render_step(
    request: &RenderRequest,
    shown: Option<Rendition>,
    local: &LocalHoldings,
    fetchable: &BTreeSet<Representation>,
) -> RenderStep {
    let best = best_held(request, local);
    let show = shown.map_or(best, |shown| shown.max(best));
    let next = next_step(request, show, local, fetchable);
    RenderStep {
        show,
        changed: shown != Some(show),
        next,
    }
}

#[cfg(test)]
mod tests;
