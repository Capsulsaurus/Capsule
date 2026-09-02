//! LQIP — the low-quality image placeholder carried in every asset's signed sidecar.
//!
//! SSoT: [Thumbnails — LQIP](https://docs/design/thumbnails/#lqip); slice `S-B14`.
//!
//! # Why this module exists at the crate root
//!
//! The placeholder is produced by the import pipeline, read by the native apps through the
//! uniffi FFI, and read by the browser through `capsule-wasm`. A placeholder that differed by
//! which client imported the photo would be a visible defect, so there is exactly **one**
//! implementation and it must be reachable from all three. That rules out
//! `capsule_core::media`, the retired decode/encode stack that lives in `legacy-review/`
//! and is `native`-only wherever it is restored. It equally rules out
//! Rawshift, which `AGENTS.md` forbids from wrapping Chromahash — Capsule imports it directly.
//!
//! This module is therefore **unconditional**: no feature gate, no `native`, and it compiles
//! for `wasm32-unknown-unknown` (chromahash has zero runtime dependencies and ships a
//! `simd128` backend). The one part that cannot be unconditional is the bridge to the sidecar
//! record, because `crate::sidecar` is itself `native`-gated; that lives in
//! [`sidecar`](crate::lqip::sidecar), behind the same gate, over this same encoder.
//!
//! # The contract
//!
//! Encoding is [Chromahash](https://crates.io/crates/chromahash) at
//! [`chromahash::DEFAULT_TIER`] — **exactly 32 bytes**. Capsule does not vary the tier per
//! asset, and four chromahash calls carry the whole contract; this module deliberately uses no
//! more than these:
//!
//! | Call | Role here |
//! | --- | --- |
//! | `encode(w, h, &rgba, gamut)` | [`Lqip::encode`](crate::lqip::Lqip::encode) — generation at the default tier. |
//! | `decode_capped(max_w, max_h)` | [`Lqip::decode_capped`](crate::lqip::Lqip::decode_capped) — the band-limited render. |
//! | `average_color()` | [`Lqip::dominant_color`](crate::lqip::Lqip::dominant_color) — the DC-only fallback fill. |
//! | `from_bytes` / `as_bytes` | [`Lqip::from_bytes`](crate::lqip::Lqip::from_bytes) / [`Lqip::as_bytes`](crate::lqip::Lqip::as_bytes) — the sidecar round trip. |
//!
//! Notably absent: any pre-resize of the source. The 100 px downscale the retired ThumbHash
//! implementation performed before hashing was a ThumbHash artifact — chromahash takes the full
//! RGBA frame and band-limits on the *read* side via `decode_capped`, so pre-resizing here
//! would silently cap fidelity that the format is able to carry.
//!
//! # Versioned fallback
//!
//! [`LQIP_FORMAT_V1`](crate::lqip::LQIP_FORMAT_V1) tags the payload inside the sidecar. A reader
//! that does not recognize the version — or that holds bytes `from_bytes` rejects — paints the
//! solid `dominant_color` fill instead of misrendering (see [`render`](crate::lqip::render)).
//! That is the mechanism that makes a future chromahash revision a versioned change rather than a
//! silent break, and it is also what makes any stale non-chromahash payload degrade to a flat
//! colour rather than to noise.

use thiserror::Error;

// The sidecar bridge, and only the bridge, is `native`-gated — because `crate::sidecar` is.
// The encoder above stays unconditional, which is what keeps `capsule-wasm` and the native
// surfaces on one implementation.
#[cfg(feature = "native")]
pub mod sidecar;

/// The LQIP chromahash format version written into the sidecar.
///
/// This is Capsule's *sidecar* format version, not chromahash's own wire-format generation
/// (`chromahash::FORMAT_VERSION`, which is self-describing inside the payload's first byte).
/// It stays `1`: the sidecar schema has always declared this field as `Lqip.chromahash` and
/// version 1 as the chromahash format version. The ThumbHash payloads the interim
/// implementation wrote were an undeclared stand-in for a dependency that had not shipped, not
/// the declared encoding — so writing real chromahash under version 1 makes the code match a
/// contract that never changed.
///
/// That is legitimate **only** while the migration is total, i.e. no persisted sidecar carries
/// a ThumbHash payload under version 1. ThumbHash payloads are shorter than 32 bytes but
/// overlap the lower chromahash tiers in length, so byte length alone cannot discriminate a
/// stale one. Should such a sidecar ever exist, the fix is a *new* format version — never a
/// redefinition of this one.
pub const LQIP_FORMAT_V1: u16 = 1;

