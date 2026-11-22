//! Still-image decode, orientation, metadata normalisation and derivative generation — the
//! Capsule-side owner of the media pipeline.
//!
//! SSoT: [Thumbnails and Previews](https://docs/design/thumbnails/).
//!
//! # Boundaries
//!
//! Rawshift owns codecs; this module owns everything Capsule decides. Concretely,
//! [`rawshift-image`] performs format sniffing, pixel decode, and the byte encode, while this
//! module owns:
//!
//! - **the closed format sets** — [`StillFormat`](crate::media::StillFormat) (what Capsule
//!   models as a still) and [`DerivativeFormat`](crate::derivative_format::DerivativeFormat) (what a signed
//!   `DerivativeManifest.format` may say);
//! - **the pixel budget and the panic guard**, because a third-party pre-1.0 decoder is fed
//!   untrusted bytes on the import path;
//! - **tier sizing and the downscale**, because `rawshift-image` has no resize and because a
//!   derivative's bytes are signed, so the resample must be deterministic;
//! - **the metadata strip**, because the crate's own default embeds EXIF (GPS included) into
//!   every encode.
//!
//! Every path above is reached through the re-exports below, and the doc links name them by
//! their full `crate::media::…` path deliberately. A bare ``[`StillFormat`]`` here does **not**
//! resolve under the `doc-check-rust` gate (`cargo doc --no-deps`, `-D warnings`) even though
//! the type is re-exported a few lines down — while it *does* resolve when the same command is
//! given `--document-private-items`, which is why the failure only appeared in CI. Rather than
//! guess at which of rustdoc's resolution rules produces that asymmetry, these links use the
//! path that resolves under both.
//!
//! LQIP is *not* here: it lives in the unconditional [`crate::lqip`] module so the import
//! pipeline, the app FFI and `capsule-wasm` share one implementation. This
//! module produces the pixels [`crate::lqip::Lqip::encode`] consumes.
//!
//! # What this build can and cannot do
//!
//! Every gap is a typed
//! [`UnsupportedFormat`](crate::media::MediaError::UnsupportedFormat) or a recorded per-format
//! deferral — never a silent absence, and never a panic.
//!
//! **Decode** covers JPEG, PNG, JXL, TIFF and GIF, plus AVIF under the opt-in
//! `media-avif-decode` feature, which the CLI enables (issue #588). AVIF goes through gamut's
//! container parser and decodes its AV1 codestream only where a hardware decoder is present
//! (VAAPI on Linux today); elsewhere an AVIF defers like a missing codec until upstream gains a
//! software AV1 fallback (visualcommons/rawshift#78). **ARW and DNG defer** (issue #1323):
//! `rawshift-image`'s RAW reader decodes a DNG eagerly at open, with no caller budget, so no
//! untrusted RAW is handed to it. Their RAW pipeline (issue #589) and its pre-open bounds stay in
//! `media::decode`, driven by its tests, until issue #1324 re-enables them. **Encode** writes the AVIF variant through
//! gamut's pure-Rust AV1 encoder in every build. The opt-in `media-linked-codecs` feature, which
//! the CLI enables and the mobile FFI does not, adds the **lossy** JXL master (libjxl) and the
//! preview tier; without it only the thumbnail is encoded, as AVIF alone, and the JXL master is
//! a recorded deferral.
//!
//! **Embedded previews** ([`extract_embedded_preview`](crate::media::extract_embedded_preview))
//! are read from every RAW family the build decodes through `RawFile::thumbnail`, under the same
//! IFD walk and pixel budget, and never developed, uploaded or signed. They are a stand-in on
//! the render ladder until an encoded tier exists, not a derivative. While ARW and DNG defer,
//! that is no family: opening one for its preview would decode it first.
//!
//! HEIC, WebP and the other five RAW families (CR2, CR3, CRW, NEF, RAF) — and AVIF without
//! `media-avif-decode` — sniff correctly and refuse to decode. HEIC is refused by decision: libheif and libde265 are LGPL-3.0 and excluded
//! by policy (`rawshift-image`'s gamut HEIC decoder needs a hardware HEVC decoder besides, and
//! is not enabled). **WebP is not enabled**: `rawshift-image`'s WebP is now pure-Rust gamut and
//! compiles everywhere, but enabling it is its own change.
//!
//! [`rawshift-image`]: https://docs.rs/rawshift-image

mod decode;
mod derivative;
mod detect;
mod error;
mod resize;
mod tiff_bounds;

// `native`-gated because `lifecycle` is its only caller and `lifecycle` is `native`-gated: a
// `--features media` build without `native` (which the aarch64 cross-check uses, to isolate the
// codecs from SQLite's C build) would otherwise carry an unused re-export.
#[cfg(feature = "native")]
pub use self::decode::guarded;
pub use self::decode::{
    DecodedImage, Decoder, EmbeddedPreview, MediaMetadata, RawshiftDecoder,
    decode_embedded_preview, decode_guarded, extract_embedded_preview,
};
pub use self::derivative::{
    DerivativeContext, DerivativeSealer, DerivativeTier, GeneratedDerivative, SealedDerivative,
    StillDerivatives, generate_still_derivatives,
};
pub use self::detect::{MAX_DECODE_PIXELS, SUPPORTED_STILL_FORMATS, StillFormat};
pub use self::error::{FormatOp, MediaError};
pub use self::resize::{capped_dimensions, downscale_rgba8};
// `DerivativeFormat` and `verify_still_format` are deliberately *not* re-exported here: they are
// owned by the unconditional `crate::derivative_format`, because the crates that receive a
// manifest build without this feature, and that module is their one public path.

#[cfg(test)]
mod tests;
