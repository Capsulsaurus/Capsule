//! The decode seam: bytes in, orientation-applied RGBA8 out.
//!
//! # The pipeline, and why each step is here
//!
//! 1. **Identify** ([`StillFormat::detect`]) — header first, so a `.jpg` that is really a HEIC
//!    is classified as HEIC.
//! 2. **Gate** ([`StillFormat::is_decodable`]) — refuse with a typed
//!    [`MediaError::UnsupportedFormat`] *before* touching a decoder, so a HEIC never reaches a
//!    stub that would return a less informative error.
//! 3. **Budget** ([`MAX_DECODE_PIXELS`]) — a header-only
//!    [`probe`](Decoder::probe) refuses an oversized frame before the decoder allocates. It has
//!    to be pre-decode: `rawshift-image` decodes to interleaved RGB `u16`, so the bomb is
//!    inside the decoder, not in Capsule's copy of the result.
//! 4. **Decode**, then **apply the EXIF orientation** to the pixels, so the frame this module
//!    returns is always upright and its dimensions are the ones a viewer shows.
//! 5. **Normalise** to packed RGBA8, which is what [`crate::lqip`] and the downscale both take.
//!
//! # Two lossy edges, both deliberate and both asserted
//!
//! - **Alpha is lost.** `rawshift-image`'s decode target is interleaved RGB `u16` with no alpha
//!   channel, and `decode_png` drops the alpha channel outright. Every frame this module
//!   returns is therefore opaque. Nothing downstream needs alpha — the LQIP is an opaque
//!   placeholder and the thumbnail is composited onto a grid — but a caller must not *assume*
//!   transparency survived, so a test pins the flattening rather than leaving it to be
//!   discovered.
//! - **16-bit is narrowed to 8.** `(sample >> 8) as u8` is the exact inverse of the crate's own
//!   `u8_to_u16` widening (`v * 257`), so an 8-bit source round-trips bit-exactly and only a
//!   genuinely deeper source loses its low byte. That is the same narrowing every encode
//!   backend in the crate performs anyway.
//!
//! # The panic guard
//!
//! [`decode_guarded`] wraps a [`Decoder`] call in [`std::panic::catch_unwind`]. It is a free
//! function over the trait rather than a detail inside [`RawshiftDecoder`] for one reason: a
//! test has to be able to prove the guard holds, and it can only do that by injecting a
//! [`Decoder`] that panics.

use std::panic::{AssertUnwindSafe, catch_unwind};

use rawshift_image::core::ColorSpace;
use rawshift_image::core::image::RgbImage;
use rawshift_image::formats::{
    StandardFormat, decode_standard_image, probe_standard_image, read_standard_image_metadata,
};
use rawshift_image::transforms::orientation::apply_orientation;

use super::detect::{MAX_DECODE_PIXELS, StillFormat};
use super::error::{FormatOp, MediaError};
use crate::lqip::{Gamut, RgbaImage};

/// The EXIF orientation value meaning "already upright".
const ORIENTATION_NORMAL: u16 = 1;

/// What a header-only probe can say about a still, normalised onto Capsule's own types.
///
/// This is the "metadata normalisation" half of the module: `rawshift-image` reports a format,
/// a size, an optional bit depth and a colour space, plus (from a separate EXIF read) an
/// orientation. None of those types may appear in Capsule's public surface — they belong to a
/// pre-1.0 dependency — so each is mapped onto a Capsule type here, once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaMetadata {
    /// The identified format.
    pub format: StillFormat,
    /// Dimensions **as stored**, i.e. before the EXIF orientation is applied. A probe reads the
    /// codec header, which knows nothing about the orientation tag; the upright dimensions are
    /// what [`DecodedImage`] carries.
    pub stored_dimensions: (u32, u32),
    /// The EXIF orientation tag (1..=8), where the format carries one and it was readable.
    pub orientation: Option<u16>,
    /// Bits per channel, where the header exposes it cheaply.
    pub bit_depth: Option<u8>,
    /// The source colour space, mapped onto the gamut [`crate::lqip::Lqip::encode`] takes.
    pub gamut: Gamut,
}

