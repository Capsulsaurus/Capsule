//! The `media`-side adapter onto [`crate::lqip`] — nothing more.
//!
//! The LQIP scheme itself moved out of this stack in slice `S-B14`: it is reachable from the
//! import pipeline, from the apps through the uniffi FFI, and from the browser through
//! `capsule-wasm`, so it cannot retire to `legacy-review/` with `capsule_core::media`. What is
//! left here is the one thing that genuinely belongs to the retiring stack — converting an
//! [`ImageBuffer`] (and its [`ColorSpace`]) into the raw RGBA frame and [`Gamut`] the encoder
//! takes. When Rawshift's replacement lands, this file goes and its caller talks to
//! [`crate::lqip`] directly.

use thiserror::Error;

pub use crate::lqip::LQIP_FORMAT_V1;
use crate::lqip::{Gamut, Lqip, LqipError};
use crate::media::image::buffer::{ComponentType, ImageBuffer, PixelFormat};
use crate::media::metadata::ColorSpace;
use crate::sidecar::sidecar_v1::Lqip as SidecarLqip;

/// Map the media stack's [`ColorSpace`] onto the chromahash source [`Gamut`].
///
/// This is the only place a colour space becomes a gamut, and it is the last point at which the
/// choice can be made: the sidecar stores no gamut and chromahash does not carry one in the
/// payload, so whatever is chosen here is baked into the signed bytes and is not recoverable
/// afterwards. Every mapping is therefore either an exact counterpart or a documented
/// conservative default — never a guess:
///
/// - [`ColorSpace::Srgb`], [`ColorSpace::AdobeRgb`], [`ColorSpace::DisplayP3`] and
///   [`ColorSpace::ProPhoto`] have exact chromahash counterparts.
/// - [`ColorSpace::Linear`] names a *transfer function*, not a set of primaries, so it carries
///   no gamut information at all. It maps to [`Gamut::Srgb`] — the sRGB/Rec.709 primaries every
///   untagged source is assumed to use. That is deliberately the conservative choice: treating
///   an unknown-primaries frame as wide-gamut would oversaturate the placeholder, whereas the
///   reverse merely under-saturates one that was wide.
/// - [`Gamut::Bt2020`] has no `ColorSpace` counterpart today, so nothing in this pipeline
///   selects it. It exists in [`Gamut`] because chromahash defines it and HDR sources will need
///   it once the metadata stack can express them.
pub fn gamut_for(color_space: ColorSpace) -> Gamut {
    match color_space {
        ColorSpace::Srgb | ColorSpace::Linear => Gamut::Srgb,
        ColorSpace::AdobeRgb => Gamut::AdobeRgb,
        ColorSpace::DisplayP3 => Gamut::DisplayP3,
        ColorSpace::ProPhoto => Gamut::ProPhotoRgb,
    }
}

/// Why an [`ImageBuffer`] could not be turned into an LQIP.
#[derive(Debug, Error)]
pub enum LQIPError {
    /// The buffer is not RGBA8. Callers hold `ImageBuffer::to_rgba8` for this.
    #[error("LQIP requires a U8 RGBA buffer (got {format:?}/{component_type:?})")]
    UnsupportedFormat {
        /// The pixel format the buffer actually carried.
        format: PixelFormat,
        /// The component type the buffer actually carried.
        component_type: ComponentType,
    },
    /// The buffer's dimensions do not fit a `u32`, so they cannot describe a frame.
    #[error("LQIP source dimensions {width}x{height} exceed u32")]
    DimensionsTooLarge {
        /// The buffer width.
        width: usize,
        /// The buffer height.
        height: usize,
    },
    /// The encoder rejected the frame.
    #[error(transparent)]
    Encode(#[from] LqipError),
}

/// An LQIP produced from an [`ImageBuffer`].
///
/// A thin newtype over [`crate::lqip::Lqip`], kept because the signed import executor
/// (`lifecycle::derivatives`) calls exactly this shape. Reach for [`crate::lqip`] directly in
/// anything new — this type retires with the surrounding stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LQIP(Lqip);

impl LQIP {
    /// Encode an RGBA8 [`ImageBuffer`] at the committed default tier, interpreting its pixels in
    /// the gamut its [`ColorSpace`] maps to.
    ///
    /// The buffer is hashed at **full resolution**. The retired ThumbHash implementation
    /// downsized to a 100 px long edge first; chromahash band-limits on the read side instead
    /// (`decode_capped`), so pre-resizing here would only throw fidelity away.
    pub fn from_rgba_buffer(buffer: &ImageBuffer) -> Result<Self, LQIPError> {
        if buffer.format != PixelFormat::Rgba || buffer.component_type != ComponentType::U8 {
            return Err(LQIPError::UnsupportedFormat {
                format: buffer.format,
                component_type: buffer.component_type,
            });
        }
        let (Ok(width), Ok(height)) = (u32::try_from(buffer.width), u32::try_from(buffer.height))
        else {
            return Err(LQIPError::DimensionsTooLarge {
                width: buffer.width,
                height: buffer.height,
            });
        };
        Ok(Self(Lqip::encode(
            width,
            height,
            &buffer.data,
            gamut_for(buffer.color_space),
        )?))
    }

