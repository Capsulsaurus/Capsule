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
//!    `ispe` box, a claim the AV1 frame need not honour, so with `media-avif-decode` decode
//!    also bounds the real AV1 frames through dav1d's own `frame_size_limit` before
//!    `rawshift-image` decodes them.
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
//! - **Budget.** `RawFile::open` parses the IFDs and reads no pixels, so the probe *is* the
//!   open. The budget is checked against every frame the pipeline will allocate from the file's
//!   own claims — the sensor, the active area, the default crop and each tile — and every strip
//!   and tile byte range must lie inside the file, because the reader allocates each one at the
//!   length the file states before it reads a byte. The work is bounded as well as each
//!   allocation: a tile or strip must fit inside the sensor, the file may list no more of them
//!   than the sensor's grid holds, and a JPEG XL tile (ProRAW), which states its own size in
//!   its codestream header, is probed and must fit inside the tile or strip it fills. Without
//!   those, a few-KB file listing many entries at one compressible codestream would make the
//!   reader run that many full-budget decodes. All of that reads what `open` has already
//!   parsed, and `open` itself allocates each IFD entry's value at the count the entry claims,
//!   so before it runs, a no-allocation walk of the IFD chain (`tiff_bounds`, issue #644)
//!   refuses as [`MediaError::Decode`] any entry whose value needs more bytes than the file
//!   holds, and (issue #647) any IFD nested deeper than `open`'s recursion can safely follow.
//! - **Orientation** is applied by `process`, so it is reported in `orientation_applied` and
//!   never applied a second time here.
//! - **Unsupported variants defer.** Upstream decodes Sony lossless-JPEG ARW and uncompressed or
//!   JPEG XL DNG. Any other compression, a camera its reader does not place, or a TIFF that is not
//!   a RAW at all is `RawError::Unsupported`, and that is mapped to
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

use rawshift_image::core::ColorSpace;
use rawshift_image::core::image::RgbImage;
use rawshift_image::error::RawError;
use rawshift_image::formats::{
    RawFile, StandardFormat, decode_standard_image, probe_standard_image,
    read_standard_image_metadata,
};
use rawshift_image::processing::{BayerAlgorithm, DemosaicMethod, ProcessingOptions};
use rawshift_image::transforms::orientation::apply_orientation;

use super::detect::{MAX_DECODE_PIXELS, StillFormat};
use super::error::{FormatOp, MediaError};
use super::tiff_bounds::{IfdRefusal, MAX_IFD_DEPTH, check_ifd_bounds};
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
    /// choice made here.** `probe_standard_image` hard-codes `ColorSpace::Srgb` into every
    /// `ImageProbe` it returns, and `decode_standard_image` likewise tags every decoded frame
    /// sRGB without converting — so `rawshift-image` 0.1.1 reports no source gamut for a
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
        // The probe's AVIF size is the `ispe` box, which the file states and nothing checks
        // against the AV1 frame `image` then decodes and allocates for. Bound the real frames.
        #[cfg(feature = "media-avif-decode")]
        if format == StillFormat::Avif {
            check_avif_frames(bytes, MAX_DECODE_PIXELS)?;
        }
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
    guarded("decode", || decoder.decode(bytes, ext))
}

/// Run any fallible step of the still pipeline behind the same unwind boundary.
///
/// Public because its one caller, the derivative pipeline in `capsule-library`'s `lifecycle`, is
/// in another crate; the `catch_unwind` itself stays here, the one module the stateless core
/// sanctions it in.
///
/// Not just for `decode` — that is not the only third-party code the import path runs over pixels:
/// the placeholder goes through `chromahash` (also pre-1.0) and the derivative through
/// `zune-jpegxl`, and the module's promise is that *none* of them can abort an import — not that the
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

/// `DAV1D_ERR(ERANGE)`: what dav1d returns when a frame header exceeds `frame_size_limit`.
/// `ERANGE` is 34 on every host `media-avif-decode` builds for (Linux and macOS; Windows is
/// excluded), and `dav1d` maps no named variant onto it.
#[cfg(feature = "media-avif-decode")]
const DAV1D_FRAME_TOO_LARGE: dav1d::Error = dav1d::Error::UnknownError(-34);