impl MediaMetadata {
    /// The dimensions a viewer shows: the stored ones, transposed when the orientation tag is
    /// one of the four quarter-turns (5, 6, 7, 8).
    pub const fn upright_dimensions(&self) -> (u32, u32) {
        let (width, height) = self.stored_dimensions;
        match self.orientation {
            Some(5..=8) => (height, width),
            _ => (width, height),
        }
    }
}

/// A decoded still: upright, opaque, packed RGBA8.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedImage {
    /// The pixels, `width * height * 4` bytes, alpha uniformly `255`.
    ///
    /// [`crate::lqip::RgbaImage`] rather than a new buffer type: it is already the shape
    /// [`crate::lqip::Lqip::encode`] and [`downscale_rgba8`](super::downscale_rgba8) take, and
    /// it is unconditional, so no `media`-only type reaches the LQIP contract.
    pub image: RgbaImage,
    /// The source colour space this frame's samples are in.
    pub gamut: Gamut,
    /// The EXIF orientation value that was **consumed** — the transform is already applied to
    /// `image`, so a renderer that rotates again is double-applying. `1` when the source
    /// carried no tag.
    pub orientation_applied: u16,
    /// The format the pixels came out of.
    pub format: StillFormat,
}

impl DecodedImage {
    /// Frame width in pixels, upright.
    pub const fn width(&self) -> u32 {
        self.image.width
    }

    /// Frame height in pixels, upright.
    pub const fn height(&self) -> u32 {
        self.image.height
    }
}

/// The still-decode seam.
///
/// A trait with exactly one production implementation ([`RawshiftDecoder`]), and that is the
/// point: the failure modes worth testing — a panicking decoder, a decoder that reports
/// dimensions its buffer does not match — cannot be produced from real bytes on demand, so they
/// are injected.
pub trait Decoder {
    /// Read the format, dimensions and orientation from the header without decoding pixels.
    ///
    /// # Errors
    /// [`MediaError::NotAStillImage`] when nothing recognisable is there,
    /// [`MediaError::UnsupportedFormat`] when the format has no decoder in this build, and
    /// [`MediaError::PixelBudgetExceeded`] when the header claims more than
    /// [`MAX_DECODE_PIXELS`].
    fn probe(&self, bytes: &[u8], ext: &str) -> Result<MediaMetadata, MediaError>;

    /// Decode to upright, packed RGBA8. Probes first, so every [`probe`](Self::probe) error is
    /// also a `decode` error.
    ///
    /// # Errors
    /// As [`probe`](Self::probe), plus [`MediaError::Decode`] when a supported format's bytes
    /// do not decode and [`MediaError::BufferLengthMismatch`] / [`MediaError::ZeroDimension`]
    /// when the decoder's own output is inconsistent.
    fn decode(&self, bytes: &[u8], ext: &str) -> Result<DecodedImage, MediaError>;
}

/// The `rawshift-image`-backed [`Decoder`] — the only production implementation.
#[derive(Debug, Clone, Copy, Default)]
pub struct RawshiftDecoder;

impl Decoder for RawshiftDecoder {
    #[tracing::instrument(level = "debug", skip_all, fields(bytes = bytes.len(), ext))]
    fn probe(&self, bytes: &[u8], ext: &str) -> Result<MediaMetadata, MediaError> {
        let format = gate(bytes, ext, FormatOp::Decode)?;
        let probe = probe_standard_image(bytes).map_err(|e| MediaError::Decode {
            format,
            detail: format!("header probe: {e}"),
        })?;
        let (width, height) = (probe.size.width, probe.size.height);
        if width == 0 || height == 0 {
            return Err(MediaError::ZeroDimension { width, height });
        }
        let pixels = u64::from(width) * u64::from(height);
        if pixels > MAX_DECODE_PIXELS {
            tracing::warn!(
                %format,
                width,
                height,
                pixels,
                limit = MAX_DECODE_PIXELS,
                "media: refusing an oversized still before the decoder allocates"
            );
            return Err(MediaError::PixelBudgetExceeded {
                pixels,
                limit: MAX_DECODE_PIXELS,
            });
        }
        let metadata = MediaMetadata {
            format,
            stored_dimensions: (width, height),
            orientation: orientation_of(bytes, format),
            bit_depth: probe.bit_depth,
            gamut: gamut_of(probe.color_space),
        };
        tracing::debug!(
            %format,
            width,
            height,
            orientation = ?metadata.orientation,
            bit_depth = ?metadata.bit_depth,
            gamut = ?metadata.gamut,
            "media: probed a still"
        );
        Ok(metadata)
    }

