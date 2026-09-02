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
//! - **the closed format sets** — [`StillFormat`] (what Capsule models as a still) and
//!   [`DerivativeFormat`] (what a signed `DerivativeManifest.format` may say);
//! - **the pixel budget and the panic guard**, because a third-party pre-1.0 decoder is fed
//!   untrusted bytes on the import path;
//! - **tier sizing and the downscale**, because `rawshift-image` has no resize and because a
//!   derivative's bytes are signed, so the resample must be deterministic;
//! - **the metadata strip**, because the crate's own default embeds EXIF (GPS included) into
//!   every encode.
//!
//! LQIP is *not* here: it lives in the unconditional [`crate::lqip`] module so the import
//! pipeline, the uniffi FFI and `capsule-wasm` share one implementation (slice `S-B14`). This
//! module produces the pixels [`crate::lqip::Lqip::encode`] consumes.
//!
//! # What this build can and cannot do
//!
//! Every gap is a typed [`UnsupportedFormat`](MediaError::UnsupportedFormat) or a recorded
//! per-format deferral — never a silent absence, and never a panic (slice `S-B13`). Decode
//! covers JPEG, PNG, JXL, TIFF, GIF and WebP; encode covers WebP alone. HEIC, AVIF and the RAW families sniff
//! correctly and refuse to decode, because their backends need system libraries (libheif,
//! libdav1d) or an assembler (nasm) that the cross and cargo-ndk builds do not have.
//!
//! [`rawshift-image`]: https://docs.rs/rawshift-image

mod decode;
mod derivative;
mod detect;
mod error;
mod resize;

pub use self::decode::{
    DecodedImage, Decoder, MediaMetadata, RawshiftDecoder, decode_guarded, guarded,
};
pub use self::derivative::{
    DerivativeContext, DerivativeFormat, DerivativeTier, GeneratedDerivative, StillDerivatives,
    generate_still_derivatives, verify_still_format,
};
pub use self::detect::{MAX_DECODE_PIXELS, SUPPORTED_STILL_FORMATS, StillFormat};
pub use self::error::{FormatOp, MediaError};
pub use self::resize::{capped_dimensions, downscale_rgba8};

#[cfg(test)]
mod tests;
