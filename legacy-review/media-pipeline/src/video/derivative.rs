//! Video derivative generation — first-frame still + H.264 baseline preview (slice `S-B5`).
//!
//! This mirrors the still-image path ([`crate::media::image::derivative`], slice `S-B1`):
//! `capsule-core` owns the **orchestration, the closed format set, and the manifest
//! signing/chaining**; the actual transcode toolchain (demux, video decode, H.264/AAC
//! encode, first-frame extraction) is a per-platform concern injected through the
//! [`VideoTranscoder`] seam — exactly as still byte-encoding rides the [`StillEncoder`] seam.
//! No ffmpeg/gstreamer/C toolchain is linked into core; `capsule-sdk` supplies the platform
//! implementation (ffmpeg / AVFoundation / MediaCodec).
//!
//! Both tiers are signed through the exact same two-signature [`DerivativeManifest`] path the
//! stills use ([`DerivativeCore::sign`]), so video derivative provenance is byte-for-byte the
//! same append-only chain:
//!
//! - **Thumbnail tier** — the transcoder extracts the first frame; core resizes it to the
//!   thumbnail long-edge and hands it to the [`StillEncoder`] to encode as the committed
//!   first-frame still formats (JXL + AVIF, per the tier table's video thumbnail row).
//! - **Preview tier** — the transcoder produces an H.264 **baseline** preview under the fixed
//!   [`H264PreviewParams`] the tier table pins (original resolution capped to 1080p, CRF 23,
//!   30 fps cap, AAC audio).
//!
//! The [`VideoDerivativeFormat`] enum is the closed set of the tier table's video rows: a
//! `DerivativeManifest.format` value outside it is a structural rejection
//! ([`VideoDerivativeFormat::is_recognized`]).
//!
//! Contract: [Thumbnails and Previews — Video Previews](https://docs/design/thumbnails/).
//!
//! [`DerivativeManifest`]: crate::crypto::provenance::DerivativeManifest
//! [`DerivativeCore::sign`]: crate::crypto::provenance::DerivativeCore::sign
//! [`StillEncoder`]: crate::media::image::derivative::StillEncoder

use thiserror::Error;

use crate::cbor;
use crate::crypto::CryptoError;
use crate::crypto::hash::{self, Hash32};
use crate::crypto::provenance::{DERIVATIVE_MANIFEST_VERSION, DerivativeCore, DerivativeManifest};
use crate::media::image::buffer::{ImageBuffer, ImageBufferError};
use crate::media::image::derivative::{
    DerivativeContext, DerivativeFormat, DerivativeTier, StillEncoder,
};
use crate::media::image::resize_to_max_dimension;
use crate::media::video::types::AudioCodec;

/// The closed set of committed video-derivative formats (SSoT: the Thumbnails doc tier table,
/// video rows). The wire value is the string carried in `DerivativeManifest.format`; a value
/// outside this set is a structural rejection, never a "future format to ignore".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoDerivativeFormat {
    /// First-frame still, encoded **JXL** (thumbnail tier). Shares the still wire string
    /// `image/jxl` — the frame is an ordinary still image once extracted.
    FirstFrameJxl,
    /// First-frame still, encoded **AVIF** (thumbnail tier). Wire string `image/avif`.
    FirstFrameAvif,
    /// **H.264 baseline** preview transcode in an MP4 container (preview tier). Wire string
    /// `video/mp4`.
    H264Preview,
}

impl VideoDerivativeFormat {
    /// The committed first-frame still formats generated for the video thumbnail tier — JXL +
    /// AVIF, per the tier table's video thumbnail row (no WebP: the video thumbnail row commits
    /// only JXL/AVIF).
    pub const FIRST_FRAME_STILL_FORMATS: [VideoDerivativeFormat; 2] = [
        VideoDerivativeFormat::FirstFrameJxl,
        VideoDerivativeFormat::FirstFrameAvif,
    ];

