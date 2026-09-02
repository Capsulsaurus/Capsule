//! Capsule's closed still-format set, its magic-byte table, and the codec-coverage predicate.
//!
//! # Why Capsule sniffs rather than delegating
//!
//! `rawshift-image` ships `detect_standard_format`, and Capsule deliberately does not use it as
//! the primary table: its HEIC arm is `#[cfg(feature = "heic-decode")]`, so a build without the
//! HEIC codec cannot *recognise* HEIC either. That would make the typed refusal for exactly the
//! formats this build cannot decode depend on whether it can decode them — a HEIC would arrive
//! as "not a still image" instead of "a still image with no codec here", which is the difference
//! between a reportable, backfillable gap and an apparent non-image. Capsule's reference library
//! is HEIC end to end, so that distinction is the whole point of slice `S-B13`.
//!
//! The two tables are held together by a test rather than by hope:
//! `capsule-core`'s `still_format_agrees_with_rawshift_detection` asserts that for every format
//! both sides define unconditionally, Capsule's sniff and `detect_standard_format` name the same
//! thing.
//!
//! # Bytes first, extension second
//!
//! Detection is by header, so a `.jpg` that is really a HEIC is classified as HEIC. The
//! extension is consulted in exactly two places, both of them cases a header genuinely cannot
//! settle:
//!
//! 1. **RAW refinement.** ARW, CR2, DNG and NEF are TIFF containers and CR3 is ISO-BMFF; their
//!    headers are their container's, so only the extension distinguishes a Sony ARW from a
//!    scanner's TIFF. A misrefinement costs a deferral, never wrong pixels, because no RAW
//!    family decodes in this build.
//! 2. **Fallback.** When the header sniffs to nothing at all.

use std::fmt;

/// The decode budget in pixels, refused **before** the decoder allocates.
///
/// 256 Mpx sits well above a 100 Mpx medium-format frame and well below an allocation bomb:
/// `rawshift-image` decodes to interleaved RGB `u16`, so this ceiling caps the decoder's own
/// buffer at ~1.5 GB and Capsule's RGBA8 copy at ~1 GB.
pub const MAX_DECODE_PIXELS: u64 = 256_000_000;

/// The closed set of still-image formats Capsule models.
///
/// Closed on purpose: a still Capsule cannot name is not a still it silently ignores, it is a
/// [`MediaError::NotAStillImage`](super::MediaError::NotAStillImage). Membership here says
/// "Capsule knows this is a photo"; [`is_decodable`](Self::is_decodable) says whether *this
/// build* can read its pixels. The two are deliberately separate — that gap is what
/// `DerivativeStatus::DeferredNoCodec` reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StillFormat {
    /// JPEG / JFIF. Decoded by `zune-jpeg`.
    Jpeg,
    /// PNG. Decoded by `zune-png`; alpha is flattened at the decode boundary.
    Png,
    /// WebP. Decoded and encoded by `libwebp`; the derivative format this build produces.
    WebP,
    /// JPEG XL. Decoded by `jxl-oxide`. No lossy encoder without C libjxl.
    Jxl,
    /// TIFF. Decoded by the `tiff` crate. Also the container of most RAW families.
    Tiff,
    /// GIF. Decoded by `gif`; the first frame only.
    Gif,
    /// Netpbm (P5/P6/P7/PFM). Recognised, not decoded: the `ppm-decode` backend is deliberately
    /// not enabled. Netpbm is an intermediate and test-fixture format rather than something a
    /// photo library holds, so recognising it and deferring is the honest outcome — and it costs
    /// one fewer dependency than a codec nobody's library needs.
    Ppm,
    /// AVIF. Recognised, not decoded — the backend needs system libdav1d.
    Avif,
    /// HEIC / HEIF. Recognised, not decoded — the backend needs system libheif.
    Heic,
    /// Sony ARW.
    Arw,
    /// Canon CR2.
    Cr2,
    /// Canon CR3.
    Cr3,
    /// Canon CRW.
    Crw,
    /// Adobe DNG (including Apple ProRAW).
    Dng,
    /// Nikon NEF.
    Nef,
    /// Fujifilm RAF.
    Raf,
}

/// Every format this build can read pixels out of — the codec-coverage table, in one place.
///
/// Logged verbatim on a deferral so the gap is legible in the field rather than only in a doc.
pub const SUPPORTED_STILL_FORMATS: &[StillFormat] = &[
    StillFormat::Jpeg,
    StillFormat::Png,
    StillFormat::WebP,
    StillFormat::Jxl,
    StillFormat::Tiff,
    StillFormat::Gif,
];

/// The RAW families, all of them recognised and none of them decodable here.
const RAW_FORMATS: &[StillFormat] = &[
    StillFormat::Arw,
    StillFormat::Cr2,
    StillFormat::Cr3,
    StillFormat::Crw,
    StillFormat::Dng,
    StillFormat::Nef,
    StillFormat::Raf,
];

impl StillFormat {
    /// Whether *this build* can read pixels out of the format — the single codec-coverage
    /// predicate, and the gate the decode path checks before touching a decoder.
    pub fn is_decodable(self) -> bool {
        SUPPORTED_STILL_FORMATS.contains(&self)
    }

    /// Whether the format is one of the RAW families. RAW is recognised, never decoded here;
    /// its container is TIFF or ISO-BMFF, so the extension is what names the family.
    pub fn is_raw(self) -> bool {
        RAW_FORMATS.contains(&self)
    }

