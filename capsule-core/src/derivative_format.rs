//! The closed set of committed still-derivative formats, and the structural check over it.
//!
//! SSoT: [Thumbnails and Previews](https://docs/design/thumbnails/) — the tier table's format
//! column *is* this enum, and "every receiver (and every federated peer) compares
//! `DerivativeManifest.format` against this list" is
//! [`verify_still_format`](crate::derivative_format::verify_still_format).
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
//! [`crate::crypto::provenance`], which is itself unconditional for the same reason. This module
//! is the one public path to both names: `media` does not re-export them.
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
    /// **JPEG XL** — the committed primary/master still codec, encodable in every `media` build:
    /// **lossy** through libjxl under the opt-in `media-linked-codecs` feature, and losslessly
    /// through the pure-Rust `zune-jpegxl` `JxlSimpleEncoder` without it.
    Jxl,
    /// **AVIF** — the universal delivery format for clients without a JXL decoder. Encodable only
    /// under `media-linked-codecs` (`ravif` -> rav1e, which needs `nasm` on an x86_64 host).
    Avif,
    /// **WebP** — the last-resort delivery fallback. Not encodable in this build: the crate's
    /// WebP codec does not compile for aarch64 — see `media::StillFormat::WebP`, which cannot be
    /// linked from here because `media` is feature-gated and this module is not.
    WebP,
    /// The recognised `format = "original"` sentinel: the tier **references** the original asset
    /// rather than generating a redundant derivative, because the source is not larger than the
    /// tier's cap. **Distinct from an absent derivative** — this is an explicit, signed marker,
    /// where absence means "rebuildable from the original".
    ///
    /// A sentinel derivative carries **no bytes of its own** (`media::GeneratedDerivative::bytes`
    /// is empty). "References" is the operative word in the contract: the signed manifest's
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
    ///
    /// Feature-dependent on purpose: the answer is about *this build*, and the AVIF encoder is
    /// linked only by `media-linked-codecs`. The mobile FFI and the `rust-cross` targets build
    /// without it, and there AVIF is a recorded per-`(tier, format)` deferral rather than a gap.
    pub const fn is_encodable(self) -> bool {
        match self {
            Self::Jxl | Self::Original => true,
            Self::Avif => cfg!(feature = "media-linked-codecs"),
            Self::WebP => false,
        }
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
/// `media::MediaError::UnsupportedFormat` — which carries the still format Capsule *would* have
/// needed — is not what an unrecognised value produces, because there is no still format to name
/// (and this module cannot reference `media` in any case: it is unconditional and `media` is
/// not). An unrecognised still-role format is `Err(format.to_string())`.
pub fn verify_still_format(
    manifest: &DerivativeManifest,
) -> Result<Option<DerivativeFormat>, String> {
    verify_still_role_format(manifest.core.role, &manifest.core.format)
}

/// The same closed-set check over a bare `(role, format)` pair — the **key-free** entry point.
///
/// [`verify_still_format`] takes a signed [`DerivativeManifest`], which a key-free receiver
/// (the server's upload gate) never holds: it sees the unencrypted envelope projection, which
/// declares the derivative's role and format as two plain fields. Both entry points answer
/// identically for the same pair, because [`verify_still_format`] is this function applied to
/// the manifest's own fields — there is one closed set and one check over it.
///
/// # Errors
/// An unrecognised still-role format is `Err(format.to_string())`, exactly as
/// [`verify_still_format`] reports it.
pub fn verify_still_role_format(
    role: DerivativeRole,
    format: &str,
) -> Result<Option<DerivativeFormat>, String> {
    match role {
        DerivativeRole::Thumbnail | DerivativeRole::Preview => DerivativeFormat::parse(format)
            .map(Some)
            .ok_or_else(|| format.to_owned()),
        // Not a still. The embedding-role format grammar belongs to `crate::ml`.
        DerivativeRole::Embedding => Ok(None),
    }
}

/// Whether an imported asset got thumbnail/preview derivatives — and if not, **why**
///.
///
/// Capsule is a backup tool, so this never gates admission: the original is imported as a
/// signed, encrypted, verifiable asset in every case below. What varies is only whether a
/// thumbnail/preview and an LQIP could be produced alongside it. The point of the enum is to
/// keep the two "no derivative" reasons apart, because one is expected and one is a bug:
/// a missing codec is a known, deferred gap, while a *supported* format that fails to decode
/// is a real problem someone should look at.
///
/// It lives beside the format set rather than with the import pipeline that produces it because
/// a drop adoption reports it too ([`AdoptedDrop`](crate::drop::AdoptedDrop)), and the drop
/// types are part of the stateless core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DerivativeStatus {
    /// The still decoded: `dimensions` and `lqip` came from real pixels, and the derivatives
    /// this build can encode were generated and signed. **Independent of how many *formats*
    /// deferred** — a decoded still whose AVIF and WebP variants have no encoder here is still
    /// `Decoded`, because it has a renderable JXL thumbnail. The per-format gap is counted
    /// separately, by the import summary's deferred-format count.
    Decoded,
    /// **Expected deferral.** This build links no codec for the asset's format — see the media
    /// stack's supported still formats for what it does link. The original is safely backed up;
    /// dimensions fall back to EXIF and there is no LQIP or thumbnail until the codec lands, at
    /// which point derivatives can be backfilled from the stored original. Counted by the import
    /// summary's deferred-derivative count.
    ///
    /// What reaches here today: HEIC, AVIF and the RAW families, each needing a system library
    /// (libheif, libdav1d) or an assembler the cross and cargo-ndk builds do not carry, and
    /// every video, whose derivatives no build generates yet.
    DeferredNoCodec,
    /// **A real problem.** The format *is* one this build can decode, but these particular
    /// bytes did not decode — truncation, corruption, or a decoder bug. The original is still
    /// imported (the bytes are backed up verbatim, whatever they are), but this is worth
    /// investigating rather than shrugging at.
    DecodeFailed,
    /// Nothing to decode: neither the bytes' header nor the extension names a still image
    /// Capsule models and there is no derivative to make — an XMP sidecar, an SVG, a document,
    /// or an unknown suffix. Distinct from [`DeferredNoCodec`](Self::DeferredNoCodec), which is
    /// a still whose codec is merely absent and whose derivatives are therefore backfillable.
    /// A **video** is not this: its derivatives are committed to by the tier table and deferred
    /// until their codec lands, so a video import reports `DeferredNoCodec`.
    NotAKnownStill,
}

