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
//!    inside the decoder, not in Capsule's copy of the result. An AVIF's header size is its
//!    `ispe` box, a claim the AV1 frame need not honour, and `rawshift-image` takes no
//!    frame-size limit for the AV1 decoder (visualcommons/rawshift#79), so an AVIF is bounded
//!    by its `ispe` before decode and by its decoded frame after it, not by its coded frame.
//! 4. **Decode**, then **apply the EXIF orientation** to the pixels, so the frame this module
//!    returns is always upright and its dimensions are the ones a viewer shows.
//! 5. **Normalise** to packed RGBA8, which is what [`crate::lqip`] and the downscale both take.
//!
//! # RAW: the same five steps through a different reader
//!
//! ARW and DNG (issue #589) do not go through `decode_standard_image`. They go through
//! `rawshift-image`'s `RawFile::open(..)` and `process(..)`, which runs black level, white
//! balance, demosaic, colour matrix, tone mapping, the file's default crop **and the EXIF
//! orientation** itself, then tags the result display-referred sRGB. So the RAW path differs at
//! three steps:
//!
//! - **Budget.** For an ARW, `RawFile::open` parses the IFDs and reads no pixels, so the probe
//!   *is* the open. The budget is checked against every frame the pipeline will allocate from
//!   the file's own claims — the sensor, the active area, the default crop and each tile — and
//!   every strip and tile byte range must lie inside the file, because the reader allocates each
//!   one at the length the file states before it reads a byte. The work is bounded as well as
//!   each allocation: a tile or strip must fit inside the sensor, and the file may list no more
//!   of them than the sensor's grid holds. A DNG's `open` instead decodes the whole raw image
//!   through gamut, which borrows each range in place, refuses one outside the file, a strip
//!   list longer than the rows, a tile list that is not the grid and a JPEG XL tile (ProRAW)
//!   whose codestream is not its tile's size, but allocates the image and each tile at their
//!   stated sizes. So before a DNG is opened, every image and tile any of its IFDs states is held
//!   to the budget, and after it the sensor, active area and crop are held as for an ARW. Before
//!   either is opened, a no-allocation walk of the IFD chain (`tiff_bounds`, issue #644)
//!   refuses as [`MediaError::Decode`] any entry whose value needs more bytes than the file
//!   holds, and (issue #647) any IFD nested deeper than a recursive reader can safely follow.
//! - **Orientation** is applied by `process`, so it is reported in `orientation_applied` and
//!   never applied a second time here.
//! - **Unsupported variants defer.** Upstream decodes Sony lossless-JPEG ARW and uncompressed,
//!   Deflate, lossless-JPEG or JPEG XL DNG. Any other ARW compression, a camera its reader does
//!   not place, or a TIFF that is not a RAW at all is `RawError::Unsupported`, and that is mapped
//!   to
//!   [`MediaError::UnsupportedFormat`] — `DeferredNoCodec`, backfillable when upstream grows the
//!   decoder — rather than [`MediaError::Decode`], which would report a known gap as a broken file.
//!
//! The processing options are upstream's defaults (as-shot white balance, the file's or the
//! camera database's colour matrix, the default tone curve) except the demosaic, which is
//! **bilinear** rather than the AMaZE that `DemosaicMethod::Auto` picks. AMaZE holds nine
//! full-frame `f32`/`i32` planes alongside the raw and RGB buffers, about 44 bytes a pixel, which
//! on a 61 Mpx frame is ~2.7 GB on a phone; bilinear holds none. Every frame this module produces
//! feeds a downscaled derivative or the LQIP, where bilinear's edge artefacts do not survive the
//! resample.
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

use std::io::Cursor;
use std::panic::{AssertUnwindSafe, catch_unwind};

use rawshift_image::core::{ColorDescription, ColourPrimaries, RgbImage};
use rawshift_image::error::RawError;
use rawshift_image::formats::{
    RawFile, StandardFormat, decode_standard_image, probe_standard_image,
    read_standard_image_metadata,
};
use rawshift_image::processing::{BayerAlgorithm, DemosaicMethod, ProcessingOptions};
use rawshift_image::transforms::orientation::apply_orientation;

