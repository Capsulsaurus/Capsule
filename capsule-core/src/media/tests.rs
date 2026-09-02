//! Unit coverage for the still pipeline (slices `S-B1`, `S-B13`).
//!
//! # Fixtures are built, never committed
//!
//! The repository carries no binary image fixtures and this suite adds none. Three kinds of
//! input appear below, and the mix is deliberate:
//!
//! 1. **Procedural frames** ([`quadrants`], [`gradient`]) encoded in-test by
//!    `rawshift-image`'s own JPEG/PNG/WebP encoders. Self-consistent by construction, which is
//!    exactly what makes them right for the *orientation* and *EXIF* cases: the crate writes the
//!    EXIF block and Capsule reads it back, so the fixture cannot drift from the parser.
//! 2. **A hand-built PNG** ([`hand_written_png`]) — IHDR/IDAT/IEND assembled byte by byte with
//!    a local CRC-32 and an uncompressed-deflate zlib stream, so at least one decode case is
//!    fed an input no part of the code under test produced. Without it the suite could pass
//!    against an encoder and decoder that agree with each other and with nothing else.
//! 3. **Bare magic-byte headers** for the formats this build cannot decode. There is nothing to
//!    decode there and nothing to fake: the assertion is that they are *recognised* and refused
//!    with a typed error.

use std::collections::HashMap;

use rawshift_image::core::metadata::{ImageInfo, ImageMetadata, URational};
use rawshift_image::core::{BitDepth, MetadataEmbedOptions};
use rawshift_image::formats::encode_rgb_image_to_vec;
use rawshift_image::formats::export::{
    CommonEncodeOptions, EncodeOptions, JpegEncEncodeConfig, ZuneJxlEncodeConfig,
    ZunePngEncodeConfig,
};
use uuid::Uuid;

use super::decode::{Decoder, RawshiftDecoder, decode_guarded};
use super::derivative::{
    DerivativeContext, DerivativeSealer, DerivativeTier, SealedDerivative, StillDerivatives,
    generate_still_derivatives,
};
use super::detect::{MAX_DECODE_PIXELS, SUPPORTED_STILL_FORMATS, StillFormat};
use super::error::{FormatOp, MediaError};
use super::resize::{capped_dimensions, downscale_rgba8};
use crate::crypto::encryption::encrypt_asset_rekey;
use crate::crypto::hash::Hash32;
use crate::crypto::keys::{Amk, AmkVersion, HybridSigningKey};
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::crypto::provenance::manifest::{DERIVATIVE_MANIFEST_VERSION, DerivativeCore};
use crate::crypto::provenance::{DerivativeManifest, DerivativeRole};
use crate::derivative_format::{DerivativeFormat, verify_still_format};
use crate::lqip::{Gamut, Lqip, RgbaImage};

// ── Procedural fixtures ──────────────────────────────────────────────────────

/// A frame of four flat quadrants at distinct luminances — TL 0, TR 85, BL 170, BR 255.
///
/// Flat regions rather than a gradient because these fixtures go through a *lossy* JPEG:
/// sampling the middle of a flat quadrant is stable to within a couple of levels at q=90, while
/// a gradient's corner is not. Four distinct values make all eight EXIF orientations
/// distinguishable from one another.
fn quadrants(width: u32, height: u32) -> RgbaImage {
    let (w, h) = (width as usize, height as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let v = match (x < w / 2, y < h / 2) {
                (true, true) => 0,
                (false, true) => 85,
                (true, false) => 170,
                (false, false) => 255,
            };
            rgba.extend_from_slice(&[v, v, v, 255]);
        }
    }
    RgbaImage {
        width,
        height,
        rgba,
    }
}

/// A deterministic RGB gradient — the general-purpose frame for size and encode cases.
fn gradient(width: u32, height: u32) -> RgbaImage {
    let (w, h) = (width as usize, height as usize);
    let mut rgba = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            rgba.extend_from_slice(&[
                (x * 255 / w.max(1)) as u8,
                (y * 255 / h.max(1)) as u8,
                ((x + y) * 255 / (w + h).max(1)) as u8,
                255,
            ]);
        }
    }
    RgbaImage {
        width,
        height,
        rgba,
    }
}

/// Mean of each quadrant's inner half, as `(TL, TR, BL, BR)`.
///
/// The inner half avoids the quadrant boundaries, where JPEG's 8x8 blocks and chroma
/// subsampling smear one region into the next.
fn quadrant_means(image: &RgbaImage) -> (u32, u32, u32, u32) {
    let (w, h) = (image.width as usize, image.height as usize);
    let mean = |xs: std::ops::Range<usize>, ys: std::ops::Range<usize>| -> u32 {
        let mut sum = 0u64;
        let mut n = 0u64;
        for y in ys.clone() {
            for x in xs.clone() {
                sum += u64::from(image.rgba[(y * w + x) * 4]);
                n += 1;
            }
        }
        (sum / n.max(1)) as u32
    };
    let (qw, qh) = (w / 2, h / 2);
    let (ix, iy) = (qw / 4, qh / 4);
    (
        mean(ix..qw - ix, iy..qh - iy),
        mean(qw + ix..w - ix, iy..qh - iy),
        mean(ix..qw - ix, qh + iy..h - iy),
        mean(qw + ix..w - ix, qh + iy..h - iy),
    )
}

/// Widen packed RGBA8 into the interleaved RGB `u16` the encoders take.
fn to_rgb_u16(frame: &RgbaImage) -> rawshift_image::core::image::RgbImage {
    let mut data = Vec::with_capacity(frame.rgba.len() / 4 * 3);
    for px in frame.rgba.chunks_exact(4) {
        data.push(u16::from(px[0]) * 257);
        data.push(u16::from(px[1]) * 257);
        data.push(u16::from(px[2]) * 257);
    }
    rawshift_image::core::image::RgbImage::with_color_space(
        frame.width,
        frame.height,
        data,
        rawshift_image::core::ColorSpace::Srgb,
    )
}

/// A `CommonEncodeOptions` embedding whatever `metadata` asks for, at 8 bits.
fn common(metadata: MetadataEmbedOptions) -> CommonEncodeOptions {
    CommonEncodeOptions {
        metadata,
        bit_depth: BitDepth::Eight,
    }
}

/// Encode a frame as a baseline JPEG, optionally embedding `metadata`.
///
/// Quality 90 rather than the tier's 50: this is a *source* fixture, and a decode test should
/// not have to absorb the tier's own quality budget as well.
fn jpeg_bytes(frame: &RgbaImage, metadata: Option<&ImageMetadata>) -> Vec<u8> {
    let embed = if metadata.is_some() {
        MetadataEmbedOptions {
            embed_exif: true,
            embed_icc: false,
            embed_xmp: false,
        }
    } else {
        MetadataEmbedOptions::none()
    };
    let options = EncodeOptions::JpegJpegEnc(JpegEncEncodeConfig {
        common: common(embed),
        quality: 90,
    });
    let empty = ImageMetadata::default();
    encode_rgb_image_to_vec(&to_rgb_u16(frame), metadata.unwrap_or(&empty), &options)
        .expect("the fixture JPEG encodes")
}

/// Encode a frame as a PNG with no metadata at all.
fn png_bytes(frame: &RgbaImage) -> Vec<u8> {
    let options = EncodeOptions::PngZune(ZunePngEncodeConfig {
        common: common(MetadataEmbedOptions::none()),
        ..ZunePngEncodeConfig::default()
    });
    encode_rgb_image_to_vec(&to_rgb_u16(frame), &ImageMetadata::default(), &options)
        .expect("the fixture PNG encodes")
}

/// A bare 12-byte RIFF/WEBP header.
///
/// A *header*, not an encode, because this build has no WebP codec at all: `rawshift-image`'s
/// WebP module does not compile for aarch64 (`*const i8` against `libwebp-sys`'s `*const
/// c_char`), so the crate's `webp` feature is off in both directions. Twelve bytes is all a
/// detection case needs, and there is nothing to decode.
fn webp_header() -> Vec<u8> {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&0u32.to_le_bytes());
    bytes.extend_from_slice(b"WEBP");
    bytes
}

