//! Still-image decode, orientation, metadata normalisation and derivative generation — the
//! Capsule-side owner of the media pipeline (slices `S-B1`, `S-B13`).
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
//! pipeline, the app FFI and `capsule-wasm` share one implementation (slice `S-B14`). This
//! module produces the pixels [`crate::lqip::Lqip::encode`] consumes.
//!
//! # What this build can and cannot do
//!
//! Every gap is a typed
//! [`UnsupportedFormat`](crate::media::MediaError::UnsupportedFormat) or a recorded per-format
//! deferral — never a silent absence, and never a panic (slice `S-B13`).
//!
//! **Decode** covers JPEG, PNG, JXL, TIFF and GIF, the ARW and DNG RAW families (issue #589,
//! through `rawshift-image`'s RAW pipeline rather than its standard-image one; see
//! `media::decode`), plus AVIF under the opt-in `media-avif-decode` feature, which links a
//! system libdav1d and which the CLI enables on every target but Windows (issue #588). An ARW
//! or DNG variant upstream has no decoder for — Sony's lossy compressed ARW, a lossless-JPEG
//! DNG — defers like a missing codec. **Encode** depends on the opt-in
//! `media-linked-codecs` feature, which the CLI enables and the mobile FFI does not. With it,
//! the thumbnail and preview tiers are encoded as **lossy** JXL (libjxl) and AVIF (ravif).
//! Without it, only the thumbnail is encoded, as JXL alone and losslessly: the pure-Rust backend
//! is `zune-jpegxl`'s `JxlSimpleEncoder`, so the tier's declared `q=50` is advisory there.
//!
//! HEIC, WebP and the other five RAW families (CR2, CR3, CRW, NEF, RAF) — and AVIF without
//! `media-avif-decode` — sniff correctly and refuse to decode. HEIC is refused by decision: libheif and libde265 are LGPL-3.0 and excluded
//! by policy, and no permissive backend exists in `rawshift-image`. **WebP is a compile failure
//! rather than a missing
//! toolchain**: `rawshift-image`'s WebP module passes `*const i8` where `libwebp-sys` declares
//! `*const c_char`, which is `u8` on aarch64 — every mobile target — and the module is compiled
//! by decode *or* encode, so there is no decode-only escape.
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
pub(crate) use self::decode::guarded;
pub use self::decode::{DecodedImage, Decoder, MediaMetadata, RawshiftDecoder, decode_guarded};
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
