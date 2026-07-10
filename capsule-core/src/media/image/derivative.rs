//! Still-image thumbnail/preview derivative generation (slice `S-B1`).
//!
//! This is the **shared manifest-building logic** the [Thumbnails and Previews] contract
//! places in `capsule-core`: format detection, tier resize, and — the load-bearing part —
//! building and signing a [`DerivativeManifest`] for every generated still through the exact
//! same two-signature path assets use ([`DerivativeCore::sign`]). The actual byte encoding to
//! the committed still codecs is a per-platform concern, injected through the [`StillEncoder`]
//! seam (the SDK's per-platform encoder libraries; see the contract's opening note). Video
//! tiers (first-frame still + H.264 preview) are a distinct toolchain and live in slice
//! `S-B5`.
//!
//! The [`DerivativeFormat`] enum is the closed set from the tier table: every receiver (and
//! federated peer) compares `DerivativeManifest.format` against it, and an unknown value is a
//! structural rejection — [`DerivativeFormat::is_recognized`] is that check.
//!
//! [Thumbnails and Previews]: https://docs/design/thumbnails/
//! [`DerivativeManifest`]: crate::crypto::provenance::DerivativeManifest
//! [`DerivativeCore::sign`]: crate::crypto::provenance::DerivativeCore::sign

use thiserror::Error;
use uuid::Uuid;

use crate::cbor;
use crate::crypto::CryptoError;
use crate::crypto::hash::{self, Hash32};
use crate::crypto::keys::{AmkVersion, Signer};
use crate::crypto::provenance::{
    DERIVATIVE_MANIFEST_VERSION, DerivativeCore, DerivativeManifest, DerivativeRole,
};
use crate::media::image::buffer::{ImageBuffer, ImageBufferError};
use crate::media::image::resize_to_max_dimension;

/// The closed set of committed still-derivative formats (SSoT: the Thumbnails doc tier
/// table). The wire value is the MIME string carried in `DerivativeManifest.format`; a value
/// outside this set is a structural rejection, never a "future format to ignore".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivativeFormat {
    /// **JPEG XL** — the committed primary/master still codec (best quality-per-byte).
    Jxl,
    /// **AVIF** — the universal delivery format served to clients lacking a JXL decoder.
    Avif,
    /// **WebP** — the last-resort delivery fallback for the rare client lacking AVIF.
    WebP,
    /// The recognized `format = "original"` sentinel: the tier references the original asset
    /// instead of generating a redundant derivative (source not larger than the tier target).
    /// Distinct from a simply-absent derivative — this is an explicit, signed marker.
    Original,
}

impl DerivativeFormat {
    /// The committed still formats generated per photo tier — the JXL master plus the
    /// AVIF→WebP delivery variants, in delivery-preference order.
    pub const STILL_FORMATS: [DerivativeFormat; 3] = [
        DerivativeFormat::Jxl,
        DerivativeFormat::Avif,
        DerivativeFormat::WebP,
    ];

    /// The exact wire string for `DerivativeManifest.format`.
    pub const fn mime(self) -> &'static str {
        match self {
            DerivativeFormat::Jxl => "image/jxl",
            DerivativeFormat::Avif => "image/avif",
            DerivativeFormat::WebP => "image/webp",
            DerivativeFormat::Original => "original",
        }
    }

    /// Parse a `DerivativeManifest.format` value against the closed set; `None` is a
    /// structural rejection (the closed-enum rule).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "image/jxl" => Some(DerivativeFormat::Jxl),
            "image/avif" => Some(DerivativeFormat::Avif),
            "image/webp" => Some(DerivativeFormat::WebP),
            "original" => Some(DerivativeFormat::Original),
            _ => None,
        }
    }

    /// Whether a `format` string is a currently-recognized still derivative format. This is
    /// the exact check a receiver runs on `DerivativeManifest.format`.
    pub fn is_recognized(s: &str) -> bool {
        Self::parse(s).is_some()
    }
}

/// A derivative tier from the tier table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DerivativeTier {
    /// Grid display: ~256 px long edge.
    Thumbnail,
    /// Lightbox / single-asset view: full still resolution.
    Preview,
}