/// Encode a frame as JXL through the same backend the derivative path uses, optionally
/// embedding `metadata`.
fn jxl_bytes(frame: &RgbaImage, metadata: Option<&ImageMetadata>) -> Vec<u8> {
    let embed = if metadata.is_some() {
        MetadataEmbedOptions::all()
    } else {
        MetadataEmbedOptions::none()
    };
    let options = EncodeOptions::JxlZune(ZuneJxlEncodeConfig {
        common: common(embed),
        quality: 50.0,
        effort: 7,
    });
    let empty = ImageMetadata::default();
    encode_rgb_image_to_vec(&to_rgb_u16(frame), metadata.unwrap_or(&empty), &options)
        .expect("the fixture JXL encodes")
}

/// An `ImageMetadata` carrying only an EXIF orientation tag.
fn oriented(orientation: u16) -> ImageMetadata {
    ImageMetadata {
        image: ImageInfo {
            orientation: Some(orientation),
            ..ImageInfo::default()
        },
        ..ImageMetadata::default()
    }
}

/// The GPS degrees/minutes/seconds triple used by the privacy cases — a distinctive fix whose
/// rationals are searchable as raw bytes.
const GPS_LAT_DMS: [u32; 3] = [51, 30, 26];
const GPS_LON_DMS: [u32; 3] = [0, 7, 39];

/// An `ImageMetadata` carrying a GPS fix — the metadata a thumbnail must never inherit.
fn located() -> ImageMetadata {
    let dms = |v: [u32; 3]| {
        v.map(|n| URational {
            numerator: n,
            denominator: 1,
        })
    };
    let mut metadata = ImageMetadata::default();
    metadata.gps.latitude = Some(dms(GPS_LAT_DMS));
    metadata.gps.latitude_ref = Some('N');
    metadata.gps.longitude = Some(dms(GPS_LON_DMS));
    metadata.gps.longitude_ref = Some('W');
    metadata
}

// ── The independently-constructed PNG ────────────────────────────────────────

/// CRC-32 (IEEE 802.3), computed here so the PNG fixture owes nothing to a dependency.
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Adler-32, the zlib stream checksum.
fn adler32(bytes: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &byte in bytes {
        a = (a + u32::from(byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}

/// One PNG chunk: length, type, payload, CRC over type+payload.
fn png_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut chunk = Vec::with_capacity(payload.len() + 12);
    chunk.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    chunk.extend_from_slice(kind);
    chunk.extend_from_slice(payload);
    let mut crc_input = kind.to_vec();
    crc_input.extend_from_slice(payload);
    chunk.extend_from_slice(&crc32(&crc_input).to_be_bytes());
    chunk
}

/// A 2x2 8-bit RGB PNG built byte by byte: signature, IHDR, a stored-deflate IDAT, IEND.
///
/// `deflate` here is a single final *stored* block (BTYPE 00), so no compressor is involved —
/// the point of this fixture is that nothing under test, and no encoder it shares code with,
/// produced it.
fn hand_written_png() -> Vec<u8> {
    // Four pixels: red, green, blue, white — each row prefixed by filter type 0 (None).
    let raw: Vec<u8> = vec![
        0, 255, 0, 0, 0, 255, 0, // row 0: filter, red, green
        0, 0, 0, 255, 255, 255, 255, // row 1: filter, blue, white
    ];

    let mut zlib = vec![0x78, 0x01]; // CM=8, CINFO=7, FLEVEL=0, FCHECK making it a multiple of 31
    zlib.push(0x01); // final stored block
    zlib.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    zlib.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    zlib.extend_from_slice(&raw);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&2u32.to_be_bytes()); // width
    ihdr.extend_from_slice(&2u32.to_be_bytes()); // height
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]); // 8-bit, colour type 2 (RGB), deflate, no filter/interlace

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(png_chunk(b"IHDR", &ihdr));
    png.extend(png_chunk(b"IDAT", &zlib));
    png.extend(png_chunk(b"IEND", &[]));
    png
}

// ── Detection ────────────────────────────────────────────────────────────────

/// A 12-byte header for each format Capsule recognises but cannot decode. Twelve bytes is
/// exactly what [`StillFormat::from_bytes`] requires, so these also pin the minimum length.
fn isobmff(brand: &[u8; 4]) -> Vec<u8> {
    let mut bytes = vec![0, 0, 0, 0x20];
    bytes.extend_from_slice(b"ftyp");
    bytes.extend_from_slice(brand);
    bytes
}

/// Real encodes on one side, bare headers on the other: every variant of the closed set is
/// reachable from bytes, and the sniff names the right one.
#[test]
fn every_still_format_is_reachable_from_its_header() {
    let frame = gradient(8, 8);
    let cases: &[(Vec<u8>, &str, StillFormat)] = &[
        (jpeg_bytes(&frame, None), "jpg", StillFormat::Jpeg),
        (png_bytes(&frame), "png", StillFormat::Png),
        (webp_header(), "webp", StillFormat::WebP),
        (hand_written_png(), "png", StillFormat::Png),
        (
            b"GIF89a\x08\x00\x08\x00\x00\x00".to_vec(),
            "gif",
            StillFormat::Gif,
        ),
        (
            b"\xFF\x0A\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec(),
            "jxl",
            StillFormat::Jxl,
        ),
        (
            b"II\x2A\x00\x08\x00\x00\x00\x00\x00\x00\x00".to_vec(),
            "tif",
            StillFormat::Tiff,
        ),
        (
            b"MM\x00\x2A\x00\x00\x00\x08\x00\x00\x00\x00".to_vec(),
            "tiff",
            StillFormat::Tiff,
        ),
        (b"P6\n8 8\n255\n".to_vec(), "ppm", StillFormat::Ppm),
        (isobmff(b"avif"), "avif", StillFormat::Avif),
        (webp_header(), "webp", StillFormat::WebP),
        (isobmff(b"heic"), "heic", StillFormat::Heic),
        (isobmff(b"mif1"), "heic", StillFormat::Heic),
        (isobmff(b"crx "), "cr3", StillFormat::Cr3),
        // TIFF-container RAW: the header says TIFF and only the extension names the family.
        (
            b"II\x2A\x00\x08\x00\x00\x00\x00\x00\x00\x00".to_vec(),
            "arw",
            StillFormat::Arw,
        ),
        (
            b"II\x2A\x00\x08\x00\x00\x00\x00\x00\x00\x00".to_vec(),
            "cr2",
            StillFormat::Cr2,
        ),
        (
            b"II\x2A\x00\x08\x00\x00\x00\x00\x00\x00\x00".to_vec(),
            "dng",
            StillFormat::Dng,
        ),
        (
            b"MM\x00\x2A\x00\x00\x00\x08\x00\x00\x00\x00".to_vec(),
            "nef",
            StillFormat::Nef,
        ),
    ];
    for (bytes, ext, expected) in cases {
        assert_eq!(
            StillFormat::detect(bytes, ext),
            Some(*expected),
            "detecting a .{ext} fixture"
        );
    }

    // CRW and RAF have no in-tree header fixture; they are extension-only entries, and the
    // point of asserting them is that the fallback table covers the whole RAW set.
    assert_eq!(StillFormat::detect(b"", "crw"), Some(StillFormat::Crw));
    assert_eq!(StillFormat::detect(b"", "raf"), Some(StillFormat::Raf));
}

/// Bytes win over the extension. A HEIC named `.jpg` must not be handed to the JPEG decoder —
/// that is the difference between a typed "no codec for HEIC" deferral and a decode failure
/// blamed on JPEG.
#[test]
fn the_header_beats_a_lying_extension() {
    assert_eq!(
        StillFormat::detect(&isobmff(b"heic"), "jpg"),
        Some(StillFormat::Heic)
    );
    let png = png_bytes(&gradient(4, 4));
    assert_eq!(StillFormat::detect(&png, "jpeg"), Some(StillFormat::Png));

    // The one refinement that runs the other way is *into* a RAW family, and only from a TIFF
    // header — a real `.tif` stays TIFF.
    let tiff_header = b"II\x2A\x00\x08\x00\x00\x00\x00\x00\x00\x00";
    assert_eq!(
        StillFormat::detect(tiff_header, "tif"),
        Some(StillFormat::Tiff)
    );
}