use super::detect::{MAX_DECODE_PIXELS, StillFormat};
use super::error::{FormatOp, MediaError};
use super::tiff_bounds::{IfdRefusal, IfdWalk, MAX_IFD_DEPTH, check_ifd_bounds};
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
    /// The gamut [`crate::lqip::Lqip::encode`] is told to interpret the samples in.
    ///
    /// **Always [`Gamut::Srgb`] in this build, and that is upstream's doing rather than a
    /// choice made here.** `probe_standard_image` hard-codes `ColorDescription::SRGB` into every
    /// `ImageProbe` it returns, and `decode_standard_image` likewise tags every decoded frame
    /// sRGB without converting — so `rawshift-image` reports no source gamut for a
    /// standard format at all, whatever the file's ICC profile says. The consequence is a
    /// fidelity limitation, not a correctness bug: a Display P3 source gets a placeholder
    /// interpreted as sRGB, i.e. slightly under-saturated, which is the safer direction
    /// when one has to be picked. This module's private `gamut_of` is kept as the single
    /// mapping point for when
    /// the crate does start reporting it. A RAW frame is sRGB for a better reason: its colour
    /// matrix converts camera RGB to sRGB primaries, so sRGB is what the samples really are.
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
        if format.is_raw() {
            return open_raw(bytes, format).map(|(_, metadata)| metadata);
        }
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
        let format = gate(bytes, ext, FormatOp::Decode)?;
        if format.is_raw() {
            return decode_raw(bytes, format);
        }
        let probed = self.probe(bytes, ext)?;
        let format = probed.format;
        let mut rgb = decode_standard_image(bytes, standard_format(format))
            .map_err(|e| standard_error(format, e))?;
        check_rgb_buffer(&rgb, format)?;
        // The probe held the header's size to the budget, but a header can understate its frame
        // (an AVIF's `ispe` is only a claim about its AV1 frame), so the decoded frame is held to
        // it too before Capsule copies or resamples it.
        check_frame_budget(format, rgb.width(), rgb.height(), MAX_DECODE_PIXELS)?;

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
    guarded("decode", || decoder.decode(bytes, ext))
}