impl DerivativeTier {
    /// The provenance role this tier records.
    pub const fn role(self) -> DerivativeRole {
        match self {
            DerivativeTier::Thumbnail => DerivativeRole::Thumbnail,
            DerivativeTier::Preview => DerivativeRole::Preview,
        }
    }

    /// Long-edge cap in pixels, or `None` to keep the source resolution. The thumbnail tier is
    /// ~256 px; the still preview tier is generated at the source resolution (the 1080p cap in
    /// the tier table governs the *video* preview transcode, slice `S-B5`).
    pub const fn target_long_edge(self) -> Option<usize> {
        match self {
            DerivativeTier::Thumbnail => Some(256),
            DerivativeTier::Preview => None,
        }
    }
}

/// The per-platform byte-encoding seam. Given an RGBA8 pixel buffer already resized for
/// `tier`, produce the encoded bytes for `format`. `capsule-sdk` implements this over each
/// platform's encoder libraries; `capsule-core` owns only the resize + manifest logic around
/// it. `Original` is never routed here — the pipeline emits the source bytes directly.
pub trait StillEncoder {
    /// Encode `buffer` to `format` for `tier`. The returned bytes are what the manifest's
    /// `ciphertext_hash` binds.
    fn encode(
        &self,
        buffer: &ImageBuffer,
        format: DerivativeFormat,
        tier: DerivativeTier,
    ) -> Result<Vec<u8>, ImageBufferError>;
}

/// Everything the manifest signer needs that is not derived from the pixels: the asset
/// identity, the epoch/authorization context, and the two signing keys (device DSK +
/// per-epoch write-tier key) that produce the manifest's two hybrid signatures.
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
    /// RFC3339 generation time (audit-only).
    pub generated_at: String,
    /// The device DSK (provenance signature); may be hardware-backed.
    pub device_signer: &'a dyn Signer,
    /// The per-epoch write-tier key (authorization signature).
    pub write_tier_signer: &'a dyn Signer,
}

/// One generated derivative: the encoded bytes plus its signed manifest.
#[derive(Debug, Clone)]
pub struct GeneratedDerivative {
    /// Which tier this is.
    pub tier: DerivativeTier,
    /// Which committed format (or the `Original` sentinel).
    pub format: DerivativeFormat,
    /// The derivative bytes (the encoder output, or the original for `Original`).
    pub bytes: Vec<u8>,
    /// The signed derivative manifest binding `hash(bytes)` and the format.
    pub manifest: DerivativeManifest,
}