/// Nothing recognisable, and never a panic. `detect` is the first thing untrusted bytes touch.
#[test]
fn unrecognisable_bytes_are_not_a_still() {
    let cases: &[&[u8]] = &[
        b"",
        b"\x00",
        b"\xFF\xD8",                    // a JPEG SOI truncated below the 12-byte floor
        b"noise-x!",                    // 8 bytes, under the floor
        b"not an image at all, really", // long enough, no signature
        b"\x00\x00\x00\x20ftypqt  ",    // ISO-BMFF with an unmodelled brand (QuickTime)
        b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>",
        b"\x1AE\xDF\xA3\x01\x00\x00\x00\x00\x00\x00\x23", // Matroska/WebM
    ];
    for bytes in cases {
        assert_eq!(
            StillFormat::detect(bytes, ""),
            None,
            "these bytes are not a still Capsule models: {:?}",
            &bytes[..bytes.len().min(12)]
        );
    }
}

/// The Capsule table and `rawshift-image`'s own `detect_standard_format` must not drift where
/// both define an answer.
///
/// Scoped to the formats whose signature is unconditional on both sides. The ISO-BMFF brands are
/// excluded on purpose: the crate's HEIC arm is feature-gated (so it cannot recognise HEIC in
/// this build — the reason Capsule sniffs at all), and it reads the generic `mif1` brand as AVIF
/// where Capsule reads it as HEIC. Neither decodes here, so that divergence is a log label, not
/// a pixel.
#[test]
fn still_format_agrees_with_rawshift_detection() {
    use rawshift_image::formats::{StandardFormat, detect_standard_format};

    let frame = gradient(8, 8);
    let cases: &[(Vec<u8>, StandardFormat, StillFormat)] = &[
        (
            jpeg_bytes(&frame, None),
            StandardFormat::Jpeg,
            StillFormat::Jpeg,
        ),
        (png_bytes(&frame), StandardFormat::Png, StillFormat::Png),
        (webp_header(), StandardFormat::WebP, StillFormat::WebP),
        (hand_written_png(), StandardFormat::Png, StillFormat::Png),
        (
            b"GIF89a\x08\x00\x08\x00\x00\x00".to_vec(),
            StandardFormat::Gif,
            StillFormat::Gif,
        ),
        (
            b"\xFF\x0A\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00".to_vec(),
            StandardFormat::Jxl,
            StillFormat::Jxl,
        ),
        (
            b"II\x2A\x00\x08\x00\x00\x00\x00\x00\x00\x00".to_vec(),
            StandardFormat::Tiff,
            StillFormat::Tiff,
        ),
        (
            b"P6\n8 8\n255\n".to_vec(),
            StandardFormat::Ppm,
            StillFormat::Ppm,
        ),
        // A JPEG SOI is three bytes and a Netpbm header eleven, so both sides recognise a file
        // far shorter than a blanket minimum-length floor would admit.
        (isobmff(b"avif"), StandardFormat::Avif, StillFormat::Avif),
    ];
    for (bytes, theirs, ours) in cases {
        assert_eq!(detect_standard_format(bytes), Some(*theirs));
        assert_eq!(StillFormat::from_bytes(bytes), Some(*ours));
    }
}

/// The coverage table is the single answer to "can this build read these pixels?", and it has
/// to agree with what the decoder actually does.
#[test]
fn is_decodable_matches_the_supported_table() {
    for format in SUPPORTED_STILL_FORMATS {
        assert!(format.is_decodable(), "{format} is in the supported table");
        assert!(!format.is_raw(), "no RAW family decodes in this build");
    }
    for format in [
        StillFormat::Ppm,
        StillFormat::WebP,
        StillFormat::Avif,
        StillFormat::Heic,
        StillFormat::Arw,
        StillFormat::Cr2,
        StillFormat::Cr3,
        StillFormat::Crw,
        StillFormat::Dng,
        StillFormat::Nef,
        StillFormat::Raf,
    ] {
        assert!(!format.is_decodable(), "{format} has no decoder here");
    }
    // Every mime is distinct, so a `content_type` cannot silently collide.
    let mut mimes: Vec<&str> = SUPPORTED_STILL_FORMATS.iter().map(|f| f.mime()).collect();
    mimes.sort_unstable();
    let count = mimes.len();
    mimes.dedup();
    assert_eq!(mimes.len(), count, "each format has its own media type");
}

// ── Decode ───────────────────────────────────────────────────────────────────

/// Every decodable format round-trips to the dimensions it was built with, with a full RGBA8
/// buffer and uniform opaque alpha.
#[test]
fn decodes_every_supported_container_to_opaque_rgba8() {
    let frame = gradient(6, 4);
    let cases: &[(Vec<u8>, &str, StillFormat, u32, u32)] = &[
        (jpeg_bytes(&frame, None), "jpg", StillFormat::Jpeg, 6, 4),
        (png_bytes(&frame), "png", StillFormat::Png, 6, 4),
        (jxl_bytes(&frame, None), "jxl", StillFormat::Jxl, 6, 4),
        (hand_written_png(), "png", StillFormat::Png, 2, 2),
    ];
    for (bytes, ext, format, width, height) in cases {
        let decoded = RawshiftDecoder
            .decode(bytes, ext)
            .unwrap_or_else(|e| panic!("decoding the .{ext} fixture: {e}"));
        assert_eq!(decoded.format, *format);
        assert_eq!((decoded.width(), decoded.height()), (*width, *height));
        assert_eq!(
            decoded.image.rgba.len() as u32,
            width * height * 4,
            "the buffer is exactly w*h*4"
        );
        assert!(
            decoded.image.rgba.chunks_exact(4).all(|px| px[3] == 255),
            "every decoded frame is opaque"
        );
        assert_eq!(decoded.orientation_applied, 1, "no tag, no transform");
    }
}

/// The hand-built PNG's actual pixels, checked against the bytes that were written into it.
///
/// This is the one place the suite is not self-consistent: the input owes nothing to the encoder
/// the other cases use, so agreement here is evidence about the decoder rather than about a
/// matched pair.
#[test]
fn the_hand_written_png_decodes_to_the_pixels_it_declares() {
    let decoded = RawshiftDecoder
        .decode(&hand_written_png(), "png")
        .expect("a hand-built 2x2 RGB PNG decodes");
    assert_eq!((decoded.width(), decoded.height()), (2, 2));
    assert_eq!(
        decoded.image.rgba,
        vec![
            255, 0, 0, 255, // red
            0, 255, 0, 255, // green
            0, 0, 255, 255, // blue
            255, 255, 255, 255, // white
        ]
    );
}

/// PNG alpha is **flattened**, not preserved — `rawshift-image` decodes to RGB with no alpha
/// channel. Asserted so the loss cannot regress into an "alpha survives" assumption somewhere
/// downstream.
#[test]
fn png_alpha_is_flattened_to_opaque() {
    // A 1x1 fully transparent RGBA PNG, hand-built for the same reason as the fixture above.
    let raw = vec![0u8, 0, 0, 0, 0]; // filter 0 + RGBA(0,0,0,0)
    let mut zlib = vec![0x78, 0x01, 0x01];
    zlib.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    zlib.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    zlib.extend_from_slice(&raw);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // colour type 6 = RGBA
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(png_chunk(b"IHDR", &ihdr));
    png.extend(png_chunk(b"IDAT", &zlib));
    png.extend(png_chunk(b"IEND", &[]));

    let decoded = RawshiftDecoder
        .decode(&png, "png")
        .expect("RGBA PNG decodes");
    assert_eq!(decoded.image.rgba, vec![0, 0, 0, 255], "alpha is dropped");
}

/// A recognised format with no codec refuses **before** any decoder runs, and says which half
/// of the codec is missing. This is the S-B13 contract at the seam.
#[test]
fn a_format_with_no_codec_refuses_with_a_typed_error() {
    let cases: &[(Vec<u8>, &str, StillFormat)] = &[
        (isobmff(b"heic"), "heic", StillFormat::Heic),
        (isobmff(b"avif"), "avif", StillFormat::Avif),
        (isobmff(b"crx "), "cr3", StillFormat::Cr3),
        (
            b"II\x2A\x00\x08\x00\x00\x00\x00\x00\x00\x00".to_vec(),
            "arw",
            StillFormat::Arw,
        ),
        (b"".to_vec(), "raf", StillFormat::Raf),
        (b"P6\n8 8\n255\n".to_vec(), "ppm", StillFormat::Ppm),
    ];
    for (bytes, ext, format) in cases {
        assert_eq!(
            RawshiftDecoder.decode(bytes, ext),
            Err(MediaError::UnsupportedFormat {
                format: *format,
                op: FormatOp::Decode,
            }),
            "a .{ext} must defer rather than fail"
        );
        assert_eq!(
            RawshiftDecoder.probe(bytes, ext),
            Err(MediaError::UnsupportedFormat {
                format: *format,
                op: FormatOp::Decode,
            }),
        );
    }
}

