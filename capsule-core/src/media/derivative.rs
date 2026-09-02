//! Still-derivative tiers, the closed format set, and the signed manifests over them.
//!
//! SSoT for the tiers and the formats: [Thumbnails and Previews](https://docs/design/thumbnails/).
//! This module owns the Capsule-side half — sizing, the closed enum, and building + signing a
//! [`DerivativeManifest`] through the same two-signature path assets use
//! ([`DerivativeCore::sign`]) — while `rawshift-image` owns the byte encode.
//!
//! # The closed format set, and where it is enforced
//!
//! [`DerivativeFormat`] is the tier table's format column as a closed enum. `format` is a
//! `String` in the signed struct and **stays** one, deliberately: the same field carries
//! `embedding/{model_id}` for embedding-role manifests
//! ([`crate::ml`]), so a still-only enum cannot be its type; and a `try_from` newtype would make
//! an *older* manifest carrying a future codec fail at deserialisation, turning a policy
//! rejection into a parse error before any signature is examined. The closed set is therefore
//! enforced at the two boundaries the contract names:
//!
//! - **production** — [`generate_still_derivatives`] only ever writes
//!   [`DerivativeFormat::mime`], so no other value can be authored here;
//! - **verification** — [`verify_still_format`] rejects a still-role manifest whose `format`
//!   does not parse, which is the structural rejection the tier table specifies.
//!
//! # What this build encodes
//!
//! **JXL only, and losslessly.** `image/jxl` is the tier table's committed *master* format, so
//! the format that ships first is the one the table already puts first — but the pure-Rust
//! backend is `zune-jpegxl`'s `JxlSimpleEncoder`, which is lossless, so the tier's `q=50` is
//! passed through and ignored and a thumbnail costs more bytes than the table intends. A lossy
//! JXL needs C libjxl (`bindgen` + `pkg-config`).
//!
//! WebP — the table's last-resort delivery variant, and the obvious cheap lossy encoder — is
//! **not available at all**: `rawshift-image` 0.1.1's WebP module does not compile for aarch64
//! (it passes `*const i8` where `libwebp-sys` declares `*const c_char`, and `c_char` is `u8`
//! there), and every mobile target Capsule ships is aarch64. AVIF needs `nasm` on every x86_64
//! build host. Each is recorded as a per-`(tier, format)` deferral on
//! [`StillDerivatives::deferred`] and warned once, so the gap is countable rather than
//! invisible.
//!
//! [`DerivativeManifest`]: crate::crypto::provenance::DerivativeManifest
//! [`DerivativeCore::sign`]: crate::crypto::provenance::manifest::DerivativeCore::sign

use std::fmt;

use rawshift_image::core::image::RgbImage;
use rawshift_image::core::metadata::ImageMetadata;
use rawshift_image::core::{BitDepth, ColorSpace, MetadataEmbedOptions};
use rawshift_image::formats::encode_rgb_image_to_vec;
use rawshift_image::formats::export::{CommonEncodeOptions, EncodeOptions, ZuneJxlEncodeConfig};
use uuid::Uuid;

use super::decode::DecodedImage;
use super::error::MediaError;
use super::resize::downscale_rgba8;
use crate::cbor;
use crate::crypto::CryptoError;
use crate::crypto::encryption::stream::NONCE_PREFIX_LEN;
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::{AmkVersion, Signer};
use crate::crypto::provenance::manifest::{DERIVATIVE_MANIFEST_VERSION, DerivativeCore};
use crate::crypto::provenance::{DerivativeManifest, DerivativeRole};
use crate::lqip::RgbaImage;

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

/// A derivative tier from the tier table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DerivativeTier {
    /// Grid display: long edge capped at 256 px, q=50.
    Thumbnail,
    /// Lightbox / single-asset view: source resolution, q=70. **Not generated by this build**
    /// — a source-resolution derivative is only worth its bytes in the master codec, and the
    /// master codec is the half that is still blocked on a toolchain. Kept in the enum because
    /// the tier table commits to it and the sizing rule is the contract.
    Preview,
}