/// Errors from still-derivative generation.
#[derive(Debug, Error)]
pub enum DerivativeError {
    /// A pixel-buffer resize/conversion failed.
    #[error("image buffer error: {0}")]
    Buffer(#[from] ImageBufferError),
    /// Manifest signing failed (e.g. a hardware device signer refused).
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
}

/// Generate the committed still derivatives for `source` across `tiers`, signing a
/// [`DerivativeManifest`] for each.
///
/// For every tier:
/// - if the tier caps the long edge (thumbnail) and `source` is **not larger** than the cap,
///   a single `format = "original"` manifest is emitted over `original_bytes` — the
///   redundant-derivative fallback from the contract, never a re-encode;
/// - otherwise `source` is resized to the tier (or kept at full resolution for the preview),
///   converted to RGBA8, and encoded to each of [`DerivativeFormat::STILL_FORMATS`] via
///   `encoder`.
///
/// Manifests of the same role are hash-chained (`prior_provenance_hash`) in generation order,
/// so the derivative provenance for a role is an append-only chain exactly like the asset's.
pub fn generate_still_derivatives(
    source: &ImageBuffer,
    original_bytes: &[u8],
    tiers: &[DerivativeTier],
    encoder: &dyn StillEncoder,
    ctx: &DerivativeContext<'_>,
) -> Result<Vec<GeneratedDerivative>, DerivativeError> {
    let mut out = Vec::new();
    let source_long_edge = source.width.max(source.height);

    for &tier in tiers {
        // Each tier records a distinct role, so its manifests form their own chain.
        let mut prior: Option<Hash32> = None;

        match tier.target_long_edge() {
            Some(cap) if source_long_edge <= cap => {
                // Redundant-derivative fallback: reference the original under the signed
                // `format = "original"` sentinel.
                out.push(sign_derivative(
                    ctx,
                    tier,
                    DerivativeFormat::Original,
                    original_bytes,
                    &mut prior,
                )?);
            }
            maybe_cap => {
                let work = match maybe_cap {
                    Some(cap) => {
                        let (w, h) = resize_to_max_dimension(source.width, source.height, cap);
                        source.resize(w, h)?.into_rgba8()?
                    }
                    None => source.to_rgba8()?,
                };
                for &format in &DerivativeFormat::STILL_FORMATS {
                    let bytes = encoder.encode(&work, format, tier)?;
                    out.push(sign_derivative(ctx, tier, format, &bytes, &mut prior)?);
                }
            }
        }
    }
    Ok(out)
}

/// Build, sign, and chain one derivative manifest over `bytes`.
fn sign_derivative(
    ctx: &DerivativeContext<'_>,
    tier: DerivativeTier,
    format: DerivativeFormat,
    bytes: &[u8],
    prior: &mut Option<Hash32>,
) -> Result<GeneratedDerivative, DerivativeError> {
    let core = DerivativeCore {
        version: DERIVATIVE_MANIFEST_VERSION.into(),
        crypto_suite_id: ctx.crypto_suite_id,
        protocol_version: Some(ctx.protocol_version.clone()),
        amk_version: Some(ctx.amk_version),
        source_asset_id: ctx.source_asset_id,
        role: tier.role(),
        format: format.mime().into(),
        ciphertext_hash: hash::hash_bytes(bytes),
        generated_by_device: ctx.generated_by_device,
        generated_by_client: ctx.generated_by_client.clone(),
        model_id: None,
        model_version: None,
        generated_at: ctx.generated_at.clone(),
        prior_provenance_hash: *prior,
    };
    let manifest = core.sign(ctx.device_signer, ctx.write_tier_signer)?;
    // The next manifest of this role chains to this one (SHA-256 over its canonical CBOR,
    // signatures included) — the same content-hash link the asset provenance chain uses.
    *prior = Some(hash::hash_bytes(
        &cbor::to_canonical_vec(&manifest).expect("derivative manifest serializes"),
    ));
    Ok(GeneratedDerivative {
        tier,
        format,
        bytes: bytes.to_vec(),
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::keys::HybridSigningKey;
    use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
    use crate::media::image::buffer::{ComponentType, PixelFormat};
    use crate::media::metadata::ColorSpace;

    /// A deterministic in-test encoder standing in for the SDK's per-platform encoders: it
    /// tags the format with a leading discriminator byte so distinct formats produce distinct
    /// bytes (and thus distinct content hashes), then appends the resized RGBA. Faithful
    /// enough to exercise the resize → hash → sign → chain pipeline; real codec bytes are the
    /// platform's job (see the module docs).
    struct TagEncoder;
    impl StillEncoder for TagEncoder {
        fn encode(
            &self,
            buffer: &ImageBuffer,
            format: DerivativeFormat,
            _tier: DerivativeTier,
        ) -> Result<Vec<u8>, ImageBufferError> {
            let tag: u8 = match format {
                DerivativeFormat::Jxl => 0x4A,
                DerivativeFormat::Avif => 0xAF,
                DerivativeFormat::WebP => 0x7B,
                DerivativeFormat::Original => 0x00,
            };
            let mut v = Vec::with_capacity(buffer.data.len() + 1);
            v.push(tag);
            v.extend_from_slice(&buffer.data);
            Ok(v)
        }
    }

    fn gradient_rgb(width: usize, height: usize) -> ImageBuffer {
        let mut data = Vec::with_capacity(width * height * 3);
        for y in 0..height {
            for x in 0..width {
                data.push((x % 256) as u8);
                data.push((y % 256) as u8);
                data.push(((x + y) % 256) as u8);
            }
        }
        ImageBuffer::new(
            data,
            width,
            height,
            PixelFormat::Rgb,
            ComponentType::U8,
            ColorSpace::Srgb,
        )
        .unwrap()
    }

    fn dev() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
    }
    fn wt() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32])
    }

    fn ctx<'a>(device: &'a HybridSigningKey, write: &'a HybridSigningKey) -> DerivativeContext<'a> {
        DerivativeContext {
            source_asset_id: Uuid::from_u128(0xF11E),
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            amk_version: AmkVersion(1),
            generated_by_device: Uuid::from_u128(0xD1),
            generated_by_client: "capsule-core/0.1.0".into(),
            generated_at: "2026-07-10T00:00:00Z".into(),
            device_signer: device,
            write_tier_signer: write,
        }
    }

    #[test]
    fn generation_produces_committed_still_formats_with_signed_manifests() {
        let (device, write) = (dev(), wt());
        let ctx = ctx(&device, &write);
        let source = gradient_rgb(512, 384); // long edge 512 > 256 → real thumbnails
        let original = b"the-original-jpeg-bytes";

        let out = generate_still_derivatives(
            &source,
            original,
            &[DerivativeTier::Thumbnail, DerivativeTier::Preview],
            &TagEncoder,
            &ctx,
        )
        .unwrap();

        // Two tiers × three committed formats.
        assert_eq!(out.len(), 6);
        for tier in [DerivativeTier::Thumbnail, DerivativeTier::Preview] {
            let got: Vec<DerivativeFormat> = out
                .iter()
                .filter(|d| d.tier == tier)
                .map(|d| d.format)
                .collect();
            assert_eq!(got, DerivativeFormat::STILL_FORMATS.to_vec());
        }

        for d in &out {
            // Format is a recognized committed value and matches the manifest wire string.
            assert!(DerivativeFormat::is_recognized(&d.manifest.core.format));
            assert_eq!(d.manifest.core.format, d.format.mime());
            assert_ne!(d.format, DerivativeFormat::Original);
            // Role mirrors the tier.
            assert_eq!(d.manifest.core.role, d.tier.role());
            // The manifest binds the exact derivative bytes.
            assert_eq!(d.manifest.core.ciphertext_hash, hash::hash_bytes(&d.bytes));
            // Both hybrid signatures verify over the signed core bytes.
            let bytes = d.manifest.core.signing_bytes();
            assert!(
                device
                    .verifying_key()
                    .verify(&bytes, &d.manifest.device_sig)
            );
            assert!(write.verifying_key().verify(&bytes, &d.manifest.write_sig));
        }

        // The thumbnail role's three manifests form an append-only chain.
        let thumbs: Vec<&GeneratedDerivative> = out
            .iter()
            .filter(|d| d.tier == DerivativeTier::Thumbnail)
            .collect();
        assert!(thumbs[0].manifest.core.prior_provenance_hash.is_none());
        for w in thumbs.windows(2) {
            let prev_hash = hash::hash_bytes(&cbor::to_canonical_vec(&w[0].manifest).unwrap());
            assert_eq!(w[1].manifest.core.prior_provenance_hash, Some(prev_hash));
        }
    }

    #[test]
    fn original_fallback_when_source_not_larger_than_thumbnail_tier() {
        let (device, write) = (dev(), wt());
        let ctx = ctx(&device, &write);
        // 128 px long edge ≤ 256 → the thumbnail references the original.
        let source = gradient_rgb(128, 96);
        let original = b"tiny-original";

        let out = generate_still_derivatives(
            &source,
            original,
            &[DerivativeTier::Thumbnail],
            &TagEncoder,
            &ctx,
        )
        .unwrap();

        assert_eq!(out.len(), 1);
        let d = &out[0];
        assert_eq!(d.format, DerivativeFormat::Original);
        assert_eq!(d.manifest.core.format, "original");
        assert!(DerivativeFormat::is_recognized(&d.manifest.core.format));
        assert_eq!(d.bytes, original);
        let bytes = d.manifest.core.signing_bytes();
        assert!(
            device
                .verifying_key()
                .verify(&bytes, &d.manifest.device_sig)
        );
        assert!(write.verifying_key().verify(&bytes, &d.manifest.write_sig));
    }

    #[test]
    fn unknown_derivative_format_is_a_structural_rejection() {
        assert!(DerivativeFormat::parse("image/future-codec").is_none());
        assert!(!DerivativeFormat::is_recognized("image/future-codec"));
        // Every committed value round-trips.
        for f in DerivativeFormat::STILL_FORMATS {
            assert_eq!(DerivativeFormat::parse(f.mime()), Some(f));
        }
        assert_eq!(
            DerivativeFormat::parse("original"),
            Some(DerivativeFormat::Original)
        );
    }
}