/// Bytes that are no still at all are a distinct outcome from a still with no codec: there is
/// nothing to backfill later.
#[test]
fn non_still_bytes_are_not_a_deferral() {
    assert_eq!(
        RawshiftDecoder.decode(b"\x1AE\xDF\xA3 a webm, not a photo", "webm"),
        Err(MediaError::NotAStillImage)
    );
}

/// A supported format whose bytes are broken is a **decode failure**, not a deferral — the
/// distinction the run summary reports and the one worth investigating.
#[test]
fn corrupt_bytes_of_a_supported_format_are_a_decode_failure() {
    let mut jpeg = jpeg_bytes(&gradient(16, 16), None);
    jpeg.truncate(jpeg.len() / 2);
    match RawshiftDecoder.decode(&jpeg, "jpg") {
        Err(MediaError::Decode { format, .. }) => assert_eq!(format, StillFormat::Jpeg),
        other => panic!("a truncated JPEG must be a decode failure, got {other:?}"),
    }

    // Not even a header: the extension is the only evidence, and it says a format we decode.
    match RawshiftDecoder.decode(b"this is definitely not a jpeg", "jpeg") {
        Err(MediaError::Decode { format, .. }) => assert_eq!(format, StillFormat::Jpeg),
        other => panic!("garbage under a .jpeg name must be a decode failure, got {other:?}"),
    }
}

/// The decode-bomb guard: a header claiming more than the budget is refused before the decoder
/// allocates. Built as a PNG header alone, because the *point* is that no pixel data is needed
/// to trigger it — a 33-byte file must not be able to ask for 21 GB.
#[test]
fn an_oversized_header_is_refused_before_decoding() {
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&30_000u32.to_be_bytes());
    ihdr.extend_from_slice(&30_000u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend(png_chunk(b"IHDR", &ihdr));

    assert_eq!(
        RawshiftDecoder.probe(&png, "png"),
        Err(MediaError::PixelBudgetExceeded {
            pixels: 900_000_000,
            limit: MAX_DECODE_PIXELS,
        }),
    );
    assert_eq!(
        RawshiftDecoder.decode(&png, "png"),
        Err(MediaError::PixelBudgetExceeded {
            pixels: 900_000_000,
            limit: MAX_DECODE_PIXELS,
        }),
        "decode probes first, so the guard covers it too"
    );
    // The budget is a ceiling on the frame, not on the file: a small image is unaffected.
    assert!(
        RawshiftDecoder
            .probe(&png_bytes(&gradient(4, 4)), "png")
            .is_ok()
    );
}

/// A probe reports the header's stored dimensions and the EXIF orientation without decoding,
/// and knows which pairs are transposed on display.
#[test]
fn a_probe_reports_stored_dimensions_and_the_upright_pair() {
    let frame = gradient(12, 6);
    let upright = RawshiftDecoder
        .probe(&jpeg_bytes(&frame, Some(&oriented(1))), "jpg")
        .expect("probe");
    assert_eq!(upright.stored_dimensions, (12, 6));
    assert_eq!(upright.upright_dimensions(), (12, 6));
    assert_eq!(upright.orientation, Some(1));
    assert_eq!(upright.gamut, Gamut::Srgb);

    let rotated = RawshiftDecoder
        .probe(&jpeg_bytes(&frame, Some(&oriented(6))), "jpg")
        .expect("probe");
    assert_eq!(rotated.stored_dimensions, (12, 6));
    assert_eq!(
        rotated.upright_dimensions(),
        (6, 12),
        "a quarter-turn transposes what a viewer shows"
    );
}

/// All eight EXIF orientations, as a permutation of four marked quadrants plus the dimension
/// pair. This is the table an upright frame depends on, and it is hand-derived from the EXIF
/// definitions rather than from the transform code.
#[test]
fn every_exif_orientation_lands_upright() {
    // (orientation, transposed?, (TL, TR, BL, BR) after the transform)
    let table: &[(u16, bool, (u32, u32, u32, u32))] = &[
        (1, false, (0, 85, 170, 255)), // identity
        (2, false, (85, 0, 255, 170)), // mirror horizontal
        (3, false, (255, 170, 85, 0)), // rotate 180
        (4, false, (170, 255, 0, 85)), // mirror vertical
        (5, true, (0, 170, 85, 255)),  // transpose
        (6, true, (170, 0, 255, 85)),  // rotate 90 CW
        (7, true, (255, 85, 170, 0)),  // transverse
        (8, true, (85, 255, 0, 170)),  // rotate 90 CCW
    ];
    let (width, height) = (64u32, 32u32);
    let frame = quadrants(width, height);

    for &(orientation, transposed, expected) in table {
        let bytes = jpeg_bytes(&frame, Some(&oriented(orientation)));
        let decoded = RawshiftDecoder
            .decode(&bytes, "jpg")
            .unwrap_or_else(|e| panic!("orientation {orientation}: {e}"));

        assert_eq!(
            decoded.orientation_applied, orientation,
            "the consumed tag is recorded so a renderer does not rotate again"
        );
        let expected_dims = if transposed {
            (height, width)
        } else {
            (width, height)
        };
        assert_eq!(
            (decoded.width(), decoded.height()),
            expected_dims,
            "orientation {orientation} dimensions"
        );

        let got = quadrant_means(&decoded.image);
        let close = |a: u32, b: u32| a.abs_diff(b) <= 20;
        assert!(
            close(got.0, expected.0)
                && close(got.1, expected.1)
                && close(got.2, expected.2)
                && close(got.3, expected.3),
            "orientation {orientation}: quadrants {got:?} are not {expected:?}"
        );
    }
}

/// An orientation value outside 1..=8 is dropped rather than recorded. `apply_orientation`
/// warns and no-ops on one, so honouring it would leave `orientation_applied` claiming a
/// transform that never happened.
#[test]
fn an_out_of_range_orientation_tag_is_ignored() {
    let bytes = jpeg_bytes(&gradient(8, 8), Some(&oriented(42)));
    let probed = RawshiftDecoder.probe(&bytes, "jpg").expect("probe");
    assert_eq!(probed.orientation, None);
    let decoded = RawshiftDecoder.decode(&bytes, "jpg").expect("decode");
    assert_eq!(decoded.orientation_applied, 1);
}

// ── The panic guard ──────────────────────────────────────────────────────────

/// A [`Decoder`] that panics, and one that lies about its buffer — the two failures real bytes
/// cannot be relied on to produce.
struct HostileDecoder {
    panic: bool,
}

impl Decoder for HostileDecoder {
    fn probe(&self, _bytes: &[u8], _ext: &str) -> Result<super::MediaMetadata, MediaError> {
        Err(MediaError::NotAStillImage)
    }

    fn decode(&self, _bytes: &[u8], _ext: &str) -> Result<super::DecodedImage, MediaError> {
        assert!(
            !self.panic,
            "a third-party decoder panicking on untrusted bytes"
        );
        Err(MediaError::NotAStillImage)
    }
}

/// A panicking decoder becomes a reported error, never an aborted import.
#[test]
fn a_panicking_decoder_is_caught_at_the_boundary() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught = decode_guarded(&HostileDecoder { panic: true }, b"whatever", "jpg");
    std::panic::set_hook(previous);
    assert_eq!(caught, Err(MediaError::DecoderPanic));

    // The guard is transparent when nothing panics.
    assert_eq!(
        decode_guarded(&HostileDecoder { panic: false }, b"whatever", "jpg"),
        Err(MediaError::NotAStillImage)
    );
    assert!(decode_guarded(&RawshiftDecoder, &png_bytes(&gradient(4, 4)), "png").is_ok());
}

// ── Resize ───────────────────────────────────────────────────────────────────