impl DerivativeTier {
    /// The tiers this build actually generates.
    pub const GENERATED: [Self; 1] = [Self::Thumbnail];

    /// The provenance role this tier records.
    pub const fn role(self) -> DerivativeRole {
        match self {
            Self::Thumbnail => DerivativeRole::Thumbnail,
            Self::Preview => DerivativeRole::Preview,
        }
    }

    /// The role's on-disk name — mirrors
    /// [`derivative_role_name`](crate::lifecycle) in the upload bundle reader, which finds a
    /// derivative's bytes by this prefix.
    pub const fn role_name(self) -> &'static str {
        match self {
            Self::Thumbnail => "thumbnail",
            Self::Preview => "preview",
        }
    }

    /// Long-edge cap in pixels, or `None` to keep the source resolution. The 1080p cap in the
    /// tier table governs the *video* preview transcode (slice `S-B5`), not the still preview.
    pub const fn max_long_edge(self) -> Option<u32> {
        match self {
            Self::Thumbnail => Some(256),
            Self::Preview => None,
        }
    }

    /// Lossy encoder quality for this tier, on the 0..=100 scale every backend here uses.
    pub const fn quality(self) -> f32 {
        match self {
            Self::Thumbnail => 50.0,
            Self::Preview => 70.0,
        }
    }
}

impl fmt::Display for DerivativeTier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.role_name())
    }
}

/// What a derivative's signed manifest has to commit to about its **ciphertext**.
///
/// Derivative bytes cross the network encrypted, exactly like the original
/// ([Encryption](https://docs/design/cryptography/encryption/) — "every asset — original bytes,
/// derivative bytes, metadata blob — is encrypted client-side"), so the content address a
/// manifest signs is the address of the *ciphertext*, and the nonce prefix that produced it is
/// signed alongside because it also selects the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealedDerivative {
    /// SHA-256 over the derivative ciphertext.
    pub ciphertext_hash: Hash32,
    /// The STREAM nonce prefix the ciphertext was produced under.
    pub nonce_prefix: [u8; NONCE_PREFIX_LEN],
}

/// The encryption seam between `media` and `lifecycle`.
///
/// `media` owns pixels and knows nothing about album keys; `lifecycle` owns the AMK and must
/// not own codecs. A derivative's manifest cannot be signed until its ciphertext exists — the
/// hash is *of* the ciphertext — so the encryption has to happen inside generation, and this is
/// the narrowest thing that lets it: one method, no key material in any `media` signature.
///
/// Each call must draw a **fresh** nonce prefix, so two derivatives of one asset get distinct
/// keys and distinct ciphertexts even when their plaintext is identical.
pub trait DerivativeSealer {
    /// Encrypt `plaintext` under the source asset's identity and epoch, returning what the
    /// manifest commits to. The ciphertext itself is discarded: clients keep the plaintext
    /// derivative locally and re-derive the ciphertext at push time from the recorded prefix,
    /// exactly as they do for the original.
    ///
    /// # Errors
    /// [`MediaError::Encode`] when the encryption refuses (a drawn prefix that collides with
    /// the one being replaced).
    fn seal(&self, plaintext: &[u8]) -> Result<SealedDerivative, MediaError>;
}

/// Everything the manifest signer needs that the pixels do not carry: the asset identity, the
/// epoch/authorisation context, and the two signing keys that produce the manifest's two hybrid
/// signatures.
pub struct DerivativeContext<'a> {
    /// The asset the derivatives are generated from.
    pub source_asset_id: Uuid,
    /// Primitive bundle in force.
    pub crypto_suite_id: u16,
    /// Date-based wire protocol version (matches the album pin).
    pub protocol_version: String,
    /// The AMK epoch whose write-tier key signs the manifests.
    pub amk_version: AmkVersion,
    /// Device that generated the derivatives.
    pub generated_by_device: Uuid,
    /// Generating client version string.
    pub generated_by_client: String,
    /// RFC 3339 generation time (audit-only).
    pub generated_at: String,
    /// The device DSK (provenance signature); may be hardware-backed.
    pub device_signer: &'a dyn Signer,
    /// The per-epoch write-tier key (authorisation signature).
    pub write_tier_signer: &'a dyn Signer,
    /// Encrypts each generated derivative so its manifest can commit to the ciphertext.
    pub sealer: &'a dyn DerivativeSealer,
    /// What the **original**'s own manifest committed to. The `original` sentinel generates no
    /// bytes and encrypts nothing: it is a reference to the original blob, so it signs the
    /// original's ciphertext address and the original's nonce prefix, and a receiver resolves it
    /// against the blob it already has.
    pub original: SealedDerivative,
}