impl DerivativeStatus {
    /// Whether the asset was imported *without* thumbnail/preview derivatives because no codec
    /// exists for it — the deferred-gap count, excluding genuine decode failures and non-stills.
    pub const fn is_deferred_for_missing_codec(self) -> bool {
        matches!(self, Self::DeferredNoCodec)
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

    /// The key-free pair check is the closed set itself: every still role accepts exactly the
    /// recognised formats, refuses everything else by naming it, and an embedding role is out of
    /// the set's scope rather than a violation of it.
    #[test]
    fn the_key_free_pair_check_accepts_the_closed_set_and_names_the_rest() {
        for role in [DerivativeRole::Thumbnail, DerivativeRole::Preview] {
            for format in [
                DerivativeFormat::Jxl,
                DerivativeFormat::Avif,
                DerivativeFormat::WebP,
                DerivativeFormat::Original,
            ] {
                assert_eq!(
                    verify_still_role_format(role, format.mime()),
                    Ok(Some(format))
                );
            }
            for unknown in ["image/future-codec", "image/jpeg", "IMAGE/JXL", ""] {
                assert_eq!(
                    verify_still_role_format(role, unknown),
                    Err(unknown.to_owned()),
                    "{role:?} must refuse {unknown:?}"
                );
            }
        }
        assert_eq!(
            verify_still_role_format(DerivativeRole::Embedding, "embedding/mobileclip-b"),
            Ok(None)
        );
        assert_eq!(
            verify_still_role_format(DerivativeRole::Embedding, "image/future-codec"),
            Ok(None),
            "the embedding grammar is not this set's to judge"
        );
    }
}