/// The sizing rule: cap the long edge, keep the aspect ratio, never return a zero edge, and
/// leave a frame that already fits exactly as it was.
#[test]
fn capped_dimensions_preserves_aspect_and_never_returns_zero() {
    assert_eq!(capped_dimensions(512, 384, 256), (256, 192));
    assert_eq!(capped_dimensions(384, 512, 256), (192, 256));
    assert_eq!(capped_dimensions(1000, 1000, 256), (256, 256));
    assert_eq!(capped_dimensions(4032, 3024, 256), (256, 192));
    // Already inside the cap: untouched, which is what lets a caller compare and skip.
    assert_eq!(capped_dimensions(128, 96, 256), (128, 96));
    assert_eq!(capped_dimensions(256, 256, 256), (256, 256));
    // Extreme ratios round the short edge to 1 rather than to an empty buffer.
    assert_eq!(capped_dimensions(8000, 3, 256), (256, 1));
    assert_eq!(capped_dimensions(3, 8000, 256), (1, 256));
    // A zero cap is raised to 1 rather than producing an empty frame.
    assert_eq!(capped_dimensions(100, 50, 0), (1, 1));
}

/// The downscale is deterministic and its output is exactly `w*h*4`. Determinism is a
/// requirement, not a nicety: the derivative's bytes are content-addressed by a signed manifest.
#[test]
fn the_downscale_is_deterministic_and_correctly_sized() {
    let source = gradient(512, 384);
    let first = downscale_rgba8(&source, 256);
    let second = downscale_rgba8(&source, 256);
    assert_eq!((first.width, first.height), (256, 192));
    assert_eq!(first.rgba.len(), 256 * 192 * 4);
    assert_eq!(first.rgba, second.rgba, "identical input, identical bytes");

    // A frame within the cap comes back untouched.
    let small = gradient(100, 80);
    assert_eq!(downscale_rgba8(&small, 256), small);
}

/// A box average over a flat region reproduces that region's own value exactly — the property
/// that keeps a downscaled thumbnail from drifting darker with every reduction.
#[test]
fn the_downscale_preserves_flat_regions_and_stays_opaque() {
    let uniform = RgbaImage {
        width: 64,
        height: 64,
        rgba: vec![137, 42, 200, 255].repeat(64 * 64),
    };
    let out = downscale_rgba8(&uniform, 16);
    assert_eq!((out.width, out.height), (16, 16));
    assert!(
        out.rgba.chunks_exact(4).all(|px| px == [137, 42, 200, 255]),
        "a flat region survives an area average exactly"
    );

    // The quadrant markers survive a 4x reduction, i.e. the filter is not smearing regions into
    // one another beyond their own boundary.
    let reduced = downscale_rgba8(&quadrants(128, 128), 32);
    let means = quadrant_means(&reduced);
    assert_eq!(means, (0, 85, 170, 255));
}

// ── Derivatives ──────────────────────────────────────────────────────────────

/// Two signing keys and a fixed context — the epoch/authorisation material a manifest needs
/// that pixels do not carry.
fn signers() -> (HybridSigningKey, HybridSigningKey) {
    (
        HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]),
        HybridSigningKey::from_seed_bytes(&[9; 32], &[10; 32]),
    )
}

/// A real AMK-backed sealer — the same `encrypt_asset_rekey` construction the import path uses,
/// so these tests exercise the production encryption rather than a stand-in.
struct TestSealer {
    amk: Amk,
    asset_id: Uuid,
}

impl DerivativeSealer for TestSealer {
    fn seal(&self, plaintext: &[u8]) -> Result<SealedDerivative, MediaError> {
        let (enc, _ciphertext, _key) =
            encrypt_asset_rekey(&self.amk, &self.asset_id, plaintext, None).expect("sealing");
        Ok(SealedDerivative {
            ciphertext_hash: enc.ciphertext_hash,
            nonce_prefix: enc.nonce_prefix,
        })
    }
}

fn sealer(asset_id: Uuid) -> TestSealer {
    TestSealer {
        amk: Amk::from_bytes([0x5A; 32]),
        asset_id,
    }
}

/// What the *original*'s own manifest committed to; the `original` sentinel signs exactly this.
const ORIGINAL_SEAL: SealedDerivative = SealedDerivative {
    ciphertext_hash: Hash32([0xC1; 32]),
    nonce_prefix: [1, 2, 3, 4, 5, 6, 7],
};

/// No prior chain: every test that does not say otherwise generates for a fresh asset.
fn no_prior_heads() -> HashMap<DerivativeRole, crate::crypto::hash::Hash32> {
    HashMap::new()
}

fn context<'a>(
    device: &'a HybridSigningKey,
    write_tier: &'a HybridSigningKey,
    sealer: &'a dyn DerivativeSealer,
    prior_heads: &'a HashMap<DerivativeRole, crate::crypto::hash::Hash32>,
    asset_id: Uuid,
) -> DerivativeContext<'a> {
    DerivativeContext {
        source_asset_id: asset_id,
        crypto_suite_id: CRYPTO_SUITE_ID,
        protocol_version: PROTOCOL_VERSION.into(),
        amk_version: AmkVersion(1),
        generated_by_device: Uuid::from_u128(0xD1),
        generated_by_client: "capsule-core/test".into(),
        generated_at: "2026-09-01T00:00:00Z".into(),
        device_signer: device,
        write_tier_signer: write_tier,
        sealer,
        prior_heads,
        original: ORIGINAL_SEAL,
    }
}

fn generate(frame: &RgbaImage, original: &[u8]) -> StillDerivatives {
    let (device, write_tier) = signers();
    let seal = sealer(Uuid::from_u128(0xB1));
    let heads = no_prior_heads();
    let ctx = context(&device, &write_tier, &seal, &heads, Uuid::from_u128(0xB1));
    let decoded = RawshiftDecoder
        .decode(original, "png")
        .expect("the fixture decodes");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (frame.width, frame.height)
    );
    generate_still_derivatives(&decoded, &DerivativeTier::GENERATED, &ctx)
        .expect("generation succeeds")
}

/// The thumbnail tier over a source larger than the cap: real JXL bytes, a signed manifest
/// binding their hash, and the two formats this build cannot encode recorded as deferrals rather
/// than silently omitted.
#[test]
fn the_thumbnail_tier_encodes_jxl_and_defers_the_rest() {
    let frame = gradient(512, 384);
    let original = png_bytes(&frame);
    let result = generate(&frame, &original);

    assert_eq!(result.generated.len(), 1, "one encodable format today");
    let thumb = &result.generated[0];
    assert_eq!(thumb.tier, DerivativeTier::Thumbnail);
    assert_eq!(thumb.format, DerivativeFormat::Jxl);
    assert_eq!(thumb.manifest.core.format, "image/jxl");
    assert_eq!(thumb.manifest.core.role, DerivativeRole::Thumbnail);
    // The manifest commits to the **ciphertext**, not to the plaintext on disk. Re-derive it
    // the way the push path does — from the recorded prefix under the same AMK — and the
    // signed address has to come back.
    let seal = sealer(Uuid::from_u128(0xB1));
    let file_key = seal
        .amk
        .derive_file_key(&Uuid::from_u128(0xB1), &thumb.manifest.core.nonce_prefix);
    let (_, ciphertext) = crate::crypto::encryption::stream::encrypt_asset_vec_with_prefix(
        &file_key,
        thumb.manifest.core.nonce_prefix,
        &thumb.bytes,
    );
    assert_eq!(
        thumb.manifest.core.ciphertext_hash,
        crate::crypto::hash::hash_bytes(&ciphertext),
        "the manifest binds the ciphertext the push path re-derives"
    );
    assert_ne!(
        thumb.manifest.core.ciphertext_hash,
        crate::crypto::hash::hash_bytes(&thumb.bytes),
        "and that is not the plaintext's address — a thumbnail is a recognisable copy of a \
         private photo and does not cross the network in the clear"
    );
    assert_eq!(
        crate::crypto::encryption::stream::decrypt_asset_vec(
            &file_key,
            &thumb.manifest.core.nonce_prefix,
            &ciphertext
        )
        .expect("the ciphertext authenticates"),
        thumb.bytes,
        "and it round-trips back to the bytes on disk"
    );
    assert_eq!(thumb.manifest.core.version, DERIVATIVE_MANIFEST_VERSION);
    assert!(
        thumb.manifest.core.prior_provenance_hash.is_none(),
        "first of its role"
    );

    // The bytes are a real JXL of the tier's size.
    assert_eq!(
        StillFormat::from_bytes(&thumb.bytes),
        Some(StillFormat::Jxl)
    );
    let back = RawshiftDecoder
        .decode(&thumb.bytes, "jxl")
        .expect("the thumbnail decodes");
    assert_eq!((back.width(), back.height()), (256, 192));

    // The gap is per (tier, format), recorded rather than collapsed. WebP is here for a
    // different reason from AVIF: not a missing toolchain but a codec that does not compile for
    // aarch64, so it is deferred on every target rather than only where nasm is absent.
    assert_eq!(
        result.deferred,
        vec![
            (DerivativeTier::Thumbnail, DerivativeFormat::Avif),
            (DerivativeTier::Thumbnail, DerivativeFormat::WebP),
        ]
    );

    // Both signatures verify over the canonical core.
    let (device, write_tier) = signers();
    let bytes = thumb.manifest.core.signing_bytes();
    assert!(
        device
            .verifying_key()
            .verify(&bytes, &thumb.manifest.device_sig)
    );
    assert!(
        write_tier
            .verifying_key()
            .verify(&bytes, &thumb.manifest.write_sig)
    );
}