    /// The canonical media type — what the sidecar's `content_type` carries when detection
    /// succeeded.
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Jpeg => "image/jpeg",
            Self::Png => "image/png",
            Self::WebP => "image/webp",
            Self::Jxl => "image/jxl",
            Self::Tiff => "image/tiff",
            Self::Gif => "image/gif",
            Self::Ppm => "image/x-portable-anymap",
            Self::Avif => "image/avif",
            Self::Heic => "image/heic",
            Self::Arw => "image/x-sony-arw",
            Self::Cr2 => "image/x-canon-cr2",
            Self::Cr3 => "image/x-canon-cr3",
            Self::Crw => "image/x-canon-crw",
            Self::Dng => "image/x-adobe-dng",
            Self::Nef => "image/x-nikon-nef",
            Self::Raf => "image/x-fuji-raf",
        }
    }

    /// The lowercase extension table — the *fallback*, used only where a header cannot settle
    /// the question (see the module docs). Extensions arrive lowercased.
    pub fn from_extension(ext: &str) -> Option<Self> {
        Some(match ext {
            "jpg" | "jpeg" | "jpe" | "jfif" => Self::Jpeg,
            "png" => Self::Png,
            "webp" => Self::WebP,
            "jxl" => Self::Jxl,
            "tif" | "tiff" => Self::Tiff,
            "gif" => Self::Gif,
            "ppm" | "pgm" | "pnm" | "pfm" => Self::Ppm,
            "avif" | "avifs" => Self::Avif,
            "heic" | "heif" | "hif" => Self::Heic,
            "arw" => Self::Arw,
            "cr2" => Self::Cr2,
            "cr3" => Self::Cr3,
            "crw" => Self::Crw,
            "dng" => Self::Dng,
            "nef" | "nrw" => Self::Nef,
            "raf" => Self::Raf,
            _ => return None,
        })
    }

    /// Sniff the format from the file header alone.
    ///
    /// `None` means "no still image Capsule models starts like this" — a video, an SVG, an XMP
    /// sidecar, or noise. A RAW file sniffs to its *container* here ([`Tiff`](Self::Tiff), or
    /// `None` for CR3's unrecognised `ftyp` brand); [`detect`](Self::detect) is what refines it.
    ///
    /// Each signature carries **its own** length requirement rather than sharing one floor: a
    /// Netpbm header is eleven bytes and a JPEG's SOI three, so a blanket minimum would report
    /// a perfectly well-formed short file as "not an image".
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let at = |start: usize, needle: &[u8]| -> bool {
            bytes
                .get(start..start + needle.len())
                .is_some_and(|window| window == needle)
        };

        if at(0, b"GIF87a") || at(0, b"GIF89a") {
            return Some(Self::Gif);
        }
        if at(0, b"\xFF\xD8\xFF") {
            return Some(Self::Jpeg);
        }
        if at(0, b"\x89PNG\r\n\x1a\n") {
            return Some(Self::Png);
        }
        if at(0, b"RIFF") && at(8, b"WEBP") {
            return Some(Self::WebP);
        }
        // Bare JXL codestream, then the ISO-BMFF-wrapped form.
        if at(0, b"\xFF\x0A") {
            return Some(Self::Jxl);
        }
        if at(4, b"JXL ") {
            return Some(Self::Jxl);
        }
        if at(0, b"II\x2A\x00") || at(0, b"MM\x00\x2A") {
            return Some(Self::Tiff);
        }
        if at(4, b"ftyp")
            && let Some(brand) = bytes.get(8..12)
        {
            return Self::from_isobmff_brand(brand);
        }
        // Netpbm: 'P' + a binary version + whitespace. P1-P4 are excluded because the
        // `zune-ppm` backend does not decode them, matching `rawshift-image`.
        if let Some(&[b'P', version, space]) = bytes.get(..3)
            && matches!(version, b'5' | b'6' | b'7' | b'F' | b'f')
            && space.is_ascii_whitespace()
        {
            return Some(Self::Ppm);
        }
        None
    }

    /// The ISO-BMFF `ftyp` brand table.
    ///
    /// `mif1` is the generic HEIF brand and is claimed in practice by both HEIC and AVIF
    /// writers. Capsule reads it as HEIC because the reference library is HEIC end to end;
    /// `rawshift-image` reads it as AVIF. The divergence is cosmetic while neither decodes —
    /// both produce the same
    /// [`UnsupportedFormat`](super::MediaError::UnsupportedFormat) refusal and the same
    /// `DeferredNoCodec` status — and it is a log-label difference, never a pixel difference.
    fn from_isobmff_brand(brand: &[u8]) -> Option<Self> {
        Some(match brand {
            b"avif" | b"avis" => Self::Avif,
            b"heic" | b"heix" | b"heis" | b"hevc" | b"hevx" | b"msf1" | b"mif1" => Self::Heic,
            b"crx " => Self::Cr3,
            _ => return None,
        })
    }

    /// Identify a still: header first, extension only where a header cannot settle it.
    ///
    /// `ext` is the source file's lowercase extension without the dot (`""` when it has none).
    /// The two extension-consulting cases are documented on the module.
    pub fn detect(bytes: &[u8], ext: &str) -> Option<Self> {
        match Self::from_bytes(bytes) {
            // A TIFF header is also every TIFF-based RAW's header. Refine on the extension, and
            // only ever *into* a RAW family — a `.tif` stays TIFF.
            Some(Self::Tiff) => match Self::from_extension(ext) {
                Some(raw) if raw.is_raw() => Some(raw),
                _ => Some(Self::Tiff),
            },
            Some(format) => Some(format),
            None => Self::from_extension(ext),
        }
    }
}

impl fmt::Display for StillFormat {
    /// The format's media type — what a log line and an error message both want.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.mime())
    }
}
