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
//!   models as a still) and [`DerivativeFormat`](crate::media::DerivativeFormat) (what a signed
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
//! pipeline, the uniffi FFI and `capsule-wasm` share one implementation (slice `S-B14`). This
//! module produces the pixels [`crate::lqip::Lqip::encode`] consumes.
//!
//! # What this build can and cannot do
//!
//! Every gap is a typed
//! [`UnsupportedFormat`](crate::media::MediaError::UnsupportedFormat) or a recorded per-format
//! deferral — never a silent absence, and never a panic (slice `S-B13`).
//!
//! **Decode** covers JPEG, PNG, JXL, TIFF and GIF. **Encode** covers JXL alone, and losslessly:
//! `image/jxl` is the tier table's committed master format, but the pure-Rust backend is
//! `zune-jpegxl`'s `JxlSimpleEncoder`, so the tier's declared `q=50` is advisory today.
//!
//! HEIC, AVIF, WebP and the RAW families sniff correctly and refuse to decode. HEIC and AVIF
//! need system libraries (libheif, libdav1d), AVIF encode needs an assembler (nasm) the cross
//! and cargo-ndk builds do not have, and **WebP is a compile failure rather than a missing
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

pub(crate) use self::decode::guarded;
pub use self::decode::{DecodedImage, Decoder, MediaMetadata, RawshiftDecoder, decode_guarded};
pub use self::derivative::{
    DerivativeContext, DerivativeSealer, DerivativeTier, GeneratedDerivative, SealedDerivative,
    StillDerivatives, generate_still_derivatives,
};
pub use self::detect::{MAX_DECODE_PIXELS, SUPPORTED_STILL_FORMATS, StillFormat};
pub use self::error::{FormatOp, MediaError};
pub use self::resize::{capped_dimensions, downscale_rgba8};
// Re-exported so `media::DerivativeFormat` keeps resolving, but *owned* by the unconditional
// module: the closed set has to be linkable by the crates that receive a manifest, and they
// build without this feature. See [`crate::derivative_format`].
pub use crate::derivative_format::{DerivativeFormat, verify_still_format};

#[cfg(test)]
mod tests;