/// The tier's declared `q=50` is **advisory today**: `zune-jpegxl`'s `JxlSimpleEncoder` is
/// lossless, so the thumbnail round-trips its downscaled pixels exactly and costs what a
/// lossless encode costs.
///
/// Asserted rather than left as a comment, because it is the one place this build visibly
/// departs from the tier table and the departure disappears the moment a lossy backend lands.
#[test]
fn the_jxl_thumbnail_is_lossless_today() {
    let frame = gradient(512, 384);
    let original = png_bytes(&frame);
    let result = generate(&frame, &original);
    let thumb = &result.generated[0];

    let back = RawshiftDecoder
        .decode(&thumb.bytes, "jxl")
        .expect("the thumbnail decodes");
    let expected = downscale_rgba8(&gradient(512, 384), 256);
    assert_eq!(
        back.image, expected,
        "a lossless encode reproduces the downscaled frame exactly"
    );
}

/// A source no larger than the tier's cap takes the signed `original` sentinel — an explicit
/// marker, distinct from an absent derivative, and never a redundant re-encode.
#[test]
fn a_source_within_the_cap_signs_the_original_sentinel() {
    let frame = gradient(128, 96);
    let original = png_bytes(&frame);
    let result = generate(&frame, &original);

    assert_eq!(result.generated.len(), 1);
    let only = &result.generated[0];
    assert_eq!(only.format, DerivativeFormat::Original);
    assert_eq!(only.manifest.core.format, "original");
    assert!(
        only.bytes.is_empty(),
        "the sentinel *references* the original rather than copying it: a copy would put the \
         source's EXIF, GPS included, into a derivative blob"
    );
    assert_eq!(
        only.manifest.core.ciphertext_hash, ORIGINAL_SEAL.ciphertext_hash,
        "the sentinel signs the **original's** ciphertext address — the blob a receiver already \
         holds — and encrypts nothing of its own"
    );
    assert_eq!(
        only.manifest.core.nonce_prefix, ORIGINAL_SEAL.nonce_prefix,
        "and the original's prefix, so the reference selects the same key"
    );
    assert!(
        result.deferred.is_empty(),
        "nothing was deferred: the tier is satisfied by the original, not by a missing encoder"
    );
    assert_eq!(DerivativeFormat::Original.extension(), None);
}

/// Manifests of the same role chain by content hash over the previous one's canonical CBOR,
/// signatures included — the same append-only link the asset provenance chain uses.
///
/// Exercised through [`sign_derivative`](super::derivative::sign_derivative) rather than
/// through [`generate_still_derivatives`], and deliberately: only JXL is encodable today, so a
/// single call produces one manifest per role and the multi-link case — the half that can
/// actually be wrong — is unreachable from the public entry point until a second encoder lands
/// (the filed `S-B1` remainder, #437).
#[test]
fn manifests_of_one_role_form_an_append_only_chain() {
    let (device, write_tier) = signers();
    let seal = sealer(Uuid::from_u128(0xB2));
    let heads = no_prior_heads();
    let ctx = context(&device, &write_tier, &seal, &heads, Uuid::from_u128(0xB2));
    let mut prior = None;

    let first = super::derivative::sign_derivative(
        &ctx,
        DerivativeTier::Thumbnail,
        DerivativeFormat::Jxl,
        b"first generation bytes".to_vec(),
        seal.seal(b"first generation bytes").expect("sealing"),
        &mut prior,
    )
    .expect("signing the first manifest");
    assert!(
        first.manifest.core.prior_provenance_hash.is_none(),
        "the first manifest of a role starts that role's chain"
    );

    let expected_link = crate::crypto::hash::hash_bytes(
        &crate::cbor::to_canonical_vec(&first.manifest).expect("canonical CBOR"),
    );
    assert_eq!(
        prior,
        Some(expected_link),
        "the cursor advances to this manifest"
    );

    let second = super::derivative::sign_derivative(
        &ctx,
        DerivativeTier::Thumbnail,
        DerivativeFormat::Jxl,
        b"second generation bytes".to_vec(),
        seal.seal(b"second generation bytes").expect("sealing"),
        &mut prior,
    )
    .expect("signing the second manifest");
    assert_eq!(
        second.manifest.core.prior_provenance_hash,
        Some(expected_link),
        "the second manifest chains to the first by content hash"
    );

    // Breaking the link is detectable: the hash covers the signatures, so any edit to the first
    // manifest moves the value the second one has to carry.
    let mut tampered = first.manifest.clone();
    tampered.core.generated_at = "2026-09-02T00:00:00Z".into();
    let tampered_link = crate::crypto::hash::hash_bytes(
        &crate::cbor::to_canonical_vec(&tampered).expect("canonical CBOR"),
    );
    assert_ne!(
        tampered_link, expected_link,
        "a rewritten predecessor no longer matches the link its successor signed"
    );
}

/// Each tier records its own role, so each role is its own chain rather than one interleaved
/// sequence.
#[test]
fn each_tier_starts_its_own_role_chain() {
    let frame = gradient(512, 384);
    let original = png_bytes(&frame);
    let (device, write_tier) = signers();
    let decoded = RawshiftDecoder.decode(&original, "png").expect("decode");

    let seal = sealer(Uuid::from_u128(0xB5));
    let both = generate_still_derivatives(
        &decoded,
        &[DerivativeTier::Thumbnail, DerivativeTier::Preview],
        &context(
            &device,
            &write_tier,
            &seal,
            &no_prior_heads(),
            Uuid::from_u128(0xB5),
        ),
    )
    .expect("both tiers");

    let roles: Vec<DerivativeRole> = both
        .generated
        .iter()
        .map(|d| d.manifest.core.role)
        .collect();
    assert_eq!(
        roles,
        vec![DerivativeRole::Thumbnail, DerivativeRole::Preview]
    );
    for derivative in &both.generated {
        assert!(
            derivative.manifest.core.prior_provenance_hash.is_none(),
            "the first manifest of each role starts that role's chain"
        );
    }

    // The preview tier keeps the source resolution; only the thumbnail caps a long edge.
    let previewed = both
        .generated
        .iter()
        .find(|d| d.tier == DerivativeTier::Preview)
        .expect("a preview was generated");
    let back = RawshiftDecoder
        .decode(&previewed.bytes, "jxl")
        .expect("the preview decodes");
    assert_eq!((back.width(), back.height()), (512, 384));
}