    /// The exact wire string for `DerivativeManifest.format`.
    pub const fn mime(self) -> &'static str {
        match self {
            VideoDerivativeFormat::FirstFrameJxl => "image/jxl",
            VideoDerivativeFormat::FirstFrameAvif => "image/avif",
            VideoDerivativeFormat::H264Preview => "video/mp4",
        }
    }

    /// The still [`DerivativeFormat`] a first-frame still is encoded to via the [`StillEncoder`]
    /// seam, or `None` for the H.264 preview (which is produced by the transcoder, not the
    /// still encoder).
    pub const fn still_format(self) -> Option<DerivativeFormat> {
        match self {
            VideoDerivativeFormat::FirstFrameJxl => Some(DerivativeFormat::Jxl),
            VideoDerivativeFormat::FirstFrameAvif => Some(DerivativeFormat::Avif),
            VideoDerivativeFormat::H264Preview => None,
        }
    }

    /// Parse a `DerivativeManifest.format` value against the closed video set; `None` is a
    /// structural rejection (the closed-enum rule).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "image/jxl" => Some(VideoDerivativeFormat::FirstFrameJxl),
            "image/avif" => Some(VideoDerivativeFormat::FirstFrameAvif),
            "video/mp4" => Some(VideoDerivativeFormat::H264Preview),
            _ => None,
        }
    }

    /// Whether a `format` string is a currently-recognized video derivative format — the exact
    /// check a receiver runs on `DerivativeManifest.format` for a video asset's derivatives.
    pub fn is_recognized(s: &str) -> bool {
        Self::parse(s).is_some()
    }
}

/// H.264 profile. The tier table pins **baseline** for video previews (universally decodable,
/// cheap to decode on every platform); it is the only profile a compliant preview may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H264Profile {
    /// H.264 baseline profile — the committed preview profile.
    Baseline,
}

/// The **fixed** H.264 preview parameters the tier table pins, expressed as types/constants a
/// real [`VideoTranscoder`] must honor. Core passes [`H264PreviewParams::CONTRACT`] into the
/// transcoder for every preview; the transcoder does not get to choose these — they are the
/// contract, not a suggestion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct H264PreviewParams {
    /// H.264 profile — always [`H264Profile::Baseline`].
    pub profile: H264Profile,
    /// Long-edge cap in pixels: the original resolution is capped to 1080p, i.e. neither
    /// dimension exceeds [`Self::MAX_WIDTH`]×[`Self::MAX_HEIGHT`]. A source already within the
    /// cap keeps its resolution.
    pub max_width: u32,
    /// Short-edge cap in pixels (1080p height).
    pub max_height: u32,
    /// Constant Rate Factor.
    pub crf: u8,
    /// Frame-rate cap in fps: a higher source rate is decimated to this, a lower rate is kept.
    pub max_fps: u32,
    /// Audio codec — always AAC.
    pub audio: AudioCodec,
}

impl H264PreviewParams {
    /// H.264 CRF from the tier table.
    pub const CRF: u8 = 23;
    /// Frame-rate cap (fps) from the tier table.
    pub const MAX_FPS: u32 = 30;
    /// 1080p width cap from the tier table (= [`StandardResolution::Fhd1080p`] width).
    pub const MAX_WIDTH: u32 = 1920;
    /// 1080p height cap from the tier table (= [`StandardResolution::Fhd1080p`] height).
    pub const MAX_HEIGHT: u32 = 1080;

    /// The one and only compliant parameter set: H.264 baseline, 1080p cap, CRF 23, 30 fps cap,
    /// AAC audio. This is what core hands every transcoder — the tier table pinned as a value.
    pub const CONTRACT: H264PreviewParams = H264PreviewParams {
        profile: H264Profile::Baseline,
        max_width: Self::MAX_WIDTH,
        max_height: Self::MAX_HEIGHT,
        crf: Self::CRF,
        max_fps: Self::MAX_FPS,
        audio: AudioCodec::Aac,
    };
}

/// The video source a [`VideoTranscoder`] consumes: the original encoded bytes plus the source
/// pixel dimensions (used to reason about the 1080p cap). Demux/decode of `bytes` is entirely
/// the transcoder's job.
#[derive(Debug, Clone, Copy)]
pub struct VideoSource<'a> {
    /// The original encoded video bytes (any supported container/codec).
    pub bytes: &'a [u8],
    /// Source frame width in pixels.
    pub width: u32,
    /// Source frame height in pixels.
    pub height: u32,
}