    /// The encoded payload itself.
    pub fn hash(&self) -> &Lqip {
        &self.0
    }

    /// The raw chromahash bytes.
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// Build the encrypted-sidecar record.
    ///
    /// Fallible only for source compatibility with the import executor that calls it; the
    /// conversion itself cannot fail (see [`crate::lqip::Lqip::to_sidecar`]).
    pub fn to_sidecar(&self) -> Result<SidecarLqip, LQIPError> {
        Ok(self.0.to_sidecar())
    }
}

#[cfg(test)]
mod tests {
    use super::{ColorSpace, ComponentType, ImageBuffer, LQIP, LQIPError, PixelFormat, gamut_for};
    use crate::lqip::{Gamut, LQIP_FORMAT_V1, Lqip};

    fn buffer(width: usize, height: usize, color_space: ColorSpace) -> ImageBuffer {
        let mut data = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                data.extend_from_slice(&[
                    (x * 255 / width) as u8,
                    (y * 255 / height) as u8,
                    ((x + y) * 255 / (width + height)) as u8,
                    255,
                ]);
            }
        }
        ImageBuffer::new(
            data,
            width,
            height,
            PixelFormat::Rgba,
            ComponentType::U8,
            color_space,
        )
        .expect("well-formed RGBA8 buffer")
    }

    /// Every `ColorSpace` maps to exactly one `Gamut`. Exhaustive, so a new colour space cannot
    /// be added without deciding what it means here.
    #[test]
    fn gamut_for_maps_every_color_space() {
        assert_eq!(gamut_for(ColorSpace::Srgb), Gamut::Srgb);
        assert_eq!(gamut_for(ColorSpace::AdobeRgb), Gamut::AdobeRgb);
        assert_eq!(gamut_for(ColorSpace::DisplayP3), Gamut::DisplayP3);
        assert_eq!(gamut_for(ColorSpace::ProPhoto), Gamut::ProPhotoRgb);
        // `Linear` is a transfer function, not a set of primaries: sRGB is the conservative
        // default, and the choice is permanent once encoded.
        assert_eq!(gamut_for(ColorSpace::Linear), Gamut::Srgb);
    }

    /// The adapter is a pure pass-through: identical bytes to calling the encoder directly.
    #[test]
    fn from_rgba_buffer_matches_the_core_encoder() {
        let buf = buffer(64, 48, ColorSpace::Srgb);
        let adapted = LQIP::from_rgba_buffer(&buf).expect("RGBA8 buffer");
        let direct = Lqip::encode(64, 48, &buf.data, Gamut::Srgb).expect("valid frame");
        assert_eq!(adapted.as_bytes(), direct.as_bytes());
        assert_eq!(adapted.as_bytes().len(), 32);
    }

    /// The buffer's colour space reaches the encoder rather than being ignored.
    #[test]
    fn from_rgba_buffer_honours_the_buffer_color_space() {
        let srgb = LQIP::from_rgba_buffer(&buffer(32, 32, ColorSpace::Srgb)).expect("RGBA8");
        let p3 = LQIP::from_rgba_buffer(&buffer(32, 32, ColorSpace::DisplayP3)).expect("RGBA8");
        assert_ne!(srgb.as_bytes(), p3.as_bytes());
    }

    #[test]
    fn from_rgba_buffer_rejects_a_non_rgba8_buffer() {
        let gray = ImageBuffer::new(
            vec![0u8; 16],
            4,
            4,
            PixelFormat::Gray,
            ComponentType::U8,
            ColorSpace::Srgb,
        )
        .expect("well-formed gray buffer");
        assert!(matches!(
            LQIP::from_rgba_buffer(&gray),
            Err(LQIPError::UnsupportedFormat { .. })
        ));
    }

    /// The exact shape `lifecycle::derivatives` drives: buffer in, sidecar record out.
    #[test]
    fn to_sidecar_produces_a_chromahash_record() {
        let lqip = LQIP::from_rgba_buffer(&buffer(80, 40, ColorSpace::Srgb)).expect("RGBA8");
        let record = lqip.to_sidecar().expect("infallible conversion");
        assert_eq!(record.chromahash, lqip.as_bytes());
        assert_eq!(record.chromahash.len(), 32);
        assert_eq!(record.format_version, LQIP_FORMAT_V1);
        assert_eq!(record.dominant_color, lqip.hash().dominant_color());
    }
}