/// Two derivatives of one asset get **distinct** nonce prefixes, and therefore distinct keys and
/// distinct ciphertexts, even when their plaintext is byte-identical.
///
/// This is the property that makes per-derivative sealing worth doing rather than reusing the
/// original's key: a shared prefix would reuse a keystream across two blobs under one file key,
/// which is the failure the encryption doc's per-file derivation exists to prevent.
#[test]
fn two_derivatives_of_one_asset_never_share_a_nonce_prefix() {
    let (device, write_tier) = signers();
    let asset_id = Uuid::from_u128(0xB6);
    let seal = sealer(asset_id);
    let heads = no_prior_heads();
    let ctx = context(&device, &write_tier, &seal, &heads, asset_id);
    let identical = b"byte-identical derivative plaintext".to_vec();

    let mut prior = None;
    let first = super::derivative::sign_derivative(
        &ctx,
        DerivativeTier::Thumbnail,
        DerivativeFormat::Jxl,
        identical.clone(),
        seal.seal(&identical).expect("sealing"),
        &mut prior,
    )
    .expect("first");
    let mut prior = None;
    let second = super::derivative::sign_derivative(
        &ctx,
        DerivativeTier::Preview,
        DerivativeFormat::Jxl,
        identical.clone(),
        seal.seal(&identical).expect("sealing"),
        &mut prior,
    )
    .expect("second");

    assert_eq!(
        first.bytes, second.bytes,
        "the plaintext really is identical"
    );
    assert_ne!(
        first.manifest.core.nonce_prefix, second.manifest.core.nonce_prefix,
        "a fresh prefix is drawn per derivative"
    );
    assert_ne!(
        first.manifest.core.ciphertext_hash, second.manifest.core.ciphertext_hash,
        "so identical plaintext does not produce a shared ciphertext or a shared key"
    );
}

/// **The privacy case.** A thumbnail must not inherit the source's EXIF, and above all not its
/// GPS fix.
///
/// The control half is what makes this a real test: the crate's *default* is to embed
/// everything, so the same frame encoded the way `MetadataEmbedOptions::default()` would encode
/// it does carry the fix. Capsule's derivative does not.
#[test]
fn a_thumbnail_carries_no_exif_and_no_gps() {
    let frame = gradient(512, 384);
    let located_metadata = located();
    let source = jpeg_bytes(&frame, Some(&located_metadata));

    // The source really does carry the fix — otherwise the assertion below proves nothing.
    assert!(
        contains(&source, b"Exif\0\0"),
        "the fixture JPEG must carry an EXIF APP1 segment"
    );
    assert!(
        gps_rationals_present(&source),
        "the fixture JPEG must carry the GPS rationals"
    );

    // The control: the same frame through the same JXL backend, configured the way the crate's
    // own `MetadataEmbedOptions::default()` would configure it. It leaks — which is what makes
    // the assertion below a test of Capsule's strip rather than of a codec that never embeds.
    let leaky = jxl_bytes(&frame, Some(&located_metadata));
    assert!(
        gps_rationals_present(&leaky),
        "the control must leak, or this test is not testing the strip"
    );

    // Capsule's derivative, over the same GPS-bearing source.
    let (device, write_tier) = signers();
    let decoded = RawshiftDecoder.decode(&source, "jpg").expect("decode");
    let seal = sealer(Uuid::from_u128(0xB3));
    let result = generate_still_derivatives(
        &decoded,
        &DerivativeTier::GENERATED,
        &context(
            &device,
            &write_tier,
            &seal,
            &no_prior_heads(),
            Uuid::from_u128(0xB3),
        ),
    )
    .expect("generation");
    let thumb = &result.generated[0].bytes;
    assert!(!thumb.is_empty(), "the thumbnail has bytes to inspect");
    assert!(!contains(thumb, b"Exif"), "no EXIF box in the thumbnail");
    assert!(!contains(thumb, b"xml "), "no XMP box");
    assert!(!contains(thumb, b"jumb"), "no metadata container box");
    assert!(
        !gps_rationals_present(thumb),
        "the GPS rationals must not survive into a thumbnail"
    );
}

/// Whether `needle` appears anywhere in `haystack`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Whether the fixture's GPS degrees/minutes/seconds appear as EXIF rationals — a
/// `numerator/denominator` pair per component, in either byte order.
fn gps_rationals_present(bytes: &[u8]) -> bool {
    let rational = |value: u32| -> [Vec<u8>; 2] {
        let mut be = value.to_be_bytes().to_vec();
        be.extend_from_slice(&1u32.to_be_bytes());
        let mut le = value.to_le_bytes().to_vec();
        le.extend_from_slice(&1u32.to_le_bytes());
        [be, le]
    };
    // The seconds component of each coordinate is the most distinctive value; requiring both
    // keeps an incidental byte match from reading as a leak.
    let has = |value: u32| rational(value).iter().any(|n| contains(bytes, n));
    has(GPS_LAT_DMS[2]) && has(GPS_LON_DMS[2])
}

// ── The closed format set ────────────────────────────────────────────────────

/// `mime` and `parse` are inverses over the whole closed set, and nothing outside it parses.
#[test]
fn the_closed_format_set_round_trips_and_admits_nothing_else() {
    for format in [
        DerivativeFormat::Jxl,
        DerivativeFormat::Avif,
        DerivativeFormat::WebP,
        DerivativeFormat::Original,
    ] {
        assert_eq!(DerivativeFormat::parse(format.mime()), Some(format));
        assert!(DerivativeFormat::is_recognized(format.mime()));
    }
    for rejected in [
        "image/future-codec",
        "image/jpeg",
        "image/png",
        "IMAGE/WEBP",
        "original ",
        "",
        "embedding/mobileclip-b",
    ] {
        assert!(
            !DerivativeFormat::is_recognized(rejected),
            "{rejected:?} is outside the closed set"
        );
    }
    // Only JXL — the table's committed *master* format — and the sentinel can be produced here.
    // AVIF is blocked on a build-host assembler; WebP is blocked on an upstream defect, its
    // codec not compiling for aarch64 at all.
    assert!(DerivativeFormat::Jxl.is_encodable());
    assert!(DerivativeFormat::Original.is_encodable());
    assert!(!DerivativeFormat::Avif.is_encodable());
    assert!(!DerivativeFormat::WebP.is_encodable());
}

/// A signed still-role manifest whose `format` is outside the closed set is rejected at
/// verification, and an embedding-role manifest — which writes `embedding/{model_id}` into the
/// same field — is not caught in the crossfire.
#[test]
fn verification_rejects_an_unrecognised_still_format() {
    let (device, write_tier) = signers();
    let sign = |role: DerivativeRole, format: &str| -> DerivativeManifest {
        DerivativeCore {
            version: DERIVATIVE_MANIFEST_VERSION.into(),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: Some(PROTOCOL_VERSION.into()),
            amk_version: Some(AmkVersion(1)),
            source_asset_id: Uuid::from_u128(0xB4),
            role,
            format: format.into(),
            ciphertext_hash: crate::crypto::hash::hash_bytes(b"bytes"),
            nonce_prefix: [9, 8, 7, 6, 5, 4, 3],
            generated_by_device: Uuid::from_u128(0xD1),
            generated_by_client: "capsule-core/test".into(),
            model_id: None,
            model_version: None,
            generated_at: "2026-09-01T00:00:00Z".into(),
            prior_provenance_hash: None,
        }
        .sign(&device, &write_tier)
        .expect("signing")
    };

    assert_eq!(
        verify_still_format(&sign(DerivativeRole::Thumbnail, "image/webp")),
        Ok(Some(DerivativeFormat::WebP))
    );
    assert_eq!(
        verify_still_format(&sign(DerivativeRole::Preview, "original")),
        Ok(Some(DerivativeFormat::Original))
    );
    assert_eq!(
        verify_still_format(&sign(DerivativeRole::Thumbnail, "image/future-codec")),
        Err("image/future-codec".to_string()),
        "an unrecognised still format is a structural rejection"
    );
    assert_eq!(
        verify_still_format(&sign(DerivativeRole::Embedding, "embedding/mobileclip-b")),
        Ok(None),
        "the embedding-role grammar is not this set's business"
    );
}

// ── The LQIP producer ────────────────────────────────────────────────────────

/// A decoded frame is exactly what the unconditional LQIP encoder takes — the reason
/// [`DecodedImage`](super::DecodedImage) carries a [`RgbaImage`] rather than its own buffer
/// type.
#[test]
fn a_decoded_frame_encodes_an_lqip_at_the_committed_width() {
    let frame = gradient(200, 150);
    let decoded = RawshiftDecoder
        .decode(&png_bytes(&frame), "png")
        .expect("decode");
    let lqip = Lqip::encode(
        decoded.width(),
        decoded.height(),
        &decoded.image.rgba,
        decoded.gamut,
    )
    .expect("a decoded frame is a valid LQIP source");
    assert_eq!(lqip.as_bytes().len(), 32, "DEFAULT_TIER is 32 bytes");
    assert_eq!(
        lqip.to_sidecar().format_version,
        crate::lqip::LQIP_FORMAT_V1
    );

    // The placeholder is computed from the full-resolution frame, not from the thumbnail:
    // chromahash band-limits on the read side, so pre-resizing would cap fidelity.
    let thumb = downscale_rgba8(&decoded.image, 256);
    let from_thumb = Lqip::encode(thumb.width, thumb.height, &thumb.rgba, decoded.gamut)
        .expect("a downscaled frame also encodes");
    assert_eq!(
        from_thumb.as_bytes().len(),
        32,
        "the tier is fixed regardless of the source size"
    );
}