    #[tracing::instrument(level = "debug", skip_all, fields(bytes = bytes.len(), ext))]
    fn decode(&self, bytes: &[u8], ext: &str) -> Result<DecodedImage, MediaError> {
        let probed = self.probe(bytes, ext)?;
        let format = probed.format;
        let mut rgb = decode_standard_image(bytes, standard_format(format)).map_err(|e| {
            MediaError::Decode {
                format,
                detail: e.to_string(),
            }
        })?;
        check_rgb_buffer(&rgb, format)?;

        let orientation = probed.orientation.unwrap_or(ORIENTATION_NORMAL);
        if orientation != ORIENTATION_NORMAL {
            apply_orientation(&mut rgb, orientation);
            check_rgb_buffer(&rgb, format)?;
        }

        let image = to_rgba8(&rgb);
        tracing::debug!(
            %format,
            width = image.width,
            height = image.height,
            orientation,
            "media: decoded a still"
        );
        Ok(DecodedImage {
            image,
            gamut: probed.gamut,
            orientation_applied: orientation,
            format,
        })
    }
}

/// Run `decoder.decode` with the unwind boundary an import needs.
///
/// A pre-1.0 decoder fed untrusted bytes is exactly where a panic is plausible, and a missing
/// thumbnail must never be able to abort an import that has already written signed, encrypted
/// bytes. A caught unwind becomes [`MediaError::DecoderPanic`] — reported, not swallowed.
pub fn decode_guarded(
    decoder: &dyn Decoder,
    bytes: &[u8],
    ext: &str,
) -> Result<DecodedImage, MediaError> {
    if let Ok(result) = catch_unwind(AssertUnwindSafe(|| decoder.decode(bytes, ext))) {
        return result;
    }
    tracing::warn!(
        bytes = bytes.len(),
        ext,
        "media: a decoder panicked; the original is imported without a derivative"
    );
    Err(MediaError::DecoderPanic)
}

/// Identify a still and refuse anything this build has no codec for, before any decoder runs.
fn gate(bytes: &[u8], ext: &str, op: FormatOp) -> Result<StillFormat, MediaError> {
    let Some(format) = StillFormat::detect(bytes, ext) else {
        return Err(MediaError::NotAStillImage);
    };
    if !format.is_decodable() {
        return Err(MediaError::UnsupportedFormat { format, op });
    }
    Ok(format)
}

/// Map Capsule's format onto the crate's. Total by construction over the decodable set, which
/// is the only set that reaches here — [`gate`] rejects the rest, and every non-decodable
/// variant is a format the crate either cannot name without a feature (HEIC) or cannot decode
/// as a standard image at all (the RAW families).
fn standard_format(format: StillFormat) -> StandardFormat {
    match format {
        StillFormat::Jpeg => StandardFormat::Jpeg,
        StillFormat::Png => StandardFormat::Png,
        StillFormat::WebP => StandardFormat::WebP,
        StillFormat::Jxl => StandardFormat::Jxl,
        StillFormat::Gif => StandardFormat::Gif,
        StillFormat::Ppm => StandardFormat::Ppm,
        StillFormat::Avif => StandardFormat::Avif,
        // The container, for the formats whose container is all `rawshift-image` models: the
        // TIFF-based RAW families are a TIFF to it, and Canon's CR3 is an ISO-BMFF file it can
        // only reach through its HEIC arm. Every one of these is unreachable through `gate`,
        // which refuses a non-decodable format before this runs. Mapped to the truth rather
        // than panicking so a future `is_decodable` widening that forgets this table degrades
        // to a decode error instead of aborting an import.
        StillFormat::Tiff
        | StillFormat::Arw
        | StillFormat::Cr2
        | StillFormat::Crw
        | StillFormat::Dng
        | StillFormat::Nef
        | StillFormat::Raf => StandardFormat::Tiff,
        StillFormat::Heic | StillFormat::Cr3 => StandardFormat::Heic,
    }
}

