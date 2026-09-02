//! The closed set of committed still-derivative formats, and the structural check over it.
//!
//! SSoT: [Thumbnails and Previews](https://docs/design/thumbnails/) — the tier table's format
//! column *is* this enum, and "every receiver (and every federated peer) compares
//! `DerivativeManifest.format` against this list" is [`verify_still_format`].
//!
//! # Why this is at the crate root and not in `capsule_core::media`
//!
//! It was in `media` first, and that was a placement mistake rather than a constraint. `media`
//! is behind the `media` feature that `native` implies, so `capsule-server` and `capsule-wasm`
//! — both `default-features = false` — cannot link it. Those are exactly the two crates that
//! *receive* a manifest they did not author, which is where a structural rejection has to run.
//! A closed set only a producer can evaluate is not a closed set.
//!
//! So it lives here, beside nothing and depending on nothing but
//! [`crate::crypto::provenance`], which is itself unconditional for the same reason. `media`
//! re-exports both names, so every existing `media::DerivativeFormat` path still resolves.
//!
//! This mirrors [`crate::lqip`]: a contract every surface needs cannot live inside a
//! feature-gated stack, however natural the stack looks as a home.

use std::fmt;

use crate::crypto::provenance::{DerivativeManifest, DerivativeRole};

/// The closed set of committed still-derivative formats — the tier table's format column.
///
/// The wire value is [`mime`](Self::mime), carried in `DerivativeManifest.format`. A value
/// outside this set is a structural rejection, never a "future format to ignore".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DerivativeFormat {
    /// **JPEG XL** — the committed primary/master still codec, and the one format this build
    /// encodes. Losslessly: the pure-Rust backend is `zune-jpegxl`'s `JxlSimpleEncoder`.
    Jxl,
    /// **AVIF** — the universal delivery format for clients without a JXL decoder. Not
    /// encodable in this build.
    Avif,
    /// **WebP** — the last-resort delivery fallback. Not encodable in this build: the crate's
    /// WebP codec does not compile for aarch64 (see [`super::StillFormat::WebP`]).
    WebP,
    /// The recognised `format = "original"` sentinel: the tier **references** the original asset
    /// rather than generating a redundant derivative, because the source is not larger than the
    /// tier's cap. **Distinct from an absent derivative** — this is an explicit, signed marker,
    /// where absence means "rebuildable from the original".
    ///
    /// A sentinel derivative carries **no bytes of its own** ([`GeneratedDerivative::bytes`] is
    /// empty). "References" is the operative word in the contract: the signed manifest's
    /// `ciphertext_hash` content-addresses the original, which the holder already has, so
    /// copying the bytes under a thumbnail's name would duplicate a file sitting two directories
    /// up *and* re-expose the original's EXIF — GPS included — as a derivative blob, where a
    /// re-encoded thumbnail is metadata-free by construction.
    Original,
}

impl DerivativeFormat {
    /// The committed still formats per tier, in delivery-preference order: the JXL master, then
    /// the AVIF -> WebP delivery variants.
    pub const STILL_DELIVERY_ORDER: [Self; 3] = [Self::Jxl, Self::Avif, Self::WebP];

    /// The exact wire string for `DerivativeManifest.format`.
    pub const fn mime(self) -> &'static str {
        match self {
            Self::Jxl => "image/jxl",
            Self::Avif => "image/avif",
            Self::WebP => "image/webp",
            Self::Original => "original",
        }
    }

    /// The on-disk file extension for a persisted derivative of this format. `Original` has
    /// none of its own — it reuses the source asset's.
    pub const fn extension(self) -> Option<&'static str> {
        match self {
            Self::Jxl => Some("jxl"),
            Self::Avif => Some("avif"),
            Self::WebP => Some("webp"),
            Self::Original => None,
        }
    }

    /// Parse a `DerivativeManifest.format` value against the closed set. `None` **is** the
    /// structural rejection.
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "image/jxl" => Some(Self::Jxl),
            "image/avif" => Some(Self::Avif),
            "image/webp" => Some(Self::WebP),
            "original" => Some(Self::Original),
            _ => None,
        }
    }

    /// Whether a `format` string names a currently-recognised still-derivative format — the
    /// exact check a receiver runs.
    pub fn is_recognized(s: &str) -> bool {
        Self::parse(s).is_some()
    }

    /// Whether this build can produce bytes in this format.
    pub const fn is_encodable(self) -> bool {
        matches!(self, Self::Jxl | Self::Original)
    }
}

impl fmt::Display for DerivativeFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.mime())
    }
}

/// The closed-set check a receiver runs on a still-role derivative manifest.
///
/// Returns the parsed format for a `thumbnail` or `preview` manifest whose `format` is in the
/// closed set. An embedding-role manifest is **not** rejected: its `format` is
/// `embedding/{model_id}`, which this set deliberately does not model, so it is reported as
/// [`None`] rather than as a violation.
///
/// # Errors
/// [`MediaError::UnsupportedFormat`] — carrying the still format Capsule *would* have needed —
/// is not what an unrecognised value produces, because there is no [`super::StillFormat`] to
/// name. An unrecognised still-role format is `Err(format.to_string())`.
pub fn verify_still_format(
    manifest: &DerivativeManifest,
) -> Result<Option<DerivativeFormat>, String> {
    let core = &manifest.core;
    match core.role {
        DerivativeRole::Thumbnail | DerivativeRole::Preview => {
            DerivativeFormat::parse(&core.format)
                .map(Some)
                .ok_or_else(|| core.format.clone())
        }
        // Not a still. The embedding-role format grammar belongs to `crate::ml`.
        DerivativeRole::Embedding => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The reason this module is not in `media`.** These tests compile and run under
    /// `--no-default-features`, which is how `capsule-server` and `capsule-wasm` build — the two
    /// crates that receive a `DerivativeManifest` they did not author. A closed set only its
    /// producer can evaluate is not a closed set.
    ///
    /// This test asserts nothing a caller could not, and that is the point: it exists so the
    /// *linkage* is exercised by the `--no-default-features` build rather than assumed.
    #[test]
    fn the_closed_set_is_evaluable_without_the_media_feature() {
        for format in [
            DerivativeFormat::Jxl,
            DerivativeFormat::Avif,
            DerivativeFormat::WebP,
            DerivativeFormat::Original,
        ] {
            assert_eq!(DerivativeFormat::parse(format.mime()), Some(format));
        }
        assert!(!DerivativeFormat::is_recognized("image/future-codec"));
        assert!(!DerivativeFormat::is_recognized("embedding/mobileclip-b"));
    }
}