/// The per-channel accumulator genuinely crosses `u32::MAX`, and the boundary arithmetic is
/// documented for what it is.
///
/// **The accumulator overflow is real and reachable.** Reducing a frame to a cap of 1 sums every
/// sample into one destination pixel, so the running total is `pixels * 255`. At 4200x4200 that
/// is 4.5e9 — past `u32::MAX` (4.29e9) — and a `u32` accumulator would panic in debug and wrap
/// into wrong pixels in release. `downscale_rgba8` is a `pub` entry point, so a cap of 1 is
/// reachable even though the tier table only ever passes 256.
///
/// **The boundary product is defensive, not demonstrated.** `(y + 1) * src_h` reaches
/// `dst_edge * src_edge`, which for the shapes this build actually produces (a 256 px cap) stays
/// far inside 32 bits. It is computed in `u64` anyway because the function is public and its
/// inputs are not bounded by the tier table — but the earlier claim that a 1x300000 frame
/// crossed `u32::MAX` was simply wrong arithmetic (7.7e7, not 7.7e10), and a test asserting a
/// false reason is worse than no test.
#[test]
fn the_downscale_accumulator_survives_crossing_u32_max() {
    // 4200 * 4200 * 255 = 4_501_980_000 > u32::MAX.
    let edge = 4200u32;
    let pixels = u64::from(edge) * u64::from(edge);
    assert!(
        pixels * 255 > u64::from(u32::MAX),
        "the fixture must actually cross the boundary it exists to test"
    );

    let flat = RgbaImage {
        width: edge,
        height: edge,
        rgba: vec![255, 255, 255, 255].repeat((edge * edge) as usize),
    };
    let single = downscale_rgba8(&flat, 1);
    assert_eq!((single.width, single.height), (1, 1));
    assert_eq!(
        single.rgba,
        vec![255, 255, 255, 255],
        "every sample sums into one pixel, and the mean is still 255"
    );

    // The tall-frame shape, kept because it is the one the tier path can actually meet: a
    // lopsided source reduced to a 256 px long edge.
    let tall = RgbaImage {
        width: 1,
        height: 300_000,
        rgba: vec![200, 100, 50, 255].repeat(300_000),
    };
    let reduced = downscale_rgba8(&tall, 256);
    assert_eq!((reduced.width, reduced.height), (1, 256));
    assert!(
        reduced
            .rgba
            .chunks_exact(4)
            .all(|px| px == [200, 100, 50, 255]),
        "a flat frame survives a 1172x row reduction exactly"
    );
}

/// A sealer that refuses is a **workspace** fault and must be distinguishable at the type level
/// from a codec that refuses, because the import path propagates one and degrades on the other.
///
/// This is the `F1` contract: `MediaError::Sign` is its own variant precisely so
/// `prepare_still` can tell "this asset has no thumbnail" from "this workspace cannot author a
/// signed record", instead of string-matching both into one `LifecycleError::Io`.
#[test]
fn a_signing_fault_is_its_own_variant_not_an_encode_failure() {
    struct RefusingSealer;
    impl DerivativeSealer for RefusingSealer {
        fn seal(&self, _plaintext: &[u8]) -> Result<SealedDerivative, MediaError> {
            Err(MediaError::Sign {
                detail: "the hardware signer refused".into(),
            })
        }
    }

    let frame = gradient(512, 384);
    let original = png_bytes(&frame);
    let (device, write_tier) = signers();
    let decoded = RawshiftDecoder.decode(&original, "png").expect("decode");

    let error = generate_still_derivatives(
        &decoded,
        &DerivativeTier::GENERATED,
        &context(
            &device,
            &write_tier,
            &RefusingSealer,
            &no_prior_heads(),
            Uuid::from_u128(0xB8),
        ),
    )
    .expect_err("a refusing sealer fails generation");
    assert!(
        matches!(error, MediaError::Sign { .. }),
        "a signing/sealing refusal keeps its own identity all the way out: {error:?}"
    );
}

/// **A role's chain continues across generations.** A second run over an asset that already has
/// a thumbnail extends that role's chain rather than starting a parallel one.
///
/// This is what makes derivative provenance append-only *in time* rather than merely within one
/// call, and it is the property a `#437` backfill depends on: adding the AVIF variant to an
/// asset that already has JXL must not fork the record. A forked chain is not something a later
/// run can repair, so it is asserted before the backfill exists.
#[test]
fn a_roles_chain_continues_across_generation_runs() {
    let frame = gradient(512, 384);
    let original = png_bytes(&frame);
    let (device, write_tier) = signers();
    let asset_id = Uuid::from_u128(0xB7);
    let seal = sealer(asset_id);
    let decoded = RawshiftDecoder.decode(&original, "png").expect("decode");

    // First generation: the role's chain starts.
    let first = generate_still_derivatives(
        &decoded,
        &DerivativeTier::GENERATED,
        &context(&device, &write_tier, &seal, &no_prior_heads(), asset_id),
    )
    .expect("first run");
    let head = &first.generated[0].manifest;
    assert!(
        head.core.prior_provenance_hash.is_none(),
        "the first manifest of a role starts that role's chain"
    );

    // What a reader would compute from the persisted bundle.
    let link = crate::crypto::hash::hash_bytes(
        &crate::cbor::to_canonical_vec(head).expect("canonical CBOR"),
    );
    let mut heads = HashMap::new();
    heads.insert(DerivativeRole::Thumbnail, link);

    // Second generation, handed that head.
    let second = generate_still_derivatives(
        &decoded,
        &DerivativeTier::GENERATED,
        &context(&device, &write_tier, &seal, &heads, asset_id),
    )
    .expect("second run");
    assert_eq!(
        second.generated[0].manifest.core.prior_provenance_hash,
        Some(link),
        "the second run extends the chain instead of forking it"
    );

    // A role with no recorded head still starts cleanly — the map is a lookup, not a gate.
    let preview = generate_still_derivatives(
        &decoded,
        &[DerivativeTier::Preview],
        &context(&device, &write_tier, &seal, &heads, asset_id),
    )
    .expect("preview run");
    assert!(
        preview.generated[0]
            .manifest
            .core
            .prior_provenance_hash
            .is_none(),
        "a role the map does not mention starts its own chain"
    );
}

/// A **panicking sealer/encoder** is caught at the same boundary a panicking decoder is, so one
/// bad frame cannot abort a 20,000-photo import part way through.
///
/// The decode guard was never the whole story: `chromahash` and the JXL encoder are both pre-1.0
/// too, and they run *after* the decoder on the same untrusted pixels. This mirrors
/// [`HostileDecoder`] on the encode side.
#[test]
fn a_panicking_encoder_is_caught_like_a_panicking_decoder() {
    struct PanickingSealer;
    impl DerivativeSealer for PanickingSealer {
        fn seal(&self, _plaintext: &[u8]) -> Result<SealedDerivative, MediaError> {
            panic!("a pre-1.0 codec panicking on a frame the decoder accepted");
        }
    }

    let frame = gradient(512, 384);
    let original = png_bytes(&frame);
    let (device, write_tier) = signers();
    let decoded = RawshiftDecoder.decode(&original, "png").expect("decode");

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let caught = super::guarded("derivatives", || {
        generate_still_derivatives(
            &decoded,
            &DerivativeTier::GENERATED,
            &context(
                &device,
                &write_tier,
                &PanickingSealer,
                &no_prior_heads(),
                Uuid::from_u128(0xB9),
            ),
        )
    });
    std::panic::set_hook(previous);

    assert!(
        matches!(caught, Err(MediaError::DecoderPanic)),
        "an unwind from anywhere in generation becomes a reported error, never an abort"
    );
}
