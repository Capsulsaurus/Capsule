//! The typed failure set of the still pipeline (slice `S-B13`).
//!
//! Every one of these is a *report*, never a rejection: Capsule is a backup tool, so an original
//! whose pixels cannot be read is still imported as a signed, encrypted, `verify_asset`-accepting
//! asset. What varies is only whether a placeholder and a thumbnail could be produced beside it.
//! The point of the enum is that the reasons stay apart — a missing codec is a known, deferred
//! gap, while a *supported* format that fails to decode is a defect somebody should look at.

use thiserror::Error;

use super::detect::StillFormat;
use crate::derivative_format::DerivativeFormat;

/// Which direction of a codec a format was needed for. A build can decode a format it cannot
/// encode (every format here except WebP) and the message has to say which half is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FormatOp {
    /// Reading pixels out of the format.
    Decode,
    /// Writing pixels into the format.
    Encode,
}

impl FormatOp {
    /// The lowercase word used in log fields and messages.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Decode => "decode",
            Self::Encode => "encode",
        }
    }
}

/// Why the still pipeline could not produce what was asked of it.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MediaError {
    /// The format was identified and this build links no codec for it in that direction —
    /// HEIC, AVIF and the RAW families for decode; everything but WebP for encode. The
    /// **expected** gap: the format table is honest about it and
    /// [`StillFormat::is_decodable`](super::StillFormat::is_decodable) is the same predicate the
    /// pipeline gates on, so this is reached only when a caller bypasses that gate.
    #[error("this build links no {} codec for {format}", op.as_str())]
    UnsupportedFormat {
        /// The format that was identified.
        format: StillFormat,
        /// Which half of the codec was missing.
        op: FormatOp,
    },
    /// The bytes are not any still image Capsule models — a video, an XMP sidecar, an SVG, or
    /// noise. Distinct from [`UnsupportedFormat`](Self::UnsupportedFormat): there is nothing to
    /// defer, because there is no still here to decode later either.
    #[error("not a still image Capsule models")]
    NotAStillImage,
    /// A format this build *does* decode did not decode these particular bytes — truncation,
    /// corruption, or a decoder bug. `detail` is the underlying decoder's message, flattened to
    /// a `String` (rather than held as a `#[source]`) so this type stays `Clone + PartialEq` and
    /// so no pre-1.0 dependency type appears in Capsule's public error shape.
    #[error("decoding {format} failed: {detail}")]
    Decode {
        /// The format that was being decoded.
        format: StillFormat,
        /// The decoder's own message.
        detail: String,
    },
    /// A derivative encode failed. Same shape and same reasoning as
    /// [`Decode`](Self::Decode).
    #[error("encoding {format} failed: {detail}")]
    Encode {
        /// The derivative format that was being written.
        format: DerivativeFormat,
        /// The encoder's own message.
        detail: String,
    },
    /// The decoded frame has a zero dimension. Guarded rather than trusted because
    /// [`crate::lqip::Lqip::encode`] and the downscale both need a non-empty frame, and a
    /// hand-crafted header can claim one.
    #[error("decoded frame has a zero dimension ({width}x{height})")]
    ZeroDimension {
        /// The width the decoder reported.
        width: u32,
        /// The height the decoder reported.
        height: u32,
    },
    /// The header claims more pixels than [`MAX_DECODE_PIXELS`](super::MAX_DECODE_PIXELS)
    /// allows, and the decode was refused **before** allocating.
    ///
    /// This is the decode-bomb guard, and it has to be a pre-decode check rather than a
    /// post-decode sanity assert: `rawshift-image` decodes to interleaved RGB `u16`, i.e. six
    /// bytes per pixel, so a 60000x60000 PNG asks for ~21 GB inside the decoder before Capsule
    /// ever sees a buffer.
    #[error("{pixels} pixels exceeds the {limit}-pixel decode budget")]
    PixelBudgetExceeded {
        /// The pixel count the header claims.
        pixels: u64,
        /// The budget in force.
        limit: u64,
    },
    /// The decoder's buffer length does not match the dimensions it reported. A `RgbImage` is
    /// constructible from mismatched parts (`RgbImage::new` validates nothing), so this is
    /// checked at the boundary rather than assumed.
    #[error(
        "{format} decoder returned {actual} samples for {width}x{height} (expected {expected})"
    )]
    BufferLengthMismatch {
        /// The format that was decoded.
        format: StillFormat,
        /// The width the decoder reported.
        width: u32,
        /// The height the decoder reported.
        height: u32,
        /// The sample count the dimensions imply.
        expected: u128,
        /// The sample count the decoder actually returned.
        actual: u128,
    },
    /// Signing or sealing a derivative manifest failed — a hardware device signer refused, or
    /// the album's write-tier key for this epoch is missing.
    ///
    /// **The one derivative failure that is not about pixels**, and the reason it has its own
    /// variant rather than being folded into [`Encode`](Self::Encode): every other error here
    /// says "this asset has no thumbnail", which an import survives, while this one says the
    /// *workspace* cannot author a signed record — the same fault that would stop the asset's
    /// own manifest. The import path propagates this and degrades on everything else, and it can
    /// only tell them apart if the type does.
    #[error("signing the derivative manifest failed: {detail}")]
    Sign {
        /// The underlying crypto error's message.
        detail: String,
    },
    /// A third-party decoder panicked and the unwind was caught at the pipeline boundary.
    ///
    /// A pre-1.0 decoder fed untrusted bytes is exactly the place a panic is plausible, and an
    /// import must never abort over a thumbnail. Reported as a decode failure rather than
    /// swallowed, because a panic is a defect worth seeing.
    #[error("the decoder panicked")]
    DecoderPanic,
}