/// The source colour space an [`Lqip::encode`] call is told to interpret its pixels in.
///
/// A deliberate mirror of [`chromahash::Gamut`] rather than a re-export: this is a public
/// Capsule type that crosses the FFI and the wasm boundary, so a pre-1.0 dependency must not be
/// able to reshape it. The mapping is exhaustive in both directions and [`From`] keeps it
/// compile-checked.
///
/// **The gamut is an encode-side input only.** The sidecar stores the payload, the format
/// version and the fallback colour — it does not store the gamut, and chromahash does not carry
/// it in the payload either. Once encoded, the source gamut is not recoverable from the
/// sidecar; the decode side always renders to sRGB.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Gamut {
    /// sRGB / Rec. 709 primaries — the default and the only safe assumption for an
    /// untagged source.
    #[default]
    Srgb,
    /// Display P3 (DCI-P3 primaries, sRGB transfer) — the common wide-gamut capture space.
    DisplayP3,
    /// Adobe RGB (1998).
    AdobeRgb,
    /// ITU-R BT.2020 — the ultra-wide gamut used by HDR sources.
    Bt2020,
    /// ProPhoto RGB (ROMM RGB) — the widest of the raw-editing spaces.
    ProPhotoRgb,
}

impl From<Gamut> for chromahash::Gamut {
    fn from(gamut: Gamut) -> Self {
        match gamut {
            Gamut::Srgb => chromahash::Gamut::Srgb,
            Gamut::DisplayP3 => chromahash::Gamut::DisplayP3,
            Gamut::AdobeRgb => chromahash::Gamut::AdobeRgb,
            Gamut::Bt2020 => chromahash::Gamut::Bt2020,
            Gamut::ProPhotoRgb => chromahash::Gamut::ProPhotoRgb,
        }
    }
}

/// A decoded placeholder: packed RGBA8, `width * height * 4` bytes.
///
/// Deliberately a bare owned triple rather than `media::image::buffer::ImageBuffer`. That type
/// is inside the retiring `media` stack and carries `PixelFormat` / `ComponentType` /
/// `ColorSpace` discriminants this module has no use for — a placeholder is always packed
/// RGBA8 in sRGB. Depending on it would re-gate this module behind `media` (and therefore
/// `native`), which is precisely the coupling the slice exists to break; introducing a *second*
/// general-purpose buffer type outside `media` would instead pre-empt whatever the Rawshift
/// rebuild lands. The narrowest thing that carries the whole result is the right seam, and the
/// `media`-side adapter converts it back to an `ImageBuffer` for as long as that stack lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbaImage {
    /// Width in pixels; always at least 1.
    pub width: u32,
    /// Height in pixels; always at least 1.
    pub height: u32,
    /// Packed RGBA8 samples, `width * height * 4` bytes long.
    pub rgba: Vec<u8>,
}

impl RgbaImage {
    /// The 1x1 opaque solid fill used as the versioned fallback.
    fn solid([r, g, b]: [u8; 3]) -> Self {
        Self {
            width: 1,
            height: 1,
            rgba: vec![r, g, b, 255],
        }
    }
}

/// Why an LQIP could not be produced or parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum LqipError {
    /// The source frame has a zero dimension. Guarded here because `chromahash::encode`
    /// *panics* on it, and an undecodable still must degrade to "no LQIP", never to a crash in
    /// the import pipeline.
    #[error("LQIP source has a zero dimension ({width}x{height})")]
    ZeroDimension {
        /// The width that was passed.
        width: u32,
        /// The height that was passed.
        height: u32,
    },
    /// The RGBA buffer length does not match the stated dimensions. Also a `chromahash::encode`
    /// panic, so it is a checked error here.
    #[error("LQIP source is {actual} bytes; {width}x{height} RGBA8 needs {expected}")]
    PixelCountMismatch {
        /// The width that was passed.
        width: u32,
        /// The height that was passed.
        height: u32,
        /// The buffer length `width * height * 4` implies. `u128` because that product
        /// overflows `u64` at the top of the `u32` dimension range, and an error path must not
        /// panic on the very input it exists to reject.
        expected: u128,
        /// The buffer length that was actually passed.
        actual: u128,
    },
    /// The bytes are not a structurally valid chromahash payload. Callers holding sidecar
    /// bytes should prefer [`render`], which turns this into the `dominant_color` fill.
    #[error("not a valid chromahash payload: {0}")]
    InvalidHash(chromahash::ChromaHashError),
}