/// Run any fallible step of the still pipeline behind the same unwind boundary.
///
/// Public because its one caller, the derivative pipeline in `capsule-library`'s `lifecycle`, is
/// in another crate; the `catch_unwind` itself stays here, the one module the stateless core
/// sanctions it in.
///
/// Not just for `decode` — that is not the only third-party code the import path runs over pixels:
/// the placeholder goes through `chromahash` (also pre-1.0) and the derivative through the
/// encoder, and the module's promise is that *none* of them can abort an import — not that the
/// decoder specifically cannot. `stage` names the step in the warning so a caught panic is
/// attributable.
///
/// `AssertUnwindSafe` is sound for the callers here: each closure borrows shared slices and
/// stateless values, so a caught unwind cannot leave a Capsule-owned invariant torn.
pub fn guarded<T>(
    stage: &'static str,
    step: impl FnOnce() -> Result<T, MediaError>,
) -> Result<T, MediaError> {
    if let Ok(result) = catch_unwind(AssertUnwindSafe(step)) {
        return result;
    }
    tracing::warn!(
        stage,
        "media: a third-party codec panicked; the original is imported without a derivative"
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

/// Refuse a decoded frame of more than `limit` pixels.
fn check_frame_budget(
    format: StillFormat,
    width: u32,
    height: u32,
    limit: u64,
) -> Result<(), MediaError> {
    let pixels = u64::from(width) * u64::from(height);
    if pixels > limit {
        tracing::warn!(
            %format,
            width,
            height,
            pixels,
            limit,
            "media: refusing a decoded frame larger than its header claimed and the budget allows"
        );
        return Err(MediaError::PixelBudgetExceeded { pixels, limit });
    }
    Ok(())
}

/// [`check_frame_budget`] over a decoded still, with the budget as a parameter so a test can
/// exceed it with a small frame.
#[cfg(test)]
pub(super) fn check_decoded_budget(decoded: &DecodedImage, limit: u64) -> Result<(), MediaError> {
    check_frame_budget(decoded.format, decoded.width(), decoded.height(), limit)
}

/// Map a standard-format decode error onto Capsule's, keeping a missing decoder apart from a
/// broken file.
///
/// `RawError::HwDecoderUnavailable` is upstream saying this host has no decoder for the
/// codestream inside a container it parsed: an AVIF's AV1, which `rawshift-image` decodes only
/// through a hardware backend until it gains a software fallback
/// (visualcommons/rawshift#78). That is a codec this host lacks, not a broken file, so it
/// becomes the backfillable [`MediaError::UnsupportedFormat`], exactly like a build that links
/// no AVIF decoder at all. Everything else is a real decode failure.
fn standard_error(format: StillFormat, error: RawError) -> MediaError {
    match error {
        RawError::HwDecoderUnavailable { codec, reason } => {
            tracing::warn!(
                %format,
                codec,
                %reason,
                "media: this host has no decoder for the codestream; deferring"
            );
            MediaError::UnsupportedFormat {
                format,
                op: FormatOp::Decode,
            }
        }
        other => MediaError::Decode {
            format,
            detail: other.to_string(),
        },
    }
}

/// A RAW file opened over the caller's bytes.
type OpenRaw<'a> = RawFile<Cursor<&'a [u8]>>;

/// The options every RAW decode runs with: upstream's defaults, demosaicked bilinearly. The
/// module docs say why bilinear rather than `DemosaicMethod::Auto`'s AMaZE.
fn raw_processing_options() -> ProcessingOptions {
    ProcessingOptions::new().demosaic(DemosaicMethod::Bayer(BayerAlgorithm::Bilinear))
}

/// What a RAW file's own metadata claims about the buffers decoding it will allocate.
///
/// Gathered once per family, from the crate's per-family metadata rather than its unified
/// `ImageMetadata` (which carries no sensor size and no strip or tile layout), so the budget,
/// the byte-range check and the probe all read the same numbers.
#[derive(Debug)]
struct RawLayout {
    /// The full sensor frame `decode_raw` allocates.
    sensor: (u32, u32),
    /// The active area (`x`, `y`, `w`, `h`) the demosaic allocates and indexes the sensor by.
    active: (u32, u32, u32, u32),
    /// The default crop (`x`, `y`, `w`, `h`), relative to the active area, that `process`
    /// applies when it fits.
    crop: Option<(u32, u32, u32, u32)>,
    /// The frame each range decodes into, as the file declares it: the tile size, or a strip's
    /// sensor width by its rows per strip (the whole sensor where it states none). The reader
    /// places range `i` on this grid, so the grid also bounds how many ranges there can be.
    unit_size: (u32, u32),
    /// `"strip"` or `"tile"`: what [`ranges`](Self::ranges) holds, for the error detail.
    unit: &'static str,
    /// Every strip or tile as `(offset, length)`, each of which the reader allocates at the
    /// stated length before it reads. Empty for a DNG, whose reader no longer reports its ranges
    /// and checks them itself without allocating them (see [`raw_layout`]).
    ranges: Vec<(u64, u64)>,
    bit_depth: u8,
    orientation: Option<u16>,
}

impl RawLayout {
    /// The frame `process` returns before orienting it: the active area, or the default crop
    /// where it fits inside it. This is what [`MediaMetadata::stored_dimensions`] reports for a
    /// RAW, because the sensor's masked border is never shown.
    fn rendered(&self) -> (u32, u32) {
        let (_, _, width, height) = self.active;
        match self.crop {
            Some((x, y, w, h))
                if u64::from(x) + u64::from(w) <= u64::from(width)
                    && u64::from(y) + u64::from(h) <= u64::from(height) =>
            {
                (w, h)
            }
            _ => (width, height),
        }
    }
}

/// Read the layout out of whichever family `RawFile::open` identified.
///
/// The match is exhaustive over the families this build enables (`arw-decode`, `dng-decode`),
/// with no wildcard, on purpose: enabling another family is then a compile error here until its
/// layout — and so its budget — is written.
///
/// `max_tile` is the IFD walk's [`IfdWalk::max_tile`](super::tiff_bounds::IfdWalk::max_tile),
/// which stands in for the DNG tile size the reader no longer reports.
fn raw_layout(
    raw: &OpenRaw<'_>,
    format: StillFormat,
    max_tile: (u64, u64),
) -> Result<RawLayout, MediaError> {
    let missing = || MediaError::Decode {
        format,
        detail: "RAW reader returned no metadata".to_owned(),
    };
    let pairs = |offsets: &[u64], lengths: &[u64]| -> Vec<(u64, u64)> {
        offsets
            .iter()
            .copied()
            .zip(lengths.iter().copied())
            .collect()
    };
    Ok(match raw {
        RawFile::Arw(arw) => {
            let m = arw.metadata().ok_or_else(missing)?;
            let tiled = !m.tile_offsets.is_empty() && m.tile_width > 0 && m.tile_height > 0;
            RawLayout {
                sensor: (m.sensor_size.width, m.sensor_size.height),
                active: (
                    m.active_area.origin.x,
                    m.active_area.origin.y,
                    m.active_area.size.width,
                    m.active_area.size.height,
                ),
                // ARW's crop tags *are* its active area; `process` applies no second crop.
                crop: None,
                // An untiled ARW is one LJPEG stream covering the whole sensor.
                unit_size: if tiled {
                    (m.tile_width, m.tile_height)
                } else {
                    (m.sensor_size.width, m.sensor_size.height)
                },
                unit: if tiled { "tile" } else { "strip" },
                ranges: if tiled {
                    pairs(&m.tile_offsets, &m.tile_byte_counts)
                } else {
                    vec![(m.raw_data_offset, m.raw_data_size)]
                },
                bit_depth: m.bit_depth,
                orientation: m.orientation,
            }
        }
        RawFile::Dng(dng) => {
            let m = dng.metadata().ok_or_else(missing)?;
            // `DngMetadata` no longer reports the strip or tile layout: gamut's DNG reader
            // resolves it at decode, refusing a range outside the file, a strip list longer than
            // the rows and a tile list that does not match the grid, and borrowing each range
            // rather than allocating it. What it does not bound is a tile's own size, which it
            // decodes into before placing it, so the tile size is taken from the IFD walk's
            // largest stated `TileWidth` and `TileLength` and held to the sensor below. A
            // stripped DNG's strips are at most the sensor's width by its height.
            let (tile_w, tile_h) = max_tile;
            let tiled = tile_w > 0 || tile_h > 0;
            let clamp = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
            let (sensor_w, sensor_h) = (m.sensor_size.width, m.sensor_size.height);
            RawLayout {
                sensor: (sensor_w, sensor_h),
                active: m.active_area.map_or((0, 0, sensor_w, sensor_h), |area| {
                    (
                        area.origin.x,
                        area.origin.y,
                        area.size.width,
                        area.size.height,
                    )
                }),
                crop: m
                    .default_crop_origin
                    .zip(m.default_crop_size)
                    .map(|((x, y), (w, h))| (x, y, w, h)),
                unit_size: if tiled {
                    (clamp(tile_w), clamp(tile_h))
                } else {
                    (sensor_w, sensor_h)
                },
                unit: if tiled { "tile" } else { "strip" },
                // Upstream checks the ranges themselves; see above.
                ranges: Vec::new(),
                bit_depth: m.bit_depth,
                orientation: m.orientation,
            }
        }
    })
}

/// Hold a DNG's stated frames to the budget before `RawFile::open`, which for a DNG decodes the
/// whole raw image (gamut's reader is eager) and so allocates for the image and each tile at
/// the sizes the file states before Capsule can read a layout back.
///
/// The bound is taken from the IFD walk rather than from the reader: the largest image any IFD
/// states, and the largest tile width and length any IFD states. Every IFD's image is held, the
/// raw one among them, because which IFD is the raw image is the reader's choice. ARW's reader
/// reads no pixels at open, so its layout is checked after it, by [`check_raw_layout`].
fn check_eager_raw_frames(
    format: StillFormat,
    walk: &IfdWalk,
    limit: u64,
) -> Result<(), MediaError> {
    if format != StillFormat::Dng {
        return Ok(());
    }
    let clamp = |v: u64| u32::try_from(v).unwrap_or(u32::MAX);
    let (width, height) = walk.max_image;
    if (width, height) != (0, 0) {
        check_raw_frame(format, "image", (clamp(width), clamp(height)), limit)?;
    }
    let (tile_w, tile_h) = walk.max_tile;
    if tile_w > 0 || tile_h > 0 {
        check_raw_frame(format, "tile", (clamp(tile_w), clamp(tile_h)), limit)?;
    }
    Ok(())
}

/// Refuse a frame the RAW pipeline would allocate for that is empty or over `limit` pixels.
fn check_raw_frame(
    format: StillFormat,
    frame: &'static str,
    (width, height): (u32, u32),
    limit: u64,
) -> Result<(), MediaError> {
    if width == 0 || height == 0 {
        return Err(MediaError::ZeroDimension { width, height });
    }
    let pixels = u64::from(width) * u64::from(height);
    if pixels > limit {
        tracing::warn!(
            %format,
            frame,
            width,
            height,
            pixels,
            limit,
            "media: refusing an oversized RAW frame before the decoder allocates"
        );
        return Err(MediaError::PixelBudgetExceeded { pixels, limit });
    }
    Ok(())
}

/// Hold a RAW layout to the decode budget and to the bytes actually present.
///
/// Every check here is against a claim the file makes and the reader would otherwise trust:
/// it allocates the sensor, the active area, each tile and each strip or tile read buffer at
/// the size the file states, and an allocation failure aborts rather than unwinds, so
/// [`decode_guarded`] cannot catch it. Indexing faults (an active area outside the sensor) do
/// unwind, but are refused here too so they report as a decode failure rather than a panic.
///
/// The number of decodes is held too, not only the size of each: every strip or tile must fit
/// the sensor, and there may be no more of them than the sensor's grid of that size holds.
fn check_raw_layout(
    bytes: &[u8],
    format: StillFormat,
    layout: &RawLayout,
    limit: u64,
) -> Result<(), MediaError> {
    let (x, y, width, height) = layout.active;
    check_raw_frame(format, "sensor", layout.sensor, limit)?;
    check_raw_frame(format, "active area", (width, height), limit)?;
    if let Some((_, _, crop_w, crop_h)) = layout.crop {
        check_raw_frame(format, "default crop", (crop_w, crop_h), limit)?;
    }
    check_raw_frame(format, layout.unit, layout.unit_size, limit)?;
    let (sensor_w, sensor_h) = layout.sensor;
    let unit = layout.unit;
    let (unit_w, unit_h) = layout.unit_size;
    if unit_w > sensor_w || unit_h > sensor_h {
        return Err(MediaError::Decode {
            format,
            detail: format!(
                "{unit} size {unit_w}x{unit_h} exceeds the {sensor_w}x{sensor_h} sensor"
            ),
        });
    }
    // The reader places range `i` at column `i % across`, row `i / across`; anything past the
    // grid is a range it would decode in full and then place nowhere (or underflow on).
    let grid = u64::from(sensor_w.div_ceil(unit_w)) * u64::from(sensor_h.div_ceil(unit_h));
    let count = layout.ranges.len() as u64;
    if count > grid {
        return Err(MediaError::Decode {
            format,
            detail: format!(
                "{count} {unit}s listed, but a {sensor_w}x{sensor_h} sensor holds at most \
                 {grid} {unit}s of {unit_w}x{unit_h}"
            ),
        });
    }
    if u64::from(x) + u64::from(width) > u64::from(sensor_w)
        || u64::from(y) + u64::from(height) > u64::from(sensor_h)
    {
        return Err(MediaError::Decode {
            format,
            detail: format!(
                "active area {width}x{height} at ({x}, {y}) lies outside the \
                 {sensor_w}x{sensor_h} sensor"
            ),
        });
    }
    // A DNG's ranges are its reader's to check (see `raw_layout`), so only the families that
    // report them can be refused for listing none.
    if layout.ranges.is_empty() && format != StillFormat::Dng {
        return Err(MediaError::Decode {
            format,
            detail: "RAW file locates no strip or tile data".to_owned(),
        });
    }
    let file_len = bytes.len() as u64;
    // A JPEG XL tile (ProRAW) is a DNG's, whose ranges its reader checks: it refuses a
    // codestream whose header is not its tile's size before decoding it.
    for (index, &(offset, length)) in layout.ranges.iter().enumerate() {
        if offset.checked_add(length).is_none_or(|end| end > file_len) {
            return Err(MediaError::Decode {
                format,
                detail: format!(
                    "{unit} {index} ({length} bytes at {offset}) runs past the {file_len}-byte file"
                ),
            });
        }
    }
    Ok(())
}

/// Map a RAW reader error onto Capsule's, keeping a known gap apart from a broken file.
///
/// `RawError::Unsupported` is upstream saying it has no decoder for this variant — an
/// unimplemented compression, a camera it cannot place, a TIFF that is no RAW at all — so it
/// becomes the backfillable [`MediaError::UnsupportedFormat`]. Upstream also uses it for a few
/// truncated-strip cases; those then defer rather than fail, and a backfill retries them, which
/// costs a decode and nothing else. Everything else is a real decode failure.
fn raw_error(format: StillFormat, stage: &'static str, error: RawError) -> MediaError {
    match error {
        RawError::Unsupported(reason) => {
            tracing::warn!(
                %format,
                stage,
                %reason,
                "media: rawshift-image has no decoder for this RAW variant; deferring"
            );
            MediaError::UnsupportedFormat {
                format,
                op: FormatOp::Decode,
            }
        }
        other => MediaError::Decode {
            format,
            detail: format!("RAW {stage}: {other}"),
        },
    }
}

/// Probe a RAW still against an arbitrary pixel budget.
///
/// `pub(super)` so the tests can drive the budget with a limit a small fixture can exceed, the
/// way they drive `check_avif_frames`; production always goes through [`open_raw`] at
/// [`MAX_DECODE_PIXELS`].
#[cfg(test)]
pub(super) fn probe_raw(
    bytes: &[u8],
    format: StillFormat,
    limit: u64,
) -> Result<MediaMetadata, MediaError> {
    open_raw_within(bytes, format, limit).map(|(_, metadata)| metadata)
}

/// Open a RAW still and probe it: parse the IFDs, read the layout, and hold it to the budget.
/// Reads no pixels. The opened file is returned so [`decode_raw`] does not parse it twice.
fn open_raw(bytes: &[u8], format: StillFormat) -> Result<(OpenRaw<'_>, MediaMetadata), MediaError> {
    open_raw_within(bytes, format, MAX_DECODE_PIXELS)
}

/// [`open_raw`] with the budget as a parameter.
fn open_raw_within(
    bytes: &[u8],
    format: StillFormat,
    limit: u64,
) -> Result<(OpenRaw<'_>, MediaMetadata), MediaError> {
    // Before `open`, because `open` allocates each IFD entry's value at the count the entry
    // claims, and recurses into nested IFDs with no depth limit; see `tiff_bounds`.
    let walk = check_ifd_bounds(bytes).map_err(|refusal| {
        match &refusal {
            IfdRefusal::Oversized(oversized) => tracing::warn!(
                %format,
                ifd_offset = oversized.ifd_offset,
                tag = oversized.tag,
                data_type = oversized.data_type,
                count = oversized.count,
                file_len = oversized.file_len,
                "media: refused a RAW still whose IFD entry claims more than the file holds"
            ),
            IfdRefusal::TooDeep(deep) => tracing::warn!(
                %format,
                ifd_offset = deep.ifd_offset,
                depth = deep.depth,
                max_depth = MAX_IFD_DEPTH,
                "media: refused a RAW still whose IFDs nest past the depth bound"
            ),
        }
        MediaError::Decode {
            format,
            detail: format!("RAW IFD bounds: {refusal}"),
        }
    })?;
    tracing::trace!(
        %format,
        ifds = walk.ifds,
        entries = walk.entries,
        depth = walk.depth,
        "media: RAW IFD value counts are within the file and its IFDs within the depth bound"
    );
    check_eager_raw_frames(format, &walk, limit)?;
    let raw = RawFile::open(Cursor::new(bytes)).map_err(|e| raw_error(format, "open", e))?;
    let layout = raw_layout(&raw, format, walk.max_tile)?;
    check_raw_layout(bytes, format, &layout, limit)?;
    let metadata = MediaMetadata {
        format,
        stored_dimensions: layout.rendered(),
        // The same filter as the standard path: `process` no-ops an out-of-range tag, so
        // reporting it would claim a transform that never happened.
        orientation: layout.orientation.filter(|o| (1..=8).contains(o)),
        bit_depth: Some(layout.bit_depth),
        gamut: Gamut::Srgb,
    };
    tracing::debug!(
        %format,
        sensor = ?layout.sensor,
        stored = ?metadata.stored_dimensions,
        unit = layout.unit,
        units = layout.ranges.len(),
        orientation = ?metadata.orientation,
        bit_depth = layout.bit_depth,
        "media: probed a RAW still"
    );
    Ok((raw, metadata))
}

/// Decode a RAW still to upright, packed RGBA8 through the crate's full processing pipeline.
fn decode_raw(bytes: &[u8], format: StillFormat) -> Result<DecodedImage, MediaError> {
    let (mut raw, probed) = open_raw(bytes, format)?;
    let rgb = raw
        .process(&raw_processing_options())
        .map_err(|e| raw_error(format, "process", e))?;
    check_rgb_buffer(&rgb, format)?;
    // `process` has already applied the orientation; this only reports it.
    let orientation = probed.orientation.unwrap_or(ORIENTATION_NORMAL);
    let image = to_rgba8(&rgb);
    tracing::debug!(
        %format,
        width = image.width,
        height = image.height,
        orientation,
        "media: decoded a RAW still"
    );
    Ok(DecodedImage {
        image,
        gamut: probed.gamut,
        orientation_applied: orientation,
        format,
    })
}

/// A RAW file's embedded, camera-rendered JPEG, extracted without developing the RAW.
///
/// SSoT: [Thumbnails — Embedded Previews](https://docs/design/thumbnails/#embedded-previews).
/// Local only: it is read from an original this device holds and is never uploaded, signed or
/// listed as a tier. Its dimensions are what the render ladder places it by.
#[derive(Clone, PartialEq, Eq)]
pub struct EmbeddedPreview {
    /// The JPEG exactly as the RAW file carries it.
    pub jpeg: Vec<u8>,
    /// Width in pixels **upright**, i.e. after [`orientation`](Self::orientation) is applied.
    pub width: u32,
    /// Height in pixels upright.
    pub height: u32,
    /// The EXIF orientation (1..=8) a renderer applies to `jpeg`'s stored pixels: the JPEG's own
    /// tag where it carries one, else the RAW's, since a camera writes the orientation into the
    /// RAW's IFD0 and usually leaves the preview unrotated and untagged. `1` when neither does.
    /// [`decode_embedded_preview`] applies it; a platform decoder handed `jpeg` must apply it
    /// itself, and must not apply the JPEG's own tag a second time.
    pub orientation: u16,
}

impl std::fmt::Debug for EmbeddedPreview {
    // The JPEG never reaches the log.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EmbeddedPreview")
            .field("bytes", &self.jpeg.len())
            .field("width", &self.width)
            .field("height", &self.height)
            .field("orientation", &self.orientation)
            .finish()
    }
}

/// Extract the embedded preview of a RAW still, behind the pixel budget and the unwind guard.
///
/// `Ok(None)` when the bytes are a still that is not RAW, or a RAW that carries no embedded
/// JPEG. Reads no sensor data, so it works for a RAW variant whose pixels this build cannot
/// decode, provided the family is one it decodes.
///
/// The bounds are the ones every input gets: the IFD walk before `RawFile::open`, and before the
/// thumbnail read, the walk's largest stated JPEG length held to the file's own length (the
/// reader allocates it before it reads); then the JPEG's header is probed and its frame held to
/// [`MAX_DECODE_PIXELS`], so a renderer that decodes it allocates no more than any other still.
///
/// # Errors
///
/// [`MediaError::NotAStillImage`] and [`MediaError::UnsupportedFormat`] as [`Decoder::probe`],
/// [`MediaError::PixelBudgetExceeded`] for a preview frame over budget, [`MediaError::Decode`]
/// for a RAW or a preview that does not read, and [`MediaError::DecoderPanic`] when the reader
/// panics. Every one leaves the rung to the next source.
#[tracing::instrument(level = "debug", skip_all, fields(bytes = bytes.len(), ext))]
pub fn extract_embedded_preview(
    bytes: &[u8],
    ext: &str,
) -> Result<Option<EmbeddedPreview>, MediaError> {
    let format = gate(bytes, ext, FormatOp::Decode)?;
    if !format.is_raw() {
        return Ok(None);
    }
    guarded("embedded preview", || {
        embedded_preview_within(bytes, format, MAX_DECODE_PIXELS)
    })
}

/// [`extract_embedded_preview`] for a RAW `format`, with the budget as a parameter so a test can
/// exceed it with a small fixture.
pub(super) fn embedded_preview_within(
    bytes: &[u8],
    format: StillFormat,
    limit: u64,
) -> Result<Option<EmbeddedPreview>, MediaError> {
    let walk = check_ifd_bounds(bytes).map_err(|refusal| MediaError::Decode {
        format,
        detail: format!("RAW IFD bounds: {refusal}"),
    })?;
    let file_len = bytes.len() as u64;
    if walk.max_jpeg_length > file_len {
        tracing::warn!(
            %format,
            stated = walk.max_jpeg_length,
            file_len,
            "media: refused an embedded preview longer than its file"
        );
        return Err(MediaError::Decode {
            format,
            detail: format!(
                "embedded preview claims {} bytes, more than the {file_len}-byte file holds",
                walk.max_jpeg_length
            ),
        });
    }
    // A DNG's open decodes its raw image, so the preview extraction pays the same pre-open bound.
    check_eager_raw_frames(format, &walk, limit)?;
    let mut raw = RawFile::open(Cursor::new(bytes)).map_err(|e| raw_error(format, "open", e))?;
    let raw_orientation = raw_layout(&raw, format, walk.max_tile)
        .ok()
        .and_then(|layout| layout.orientation)
        .filter(|o| (1..=8).contains(o));
    let Some(jpeg) = raw
        .thumbnail()
        .map_err(|e| raw_error(format, "embedded preview", e))?
    else {
        tracing::debug!(%format, "media: the RAW carries no embedded preview");
        return Ok(None);
    };
    let not_jpeg = || MediaError::Decode {
        format,
        detail: "embedded preview is not a JPEG".to_owned(),
    };
    if StillFormat::from_bytes(&jpeg) != Some(StillFormat::Jpeg) {
        return Err(not_jpeg());
    }
    let probe = probe_standard_image(&jpeg).map_err(|e| MediaError::Decode {
        format,
        detail: format!("embedded preview header: {e}"),
    })?;
    let stored = (probe.size.width, probe.size.height);
    check_raw_frame(format, "embedded preview", stored, limit)?;
    let orientation = orientation_of(&jpeg, StillFormat::Jpeg)
        .or(raw_orientation)
        .unwrap_or(ORIENTATION_NORMAL);
    let (width, height) = match orientation {
        5..=8 => (stored.1, stored.0),
        _ => stored,
    };
    tracing::debug!(
        %format,
        bytes = jpeg.len(),
        width,
        height,
        orientation,
        "media: extracted an embedded preview"
    );
    Ok(Some(EmbeddedPreview {
        jpeg,
        width,
        height,
        orientation,
    }))
}

/// Decode an [`EmbeddedPreview`] to upright, packed RGBA8, behind the unwind guard.
///
/// Its own orientation is the one applied — the RAW's where the JPEG carries none — never the
/// JPEG's tag on top of it.
///
/// # Errors
///
/// As [`Decoder::decode`] for a JPEG.
#[tracing::instrument(level = "debug", skip_all, fields(bytes = preview.jpeg.len()))]
pub fn decode_embedded_preview(preview: &EmbeddedPreview) -> Result<DecodedImage, MediaError> {
    guarded("embedded preview decode", || {
        let format = StillFormat::Jpeg;
        let probed = RawshiftDecoder.probe(&preview.jpeg, "jpg")?;
        if probed.format != format {
            return Err(MediaError::Decode {
                format: probed.format,
                detail: "embedded preview is not a JPEG".to_owned(),
            });
        }
        let mut rgb = decode_standard_image(&preview.jpeg, StandardFormat::Jpeg).map_err(|e| {
            MediaError::Decode {
                format,
                detail: e.to_string(),
            }
        })?;
        check_rgb_buffer(&rgb, format)?;
        if preview.orientation != ORIENTATION_NORMAL {
            apply_orientation(&mut rgb, preview.orientation);
            check_rgb_buffer(&rgb, format)?;
        }
        Ok(DecodedImage {
            image: to_rgba8(&rgb),
            gamut: probed.gamut,
            orientation_applied: preview.orientation,
            format,
        })
    })
}

/// Map Capsule's format onto the crate's standard-image formats.
///
/// Only the decodable set reaches here through [`gate`], which refuses the rest first, and the
/// decodable RAW families are routed to [`decode_raw`] before it — of the grouped arms below,
/// `Tiff` is the sole reachable one. The unreachable variants are still mapped to the container
/// the crate would actually see (a TIFF-based RAW *is* a TIFF to it, and a CR3 is an ISO-BMFF
/// file it can only reach through its HEIC arm) rather than panicking, so a future routing or
/// `is_decodable` change that forgets this table degrades to a decode error instead of aborting
/// an import.
fn standard_format(format: StillFormat) -> StandardFormat {
    match format {
        StillFormat::Jpeg => StandardFormat::Jpeg,
        StillFormat::Png => StandardFormat::Png,
        StillFormat::WebP => StandardFormat::WebP,
        StillFormat::Jxl => StandardFormat::Jxl,
        StillFormat::Gif => StandardFormat::Gif,
        StillFormat::Ppm => StandardFormat::Ppm,
        StillFormat::Avif => StandardFormat::Avif,
        // `Tiff` is reachable and is the reason this arm exists; the RAW families ride along
        // because a TIFF-based RAW is a TIFF to the crate.
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

/// Map the crate's colour description onto the gamut the LQIP encoder takes.
///
/// Only the CICP primaries matter: the transfer function names no gamut. BT.709 (sRGB and linear
/// sRGB alike) and unspecified primaries both become [`Gamut::Srgb`], since sRGB primaries are
/// the only safe assumption for an untagged source (over-saturating is worse than
/// under-saturating). Adobe RGB and ProPhoto RGB have no CICP code point, so upstream tags them
/// unspecified and leaves their truth in the ICC profile; they map to sRGB here too.
///
/// **The wide-gamut arms are unreachable today**, because `probe_standard_image` hard-codes
/// `ColorDescription::SRGB` for every format (see [`MediaMetadata::gamut`]). They are here as the
/// one place that has to change when the crate starts reporting a source gamut, rather than as a
/// claim that it already does.
fn gamut_of(color: ColorDescription) -> Gamut {
    match color.primaries {
        ColourPrimaries::DisplayP3 => Gamut::DisplayP3,
        ColourPrimaries::Bt2020 => Gamut::Bt2020,
        // BT.709, unspecified, and — because `ColourPrimaries` is `#[non_exhaustive]` — any code
        // point a future release adds. One wildcard, since the answer is the same: sRGB is the
        // conservative default.
        _ => Gamut::Srgb,
    }
}

/// Refuse a decoder result whose buffer does not match the dimensions it reports.
///
/// `RgbImage` now validates its length against its dimensions at every construction (it wraps
/// gamut's `ImageBuf`), so this cannot fire against the pinned revision. It stays because the
/// very next thing Capsule does is index that buffer by those dimensions, and a pre-1.0
/// dependency's invariant is cheaper to recheck than to trust across every bump of the pin.
fn check_rgb_buffer(rgb: &RgbImage, format: StillFormat) -> Result<(), MediaError> {
    let (width, height) = (rgb.width(), rgb.height());
    if width == 0 || height == 0 {
        return Err(MediaError::ZeroDimension { width, height });
    }
    let expected = u128::from(width) * u128::from(height) * 3;
    let actual = rgb.data().len() as u128;
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
    let mut rgba = Vec::with_capacity(rgb.data().len() / 3 * 4);
    for px in rgb.data().chunks_exact(3) {
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