/// The per-platform transcode seam — the video analogue of [`StillEncoder`]. `capsule-sdk`
/// implements this over each platform's toolchain (ffmpeg / AVFoundation / MediaCodec);
/// `capsule-core` owns only the tier orchestration and manifest signing around it.
pub trait VideoTranscoder {
    /// Extract and decode the first frame of `source` as a pixel buffer. Core resizes and hands
    /// it to a [`StillEncoder`] for the thumbnail-tier first-frame still.
    fn first_frame(&self, source: &VideoSource<'_>) -> Result<ImageBuffer, VideoTranscodeError>;

    /// Transcode `source` into an H.264 preview honoring `params`. `params` is always
    /// [`H264PreviewParams::CONTRACT`] — the transcoder must satisfy every field (baseline
    /// profile, 1080p cap, CRF 23, 30 fps cap, AAC audio). The returned bytes are what the
    /// manifest's `ciphertext_hash` binds.
    fn transcode_preview(
        &self,
        source: &VideoSource<'_>,
        params: &H264PreviewParams,
    ) -> Result<Vec<u8>, VideoTranscodeError>;
}

/// Errors a [`VideoTranscoder`] may raise. Opaque strings: the toolchain lives platform-side,
/// so core cannot enumerate its failure modes.
#[derive(Debug, Error)]
pub enum VideoTranscodeError {
    /// First-frame extraction/decode failed.
    #[error("first-frame extraction failed: {0}")]
    FirstFrame(String),
    /// The H.264 preview transcode failed.
    #[error("preview transcode failed: {0}")]
    Preview(String),
}

/// One generated video derivative: the encoded bytes plus its signed manifest.
#[derive(Debug, Clone)]
pub struct GeneratedVideoDerivative {
    /// Which tier this is.
    pub tier: DerivativeTier,
    /// Which committed video-derivative format.
    pub format: VideoDerivativeFormat,
    /// The derivative bytes (still-encoder output for the first frame, or the transcoder's
    /// H.264 preview).
    pub bytes: Vec<u8>,
    /// The signed derivative manifest binding `hash(bytes)` and the format.
    pub manifest: DerivativeManifest,
}

/// Errors from video-derivative generation.
#[derive(Debug, Error)]
pub enum VideoDerivativeError {
    /// A pixel-buffer resize/conversion failed (first-frame still path).
    #[error("image buffer error: {0}")]
    Buffer(#[from] ImageBufferError),
    /// Manifest signing failed (e.g. a hardware device signer refused).
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),
    /// The transcoder failed.
    #[error("transcode error: {0}")]
    Transcode(#[from] VideoTranscodeError),
}

/// Generate the committed video derivatives for `source`, signing a [`DerivativeManifest`] for
/// each through the same two-signature path S-B1's stills use.
///
/// - **Thumbnail tier:** `transcoder.first_frame` extracts frame 0; it is resized to the
///   thumbnail long edge and encoded to each of
///   [`VideoDerivativeFormat::FIRST_FRAME_STILL_FORMATS`] via `still_encoder`.
/// - **Preview tier:** `transcoder.transcode_preview` produces the H.264 baseline preview under
///   [`H264PreviewParams::CONTRACT`].
///
/// Manifests of the same role are hash-chained (`prior_provenance_hash`) in generation order,
/// so each role's derivative provenance is an append-only chain exactly like the asset's.
pub fn generate_video_derivatives(
    source: &VideoSource<'_>,
    transcoder: &dyn VideoTranscoder,
    still_encoder: &dyn StillEncoder,
    ctx: &DerivativeContext<'_>,
) -> Result<Vec<GeneratedVideoDerivative>, VideoDerivativeError> {
    let mut out = Vec::new();

    // ── Thumbnail tier: first-frame still ────────────────────────────────────────────────
    let frame = transcoder.first_frame(source)?;
    // Reuse the still thumbnail long-edge cap (S-B1): the thumbnail tier is ~256 px.
    let work = match DerivativeTier::Thumbnail.target_long_edge() {
        Some(cap) => {
            let (w, h) = resize_to_max_dimension(frame.width, frame.height, cap);
            frame.resize(w, h)?.into_rgba8()?
        }
        None => frame.to_rgba8()?,
    };
    let mut thumb_prior: Option<Hash32> = None;
    for &format in &VideoDerivativeFormat::FIRST_FRAME_STILL_FORMATS {
        let still = format
            .still_format()
            .expect("first-frame still formats map to a still DerivativeFormat");
        let bytes = still_encoder.encode(&work, still, DerivativeTier::Thumbnail)?;
        out.push(sign_video_derivative(
            ctx,
            DerivativeTier::Thumbnail,
            format,
            &bytes,
            &mut thumb_prior,
        )?);
    }

    // ── Preview tier: H.264 baseline transcode ───────────────────────────────────────────
    let preview = transcoder.transcode_preview(source, &H264PreviewParams::CONTRACT)?;
    let mut preview_prior: Option<Hash32> = None;
    out.push(sign_video_derivative(
        ctx,
        DerivativeTier::Preview,
        VideoDerivativeFormat::H264Preview,
        &preview,
        &mut preview_prior,
    )?);

    Ok(out)
}

/// Build, sign, and chain one video-derivative manifest over `bytes`.
fn sign_video_derivative(
    ctx: &DerivativeContext<'_>,
    tier: DerivativeTier,
    format: VideoDerivativeFormat,
    bytes: &[u8],
    prior: &mut Option<Hash32>,
) -> Result<GeneratedVideoDerivative, VideoDerivativeError> {
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
    Ok(GeneratedVideoDerivative {
        tier,
        format,
        bytes: bytes.to_vec(),
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use uuid::Uuid;

    use super::*;
    use crate::crypto::keys::{AmkVersion, HybridSigningKey};
    use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
    use crate::media::image::buffer::{ComponentType, PixelFormat};
    use crate::media::metadata::ColorSpace;
    use crate::media::video::types::StandardResolution;

    /// A deterministic in-test still encoder standing in for the SDK's per-platform encoders
    /// (mirrors S-B1's `TagEncoder`): tags the format with a leading byte so distinct formats
    /// produce distinct bytes/hashes, then appends the resized RGBA.
    struct StillTag;
    impl StillEncoder for StillTag {
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

    /// A deterministic transcoder double (the video analogue of S-B1's `TagEncoder`, the
    /// fixture "transcode toolchain"): `first_frame` synthesizes a gradient frame at the source
    /// dimensions; `transcode_preview` **serializes the params it received** into a header so
    /// the test can prove the contract parameters are plumbed through unchanged, then appends
    /// the source bytes.
    struct TranscoderDouble;

    /// Magic prefix the double writes so the preview bytes are self-describing in tests.
    const PREVIEW_MAGIC: &[u8] = b"H264DBL";

    impl VideoTranscoder for TranscoderDouble {
        fn first_frame(
            &self,
            source: &VideoSource<'_>,
        ) -> Result<ImageBuffer, VideoTranscodeError> {
            Ok(gradient_rgb(source.width as usize, source.height as usize))
        }

        fn transcode_preview(
            &self,
            source: &VideoSource<'_>,
            params: &H264PreviewParams,
        ) -> Result<Vec<u8>, VideoTranscodeError> {
            let mut v = Vec::new();
            v.extend_from_slice(PREVIEW_MAGIC);
            v.push(match params.profile {
                H264Profile::Baseline => 0xB1,
            });
            v.push(params.crf);
            v.extend_from_slice(&params.max_width.to_be_bytes());
            v.extend_from_slice(&params.max_height.to_be_bytes());
            v.extend_from_slice(&params.max_fps.to_be_bytes());
            v.push(match params.audio {
                AudioCodec::Aac => 0xAA,
                _ => 0xFF,
            });
            v.extend_from_slice(source.bytes);
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
            source_asset_id: Uuid::from_u128(0x71DE0),
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
    fn fixture_video_yields_both_tiers_with_signed_verifying_manifests() {
        let (device, write) = (dev(), wt());
        let ctx = ctx(&device, &write);
        let source = VideoSource {
            bytes: b"the-original-mov-bytes",
            width: 3840,
            height: 2160,
        };

        let out = generate_video_derivatives(&source, &TranscoderDouble, &StillTag, &ctx).unwrap();

        // Thumbnail tier: two first-frame still formats (JXL, AVIF). Preview tier: one H.264.
        assert_eq!(out.len(), 3);

        let thumbs: Vec<&GeneratedVideoDerivative> = out
            .iter()
            .filter(|d| d.tier == DerivativeTier::Thumbnail)
            .collect();
        let previews: Vec<&GeneratedVideoDerivative> = out
            .iter()
            .filter(|d| d.tier == DerivativeTier::Preview)
            .collect();

        assert_eq!(
            thumbs.iter().map(|d| d.format).collect::<Vec<_>>(),
            VideoDerivativeFormat::FIRST_FRAME_STILL_FORMATS.to_vec()
        );
        assert_eq!(previews.len(), 1);
        assert_eq!(previews[0].format, VideoDerivativeFormat::H264Preview);

        for d in &out {
            // Format is a recognized committed video value and matches the manifest wire string.
            assert!(VideoDerivativeFormat::is_recognized(
                &d.manifest.core.format
            ));
            assert_eq!(d.manifest.core.format, d.format.mime());
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

        // The thumbnail role's two manifests form an append-only chain; the preview role starts
        // its own chain.
        assert!(thumbs[0].manifest.core.prior_provenance_hash.is_none());
        let prev_hash = hash::hash_bytes(&cbor::to_canonical_vec(&thumbs[0].manifest).unwrap());
        assert_eq!(
            thumbs[1].manifest.core.prior_provenance_hash,
            Some(prev_hash)
        );
        assert!(previews[0].manifest.core.prior_provenance_hash.is_none());
    }

    #[test]
    fn preview_transcode_receives_the_pinned_contract_parameters() {
        let (device, write) = (dev(), wt());
        let ctx = ctx(&device, &write);
        let source = VideoSource {
            bytes: b"src",
            width: 1280,
            height: 720,
        };

        let out = generate_video_derivatives(&source, &TranscoderDouble, &StillTag, &ctx).unwrap();
        let preview = out
            .iter()
            .find(|d| d.format == VideoDerivativeFormat::H264Preview)
            .unwrap();

        // Decode the header the double serialized: it proves core passed exactly the tier
        // table's pinned parameters (baseline / 1080p cap / CRF 23 / 30 fps / AAC).
        let b = &preview.bytes;
        assert_eq!(&b[..PREVIEW_MAGIC.len()], PREVIEW_MAGIC);
        let mut off = PREVIEW_MAGIC.len();
        assert_eq!(b[off], 0xB1, "H.264 baseline profile"); // profile
        off += 1;
        assert_eq!(b[off], 23, "CRF 23");
        off += 1;
        let max_w = u32::from_be_bytes(b[off..off + 4].try_into().unwrap());
        off += 4;
        let max_h = u32::from_be_bytes(b[off..off + 4].try_into().unwrap());
        off += 4;
        let max_fps = u32::from_be_bytes(b[off..off + 4].try_into().unwrap());
        off += 4;
        assert_eq!((max_w, max_h), (1920, 1080), "1080p cap");
        assert_eq!(max_fps, 30, "30 fps cap");
        assert_eq!(b[off], 0xAA, "AAC audio");
    }

    #[test]
    fn contract_params_are_exactly_the_tier_table_values() {
        let c = H264PreviewParams::CONTRACT;
        assert_eq!(c.profile, H264Profile::Baseline);
        assert_eq!(c.crf, 23);
        assert_eq!(c.max_fps, 30);
        assert_eq!(c.audio, AudioCodec::Aac);
        // The 1080p cap is pinned to the shared resolution preset, not a stray literal.
        assert_eq!(c.max_width, StandardResolution::Fhd1080p.width());
        assert_eq!(c.max_height, StandardResolution::Fhd1080p.height());
        assert_eq!(
            (c.max_width, c.max_height),
            (H264PreviewParams::MAX_WIDTH, H264PreviewParams::MAX_HEIGHT)
        );
    }

    #[test]
    fn unknown_video_derivative_format_is_a_structural_rejection() {
        // The video rows of the tier table, and nothing else, are recognized.
        assert!(!VideoDerivativeFormat::is_recognized("video/future-codec"));
        assert!(!VideoDerivativeFormat::is_recognized("video/webm"));
        assert!(!VideoDerivativeFormat::is_recognized("image/webp")); // not a video row
        assert!(VideoDerivativeFormat::parse("video/future-codec").is_none());

        // Every committed video value round-trips through parse↔mime.
        for f in [
            VideoDerivativeFormat::FirstFrameJxl,
            VideoDerivativeFormat::FirstFrameAvif,
            VideoDerivativeFormat::H264Preview,
        ] {
            assert_eq!(VideoDerivativeFormat::parse(f.mime()), Some(f));
            assert!(VideoDerivativeFormat::is_recognized(f.mime()));
        }
    }

    #[test]
    fn first_frame_still_formats_map_to_still_encoder_formats() {
        assert_eq!(
            VideoDerivativeFormat::FirstFrameJxl.still_format(),
            Some(DerivativeFormat::Jxl)
        );
        assert_eq!(
            VideoDerivativeFormat::FirstFrameAvif.still_format(),
            Some(DerivativeFormat::Avif)
        );
        // The H.264 preview is not routed through the still encoder.
        assert_eq!(VideoDerivativeFormat::H264Preview.still_format(), None);
    }

    #[test]
    fn transcoder_failure_propagates_as_a_derivative_error() {
        struct FailingTranscoder;
        impl VideoTranscoder for FailingTranscoder {
            fn first_frame(
                &self,
                _source: &VideoSource<'_>,
            ) -> Result<ImageBuffer, VideoTranscodeError> {
                Err(VideoTranscodeError::FirstFrame("decode blew up".into()))
            }
            fn transcode_preview(
                &self,
                _source: &VideoSource<'_>,
                _params: &H264PreviewParams,
            ) -> Result<Vec<u8>, VideoTranscodeError> {
                unreachable!("first_frame fails first")
            }
        }

        let (device, write) = (dev(), wt());
        let ctx = ctx(&device, &write);
        let source = VideoSource {
            bytes: b"x",
            width: 640,
            height: 480,
        };
        let err =
            generate_video_derivatives(&source, &FailingTranscoder, &StillTag, &ctx).unwrap_err();
        assert!(matches!(err, VideoDerivativeError::Transcode(_)));
    }
}