/// The EXIF orientation tag, where the format carries one. Only JPEG, TIFF, WebP, PNG and AVIF
/// have an EXIF block the crate's parser reads; the others return `None` rather than guessing.
fn orientation_of(bytes: &[u8], format: StillFormat) -> Option<u16> {
    let metadata = read_standard_image_metadata(bytes, standard_format(format));
    // Only the eight defined values are honoured. `apply_orientation` warns and no-ops on
    // anything else, which would leave `orientation_applied` claiming a transform that never
    // happened — so an out-of-range tag is dropped here instead.
    metadata.image.orientation.filter(|o| (1..=8).contains(o))
}

/// Map the crate's colour space onto the gamut the LQIP encoder takes.
///
/// `LinearSrgb` and `Unknown` both become [`Gamut::Srgb`]: `Linear` names a transfer function
/// rather than a gamut, and sRGB primaries are the only safe assumption for an untagged source
/// (over-saturating is worse than under-saturating — the resolution slice `S-B14` recorded).
fn gamut_of(color_space: ColorSpace) -> Gamut {
    match color_space {
        ColorSpace::DisplayP3 => Gamut::DisplayP3,
        ColorSpace::AdobeRgb => Gamut::AdobeRgb,
        ColorSpace::Rec2020 => Gamut::Bt2020,
        ColorSpace::ProPhotoRgb => Gamut::ProPhotoRgb,
        // `Srgb`, `LinearSrgb`, `Unknown`, and — because `ColorSpace` is `#[non_exhaustive]` —
        // any variant a future release adds. One wildcard rather than an explicit list plus a
        // catch-all, since the answer is the same and two arms with one body only look like a
        // distinction. sRGB is the conservative default: under-saturating a wide-gamut source
        // is a smaller defect than over-saturating a narrow one.
        _ => Gamut::Srgb,
    }
}

/// Refuse a decoder result whose buffer does not match the dimensions it reports.
///
/// `RgbImage::new` performs no validation and `set_size` is public, so a decoder bug (or a
/// transform bug) can produce an inconsistent value. Checked here because the very next thing
/// Capsule does is index that buffer by those dimensions.
fn check_rgb_buffer(rgb: &RgbImage, format: StillFormat) -> Result<(), MediaError> {
    let (width, height) = (rgb.width(), rgb.height());
    if width == 0 || height == 0 {
        return Err(MediaError::ZeroDimension { width, height });
    }
    let expected = u128::from(width) * u128::from(height) * 3;
    let actual = rgb.data.len() as u128;
    if expected != actual {
        return Err(MediaError::BufferLengthMismatch {
            format,
            width,
            height,
            expected,
            actual,
        });
    }
    Ok(())
}

/// Narrow interleaved RGB `u16` to packed, opaque RGBA8.
///
/// `sample >> 8` is the exact inverse of the crate's `v * 257` widening, so an 8-bit source is
/// reproduced bit-for-bit.
fn to_rgba8(rgb: &RgbImage) -> RgbaImage {
    let mut rgba = Vec::with_capacity(rgb.data.len() / 3 * 4);
    for px in rgb.data.chunks_exact(3) {
        rgba.push((px[0] >> 8) as u8);
        rgba.push((px[1] >> 8) as u8);
        rgba.push((px[2] >> 8) as u8);
        rgba.push(u8::MAX);
    }
    RgbaImage {
        width: rgb.width(),
        height: rgb.height(),
        rgba,
    }
}
