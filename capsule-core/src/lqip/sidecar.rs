//! The bridge between an [`Lqip`] payload and the signed sidecar's `lqip` record.
//!
//! Gated on `native` for one reason only: [`crate::sidecar`] is itself `native`-gated. The
//! *encoder* in the parent module is unconditional and is what `capsule-wasm` links, so the two
//! surfaces never diverge — this file only assembles and reads the three fields the sidecar
//! carries.
//!
//! `lqip` is a **signed** field: the sidecar signature covers it, so its encoding is
//! signature-visible rather than a private detail. See [`LQIP_FORMAT_V1`] for why moving from
//! ThumbHash to chromahash keeps version `1` and does not move `sidecar_schema`.

use super::{LQIP_FORMAT_V1, Lqip, RgbaImage, render};
use crate::sidecar::sidecar_v1::Lqip as SidecarLqip;

impl Lqip {
    /// Build the encrypted-sidecar record: the chromahash payload, the [`LQIP_FORMAT_V1`] tag,
    /// and the `dominant_color` fill a reader paints when it cannot decode the payload.
    ///
    /// Infallible — `average_color` is a DC read that cannot fail, and the payload is this
    /// type's invariant.
    pub fn to_sidecar(&self) -> SidecarLqip {
        SidecarLqip {
            chromahash: self.as_bytes().to_vec(),
            format_version: LQIP_FORMAT_V1,
            dominant_color: self.dominant_color(),
        }
    }
}

/// Render a stored sidecar record to a displayable placeholder, band-limited to the box being
/// painted.
///
/// Delegates to [`render`], so an unrecognized `format_version` or an undecodable payload
/// yields the 1x1 solid `dominant_color` fill rather than a misrender.
pub fn render_sidecar_lqip(lqip: &SidecarLqip, max_width: u32, max_height: u32) -> RgbaImage {
    render(
        lqip.format_version,
        &lqip.chromahash,
        lqip.dominant_color,
        max_width,
        max_height,
    )
}

#[cfg(test)]
mod tests;