/// Refuse an AVIF any of whose AV1 frames holds more than `limit` pixels, before
/// `rawshift-image` decodes it.
///
/// The header probe reads the `ispe` box, which is the file's own claim. `image`'s
/// `AvifDecoder` then runs libdav1d with no `frame_size_limit` and allocates its output from the
/// real frame size, so an AVIF whose `ispe` understates its frame would reach an allocation
/// failure, which aborts rather than unwinds and so is beyond [`decode_guarded`]. This decodes
/// the same items `image` will (`mp4parse` at the same strictness, primary and alpha) with
/// dav1d's own `frame_size_limit` set, which refuses an oversized frame from its header, before
/// dav1d allocates the picture; every frame it does produce is checked again by its decoded size.
///
/// Three decisions this function embodies, each recorded in #629's `## Decisions taken`:
///
/// - **A full second decode is the bound.** Every AV1 item of every in-budget AVIF is decoded
///   here and again by `image`, so its AV1 decode CPU roughly doubles (unmeasured). The
///   alternative, refusing from a hand-written parse of the AV1 sequence and frame headers, would
///   be a second parser of a format dav1d already parses, and a weaker bound: frame-size
///   override caps a frame only by `2^frame_width_bits`. Every frame is drained rather than the
///   first only, because `image` reads them all. Each picture is dropped before the next, so the
///   two decodes never hold frames at once.
/// - **`dav1d` and `mp4parse` are direct dependencies**, pinned to the versions `image` resolves,
///   because neither `rawshift-image` nor `image` exposes a frame-size limit. A future `image`
///   bump that moves either major version leaves two copies until these pins follow it.
/// - **A header refusal reports `pixels: limit + 1`**, a lower bound, because dav1d's
///   `DAV1D_ERR(ERANGE)` carries no frame size; see [`MediaError::PixelBudgetExceeded`]. A frame
///   that decodes over budget reports its real size.
///
/// Delete this pre-flight, and the two `dep:` entries on `media-avif-decode`, once
/// `rawshift-image` passes a frame-size limit to dav1d itself.
///
/// `pub(super)` so the tests can drive it with a budget a small fixture can exceed.
#[cfg(feature = "media-avif-decode")]
pub(super) fn check_avif_frames(bytes: &[u8], limit: u64) -> Result<(), MediaError> {
    let context = mp4parse::read_avif(
        &mut std::io::Cursor::new(bytes),
        mp4parse::ParseStrictness::Normal,
    )
    .map_err(|e| MediaError::Decode {
        format: StillFormat::Avif,
        detail: format!("AVIF container: {e}"),
    })?;
    for (item, coded) in [
        ("primary", context.primary_item_coded_data()),
        ("alpha", context.alpha_item_coded_data()),
    ] {
        if let Some(coded) = coded.filter(|coded| !coded.is_empty()) {
            check_av1_frames(item, coded, limit)?;
        }
    }
    Ok(())
}

/// Decode one AV1 item with `frame_size_limit = limit`, draining every frame it holds.
#[cfg(feature = "media-avif-decode")]
fn check_av1_frames(item: &'static str, coded: &[u8], limit: u64) -> Result<(), MediaError> {
    let refused = |e: dav1d::Error| {
        if e == DAV1D_FRAME_TOO_LARGE {
            tracing::warn!(
                item,
                limit,
                "media: refusing an AVIF whose AV1 frame exceeds the decode budget"
            );
            // dav1d refuses from the frame header without reporting its size; the frame is
            // known only to hold more than `limit` pixels.
            MediaError::PixelBudgetExceeded {
                pixels: limit.saturating_add(1),
                limit,
            }
        } else {
            MediaError::Decode {
                format: StillFormat::Avif,
                detail: format!("AV1 {item} item: {e}"),
            }
        }
    };
    let mut settings = dav1d::Settings::new();
    // One thread and no frame delay, so every frame is decoded, and checked, in order here.
    settings.set_n_threads(1);
    settings.set_max_frame_delay(1);
    settings.set_frame_size_limit(u32::try_from(limit).unwrap_or(u32::MAX));
    let mut decoder = dav1d::Decoder::with_settings(&settings).map_err(refused)?;

    let mut pending = match decoder.send_data(coded.to_vec(), None, None, None) {
        Ok(()) => false,
        Err(dav1d::Error::Again) => true,
        Err(e) => return Err(refused(e)),
    };
    let mut frames = 0_u32;
    loop {
        match decoder.get_picture() {
            Ok(picture) => {
                frames += 1;
                let pixels = u64::from(picture.width()) * u64::from(picture.height());
                if pixels > limit {
                    tracing::warn!(
                        item,
                        width = picture.width(),
                        height = picture.height(),
                        pixels,
                        limit,
                        "media: refusing an AVIF whose decoded AV1 frame exceeds the decode budget"
                    );
                    return Err(MediaError::PixelBudgetExceeded { pixels, limit });
                }
                continue;
            }
            Err(dav1d::Error::Again) => {}
            Err(e) => return Err(refused(e)),
        }
        if !pending {
            break;
        }
        pending = match decoder.send_pending_data() {
            Ok(()) => false,
            Err(dav1d::Error::Again) => true,
            Err(e) => return Err(refused(e)),
        };
    }
    if frames == 0 {
        // `image` reads an item until dav1d yields a picture, so an item that yields none would
        // spin there rather than fail.
        return Err(MediaError::Decode {
            format: StillFormat::Avif,
            detail: format!("AV1 {item} item holds no frame"),
        });
    }
    tracing::debug!(
        item,
        frames,
        limit,
        "media: AVIF frames are within the decode budget"
    );
    Ok(())
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
    /// stated length before it reads.
    ranges: Vec<(u64, u64)>,
    /// Whether each range is a JPEG XL codestream (DNG compression 52546, i.e. ProRAW).
    jxl: bool,
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
fn raw_layout(raw: &OpenRaw<'_>, format: StillFormat) -> Result<RawLayout, MediaError> {
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
                jxl: false,
                bit_depth: m.bit_depth,
                orientation: m.orientation,
            }
        }
        RawFile::Dng(dng) => {
            let m = dng.metadata().ok_or_else(missing)?;
            let tiled = !m.tile_offsets.is_empty();
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
                // Upstream reads a zero `RowsPerStrip` as one strip of the whole sensor, and
                // clips a larger one to the sensor's height.
                unit_size: if tiled {
                    (m.tile_width, m.tile_height)
                } else if m.rows_per_strip == 0 {
                    (sensor_w, sensor_h)
                } else {
                    (sensor_w, m.rows_per_strip.min(sensor_h))
                },
                unit: if tiled { "tile" } else { "strip" },
                ranges: if tiled {
                    pairs(&m.tile_offsets, &m.tile_byte_counts)
                } else {
                    pairs(&m.strip_offsets, &m.strip_byte_counts)
                },
                jxl: m.compression == DNG_COMPRESSION_JXL,
                bit_depth: m.bit_depth,
                orientation: m.orientation,
            }
        }
    })
}