/// One generated derivative: the encoded bytes plus its signed manifest.
#[derive(Debug, Clone)]
pub struct GeneratedDerivative {
    /// Which tier this is.
    pub tier: DerivativeTier,
    /// Which committed format, or the `Original` sentinel.
    pub format: DerivativeFormat,
    /// The derivative bytes — the encoder output, and **empty** for
    /// [`DerivativeFormat::Original`], whose manifest is a reference to the original rather than
    /// a copy of it.
    pub bytes: Vec<u8>,
    /// The signed manifest binding `hash(bytes)`, the role and the format.
    pub manifest: DerivativeManifest,
}

/// The outcome of one asset's still-derivative generation.
///
/// `deferred` is the per-`(tier, format)` gap S-B13 asks for: a pair with no encoder in this
/// build is *recorded*, not collapsed into the asset-level status. The asset is still
/// `DerivativeStatus::Decoded` — the decode succeeded and a renderable derivative exists —
/// which is why the two live in different places.
#[derive(Debug, Clone, Default)]
pub struct StillDerivatives {
    /// The derivatives that were produced, in generation order.
    pub generated: Vec<GeneratedDerivative>,
    /// The `(tier, format)` pairs the tier table commits to and this build cannot encode.
    pub deferred: Vec<(DerivativeTier, DerivativeFormat)>,
}