/// A chromahash LQIP payload — 32 bytes at [`chromahash::DEFAULT_TIER`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lqip(chromahash::ChromaHash);

impl Lqip {
    /// Encode a full-resolution RGBA8 frame at the committed default tier.
    ///
    /// `rgba` must be exactly `width * height * 4` bytes of packed RGBA8, interpreted in
    /// `gamut`. The frame is **not** pre-resized: chromahash consumes the whole thing and
    /// band-limits at [`decode_capped`](Self::decode_capped) time.
    ///
    /// Bit-exact for a given `(width, height, rgba, gamut)` on every platform chromahash
    /// supports, which is what lets the CLI, the FFI and `capsule-wasm` agree byte-for-byte.
    pub fn encode(width: u32, height: u32, rgba: &[u8], gamut: Gamut) -> Result<Self, LqipError> {
        if width == 0 || height == 0 {
            return Err(LqipError::ZeroDimension { width, height });
        }
        // `u32 * u32 * 4` overflows `u64` near the top of the range, so the comparison is done
        // in `u128`, where it cannot.
        let expected = u128::from(width) * u128::from(height) * 4;
        let actual = rgba.len() as u128;
        if expected != actual {
            return Err(LqipError::PixelCountMismatch {
                width,
                height,
                expected,
                actual,
            });
        }

        let hash = chromahash::ChromaHash::encode(width, height, rgba, gamut.into());
        tracing::trace!(
            width,
            height,
            ?gamut,
            bytes = hash.as_bytes().len(),
            "encoded LQIP chromahash"
        );
        Ok(Self(hash))
    }

    /// Parse sidecar bytes back into a payload, validating version, tier, reserved bits and
    /// length. An `Ok` value is guaranteed to decode.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, LqipError> {
        chromahash::ChromaHash::from_bytes(bytes)
            .map(Self)
            .map_err(LqipError::InvalidHash)
    }

    /// The raw payload — what the sidecar's `lqip.chromahash` field carries.
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    /// The average colour as opaque RGB — the `dominant_color` fallback fill, read straight
    /// from the DC coefficients without a full decode.
    pub fn dominant_color(&self) -> [u8; 3] {
        let [r, g, b, _a] = self.0.average_color();
        [r, g, b]
    }

    /// The average colour as RGBA, alpha included.
    pub fn average_rgba(&self) -> [u8; 4] {
        self.0.average_color()
    }

    /// Decode to a placeholder no larger than `max_width` x `max_height` — a band-limited,
    /// alias-free render of exactly the box being painted, rather than a full-size decode the
    /// caller then scales down.
    ///
    /// A zero bound is raised to 1: chromahash would otherwise return an empty buffer, and
    /// every caller of this is about to paint something.
    pub fn decode_capped(&self, max_width: u32, max_height: u32) -> RgbaImage {
        let (width, height, rgba) = self.0.decode_capped(max_width.max(1), max_height.max(1));
        RgbaImage {
            width,
            height,
            rgba,
        }
    }
}

/// Render a stored placeholder record, honouring the versioned fallback.
///
/// The happy path is a recognized [`LQIP_FORMAT_V1`] payload that parses: it is decoded,
/// band-limited to `max_width` x `max_height`. Anything else — an unknown future version, or
/// bytes `from_bytes` rejects (a corrupt payload, or a stale non-chromahash one) — returns the
/// 1x1 solid `dominant_color` fill. A reader must never misrender a payload it does not
/// understand.
pub fn render(
    format_version: u16,
    chromahash: &[u8],
    dominant_color: [u8; 3],
    max_width: u32,
    max_height: u32,
) -> RgbaImage {
    if format_version != LQIP_FORMAT_V1 {
        tracing::debug!(
            format_version,
            known = LQIP_FORMAT_V1,
            "unrecognized LQIP format version; painting the dominant-color fill"
        );
        return RgbaImage::solid(dominant_color);
    }
    match Lqip::from_bytes(chromahash) {
        Ok(hash) => hash.decode_capped(max_width, max_height),
        Err(error) => {
            tracing::debug!(
                %error,
                len = chromahash.len(),
                "undecodable LQIP payload; painting the dominant-color fill"
            );
            RgbaImage::solid(dominant_color)
        }
    }
}

#[cfg(test)]
mod tests;