/// DNG's `Compression` value for JPEG XL (DNG 1.7), which ProRAW uses.
const DNG_COMPRESSION_JXL: u16 = 52546;

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
/// the sensor, there may be no more of them than the sensor's grid of that size holds, and a
/// JPEG XL codestream must fit the strip or tile it fills.
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
    if layout.ranges.is_empty() {
        return Err(MediaError::Decode {
            format,
            detail: "RAW file locates no strip or tile data".to_owned(),
        });
    }
    let file_len = bytes.len() as u64;
    for (index, &(offset, length)) in layout.ranges.iter().enumerate() {
        let Some(end) = offset.checked_add(length).filter(|end| *end <= file_len) else {
            return Err(MediaError::Decode {
                format,
                detail: format!(
                    "{unit} {index} ({length} bytes at {offset}) runs past the {file_len}-byte file"
                ),
            });
        };
        if layout.jxl {
            // In range, so these casts cannot truncate: both are at most `bytes.len()`.
            let codestream = &bytes[offset as usize..end as usize];
            let probe = probe_standard_image(codestream).map_err(|e| MediaError::Decode {
                format,
                detail: format!("JPEG XL {unit} {index} header: {e}"),
            })?;
            // The tile decoder allocates from this header, not from the declared size, so a
            // codestream larger than its slot is refused. Fitting the slot also holds it to the
            // budget: the slot fits the sensor, which is already within `limit`.
            let (jxl_w, jxl_h) = (probe.size.width, probe.size.height);
            if jxl_w > unit_w || jxl_h > unit_h {
                tracing::warn!(
                    %format,
                    index,
                    jxl_w,
                    jxl_h,
                    unit_w,
                    unit_h,
                    "media: refusing a JPEG XL codestream larger than its declared slot"
                );
                return Err(MediaError::Decode {
                    format,
                    detail: format!(
                        "JPEG XL {unit} {index} codestream is {jxl_w}x{jxl_h}, larger than \
                         the declared {unit_w}x{unit_h} {unit}"
                    ),
                });
            }
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
    let raw = RawFile::open(Cursor::new(bytes)).map_err(|e| raw_error(format, "open", e))?;
    let layout = raw_layout(&raw, format)?;
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
        jxl = layout.jxl,
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

/// Map the crate's colour space onto the gamut the LQIP encoder takes.
///
/// `LinearSrgb` and `Unknown` both become [`Gamut::Srgb`]: `Linear` names a transfer function
/// rather than a gamut, and sRGB primaries are the only safe assumption for an untagged source
/// (over-saturating is worse than under-saturating).
///
/// **The wide-gamut arms are unreachable today**, because `probe_standard_image` hard-codes
/// `ColorSpace::Srgb` for every format (see [`MediaMetadata::gamut`]). They are here as the one
/// place that has to change when the crate starts reporting a source gamut, rather than as a
/// claim that it already does.
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