/// Generate the committed still derivatives for `decoded` across `tiers`, signing a
/// [`DerivativeManifest`] for each.
///
/// Per tier:
/// - if the tier caps the long edge and the source is **not larger** than the cap, a single
///   `format = "original"` manifest is signed as a *reference* to the original blob (no bytes,
///   nothing encrypted, committing to [`DerivativeContext::original`]) — the
///   redundant-derivative sentinel from the contract, never a re-encode;
/// - otherwise the frame is downscaled to the tier, encoded to each encodable format of
///   [`DerivativeFormat::STILL_DELIVERY_ORDER`], and **encrypted** through
///   [`DerivativeContext::sealer`] before its manifest is signed over the ciphertext's address;
///   the formats with no encoder here are recorded as deferrals.
///
/// Manifests of the same role are hash-chained in generation order, so a role's derivative
/// provenance is append-only exactly like the asset's.
///
/// # Errors
/// [`MediaError::Encode`] when a codec refuses the frame, and [`MediaError::ZeroDimension`] for
/// an empty source. A signing failure (a hardware device signer refusing) and a sealing failure
/// both surface as [`MediaError::Encode`] too, carrying the crypto error's message.
#[tracing::instrument(
    level = "debug",
    skip_all,
    fields(asset_id = %ctx.source_asset_id, tiers = tiers.len())
)]
pub fn generate_still_derivatives(
    decoded: &DecodedImage,
    tiers: &[DerivativeTier],
    ctx: &DerivativeContext<'_>,
) -> Result<StillDerivatives, MediaError> {
    if decoded.width() == 0 || decoded.height() == 0 {
        return Err(MediaError::ZeroDimension {
            width: decoded.width(),
            height: decoded.height(),
        });
    }

    let mut out = StillDerivatives::default();
    let source_long_edge = decoded.width().max(decoded.height());

    for &tier in tiers {
        // Each tier records a distinct role, so its manifests form their own chain.
        let mut prior: Option<Hash32> = None;

        if let Some(cap) = tier.max_long_edge()
            && source_long_edge <= cap
        {
            tracing::debug!(
                asset_id = %ctx.source_asset_id,
                %tier,
                source_long_edge,
                cap,
                "media: source is within the tier cap; signing the `original` sentinel"
            );
            // A reference to the original blob: no bytes of its own, and **nothing encrypted**
            // — it commits to the original's own ciphertext address and nonce prefix, and the
            // receiver resolves it against the blob it already holds. See
            // `DerivativeFormat::Original`.
            out.generated.push(sign_derivative(
                ctx,
                tier,
                DerivativeFormat::Original,
                Vec::new(),
                ctx.original,
                &mut prior,
            )?);
            continue;
        }

        let work = match tier.max_long_edge() {
            Some(cap) => downscale_rgba8(&decoded.image, cap),
            None => decoded.image.clone(),
        };
        for format in DerivativeFormat::STILL_DELIVERY_ORDER {
            if !format.is_encodable() {
                tracing::warn!(
                    asset_id = %ctx.source_asset_id,
                    %tier,
                    %format,
                    "media: no encoder for this (tier, format) in this build; the tier still \
                     ships its encodable variants and this pair is backfillable (S-B1 remainder)"
                );
                out.deferred.push((tier, format));
                continue;
            }
            let bytes = encode(&work, format, tier)?;
            // Encrypt before signing: the manifest's content address is the ciphertext's, so
            // the ciphertext has to exist first. A fresh prefix per derivative, so two
            // derivatives of one asset never share a key even for identical plaintext.
            let sealed = ctx.sealer.seal(&bytes)?;
            out.generated.push(sign_derivative(
                ctx, tier, format, bytes, sealed, &mut prior,
            )?);
        }
    }

    tracing::debug!(
        asset_id = %ctx.source_asset_id,
        generated = out.generated.len(),
        deferred = out.deferred.len(),
        "media: still derivatives generated"
    );
    Ok(out)
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

/// Encode a tier-sized RGBA8 frame to `format`.
///
/// **Every encode passes [`MetadataEmbedOptions::none`] and an empty [`ImageMetadata`]**, and
/// both are load-bearing rather than tidy: the crate's own default is `all()`, so a
/// default-configured encode copies the source's EXIF — GPS fix included — into the derivative
/// bytes, and the JXL backend has a working `append_to_jxl` that would do exactly that. A
/// thumbnail is the derivative most likely to be served widest, so leaking a home address into
/// it would be the worst possible place for that default to win. Passing empty metadata means
/// the source's block is never even read. A test asserts the absence rather than trusting this
/// comment.
fn encode(
    frame: &RgbaImage,
    format: DerivativeFormat,
    tier: DerivativeTier,
) -> Result<Vec<u8>, MediaError> {
    let options = match format {
        DerivativeFormat::Jxl => EncodeOptions::JxlZune(ZuneJxlEncodeConfig {
            common: CommonEncodeOptions {
                metadata: MetadataEmbedOptions::none(),
                bit_depth: BitDepth::Eight,
            },
            // The tier table's number, passed through as declared even though the backend
            // ignores it: `JxlSimpleEncoder` is lossless, so today this is advisory. Keeping the
            // contract's value here rather than hard-coding `0.0` (the crate's explicit
            // "lossless" request) makes the eventual libjxl swap a backend change and not a
            // quality decision taken again from scratch.
            quality: tier.quality(),
            // Encoder effort, 1..=9; the simple encoder may ignore this too. 7 is the crate's
            // own default and there is no reason to differ.
            effort: 7,
        }),
        // Unreachable: `is_encodable` gates the call, and `Original` never routes here at all
        // (it carries no bytes). Kept as a typed refusal rather than a panic so a future
        // widening that forgets an arm degrades to a reported failure. Reported as
        // `Encode { format }` and not `UnsupportedFormat`, because the latter names a
        // `StillFormat` and there is no still format at fault here — the caller asked for a
        // *derivative* format this build cannot write, and the message has to say which.
        DerivativeFormat::WebP | DerivativeFormat::Avif | DerivativeFormat::Original => {
            return Err(MediaError::Encode {
                format,
                detail: "this build links no encoder for this derivative format".to_string(),
            });
        }
    };

    let rgb = to_rgb_u16(frame);
    encode_rgb_image_to_vec(&rgb, &ImageMetadata::default(), &options).map_err(|e| {
        MediaError::Encode {
            format,
            detail: e.to_string(),
        }
    })
}

/// Widen packed RGBA8 to the interleaved RGB `u16` the encoders take, dropping alpha.
///
/// `v * 257` is the crate's own widening, and the encoders narrow it back with `v >> 8`, so an
/// 8-bit frame reaches the codec bit-for-bit. Alpha is dropped because the decode path already
/// flattened it — every frame here is opaque.
fn to_rgb_u16(frame: &RgbaImage) -> RgbImage {
    let mut data = Vec::with_capacity(frame.rgba.len() / 4 * 3);
    for px in frame.rgba.chunks_exact(4) {
        data.push(u16::from(px[0]) * 257);
        data.push(u16::from(px[1]) * 257);
        data.push(u16::from(px[2]) * 257);
    }
    RgbImage::with_color_space(frame.width, frame.height, data, ColorSpace::Srgb)
}

/// Build, sign and chain one derivative manifest.
///
/// `bytes` is the **plaintext** the client keeps on disk; `sealed` is what the manifest actually
/// commits to — the ciphertext's content address and the nonce prefix that produced it. The two
/// are separate arguments because they are separate artefacts: only one of them ever crosses the
/// network, and only the other is ever painted locally.
///
/// `pub(super)` so the module's tests can exercise the chaining directly. That is not test
/// convenience for its own sake: today exactly one still format is encodable, so a single call
/// to [`generate_still_derivatives`] produces one manifest per role and the multi-link case —
/// the part of the chain that can actually be wrong — is unreachable through the public entry
/// point until a second encoder lands.
pub(super) fn sign_derivative(
    ctx: &DerivativeContext<'_>,
    tier: DerivativeTier,
    format: DerivativeFormat,
    bytes: Vec<u8>,
    sealed: SealedDerivative,
    prior: &mut Option<Hash32>,
) -> Result<GeneratedDerivative, MediaError> {
    let core = DerivativeCore {
        version: DERIVATIVE_MANIFEST_VERSION.into(),
        crypto_suite_id: ctx.crypto_suite_id,
        protocol_version: Some(ctx.protocol_version.clone()),
        amk_version: Some(ctx.amk_version),
        source_asset_id: ctx.source_asset_id,
        role: tier.role(),
        format: format.mime().into(),
        // The address of the **ciphertext**, not of `bytes`: `bytes` is the plaintext kept
        // locally, and what a receiver content-addresses is what crossed the network.
        ciphertext_hash: sealed.ciphertext_hash,
        nonce_prefix: sealed.nonce_prefix,
        generated_by_device: ctx.generated_by_device,
        generated_by_client: ctx.generated_by_client.clone(),
        model_id: None,
        model_version: None,
        generated_at: ctx.generated_at.clone(),
        prior_provenance_hash: *prior,
    };
    let manifest = core
        .sign(ctx.device_signer, ctx.write_tier_signer)
        .map_err(|e: CryptoError| MediaError::Encode {
            format,
            detail: format!("signing the derivative manifest: {e}"),
        })?;
    // The next manifest of this role chains to this one: SHA-256 over its canonical CBOR,
    // signatures included — the same content-hash link the asset provenance chain uses.
    *prior = Some(hash::hash_bytes(
        &cbor::to_canonical_vec(&manifest).map_err(|e| MediaError::Encode {
            format,
            detail: format!("serialising the derivative manifest: {e}"),
        })?,
    ));
    Ok(GeneratedDerivative {
        tier,
        format,
        bytes,
        manifest,
    })
}
