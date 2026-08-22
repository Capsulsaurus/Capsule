//! Still-derivative generation for the signed import path — the only `lifecycle` file that
//! reaches [`crate::media`].

use std::fs;
use std::path::Path;

use uuid::Uuid;

use super::{
    AssetState, DerivativeStatus, LifecycleError, Result, Workspace, media_dir, now_rfc3339,
};
use crate::cbor;
use crate::crypto::keys::AmkVersion;
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::media::image::derivative::{
    DerivativeContext, DerivativeFormat, DerivativeTier, GeneratedDerivative,
    generate_still_derivatives,
};
use crate::sidecar::sidecar_v1::Dimensions;

/// Everything [`Workspace::prepare_still`] derives from a still in one pass: the sidecar fields
/// (`dimensions`, `lqip`), the signed derivatives to persist after the commit, and the
/// [`DerivativeStatus`] explaining what was or was not produced.
pub(super) struct PreparedStill {
    /// Pixel dimensions when the still decoded, EXIF dimensions otherwise, `None` if neither.
    pub(super) dimensions: Option<Dimensions>,
    /// The sidecar LQIP, present only when the still decoded.
    pub(super) lqip: Option<crate::sidecar::sidecar_v1::Lqip>,
    /// Signed thumbnail/preview derivatives; empty when the still did not decode or no
    /// [`StillEncoder`](crate::media::image::derivative::StillEncoder) is attached.
    pub(super) derivatives: Vec<GeneratedDerivative>,
    /// Whether derivatives were generated, and if not, why.
    pub(super) status: DerivativeStatus,
}

/// Still-derivative generation for the signed import path (S-B1 → S-B2). Compiled only with the
/// `media` feature: decode the still, compute its LQIP, and generate + sign the thumbnail/preview
/// [`DerivativeManifest`](crate::crypto::provenance::DerivativeManifest)s through the injected
/// [`StillEncoder`](crate::media::image::derivative::StillEncoder).
#[cfg(feature = "media")]
impl Workspace {
    /// Decode a still into an in-memory pixel buffer, dispatching on extension, and classify
    /// *why* when it cannot (slice `S-B13`).
    ///
    /// A `None` buffer never fails the import — the original is backed up regardless — but the
    /// two reasons for it are very different and must be distinguishable in the logs and in the
    /// run summary:
    ///
    /// - [`DeferredNoCodec`](DerivativeStatus::DeferredNoCodec): this build links no codec for
    ///   the format (HEIC, RAW, …). Expected and deferred; warned once per asset so the gap is
    ///   visible rather than silent.
    /// - [`DecodeFailed`](DerivativeStatus::DecodeFailed): a format we *do* support did not
    ///   decode. A real problem, warned with the underlying error.
    fn decode_still(
        &self,
        bytes: &[u8],
        ext: &str,
        src: &Path,
    ) -> (
        Option<crate::media::image::buffer::ImageBuffer>,
        DerivativeStatus,
    ) {
        use crate::media::image::types::{ImageFormat, SUPPORTED_IMAGE_FORMATS};
        use crate::media::image::{FormatOp, Image, ImageDecode, ImageError};

        let Some(format) = ImageFormat::from_extension(ext) else {
            tracing::debug!(
                path = %src.display(),
                %ext,
                "derivatives: not a known still format; nothing to decode"
            );
            return (None, DerivativeStatus::NotAKnownStill);
        };

        if !format.is_decodable() {
            tracing::warn!(
                path = %src.display(),
                ?format,
                supported = ?SUPPORTED_IMAGE_FORMATS,
                "derivatives: no codec for this format in this build; the original is imported \
                 signed and encrypted, but without a thumbnail/preview or LQIP until the codec \
                 lands (S-B13)"
            );
            return (None, DerivativeStatus::DeferredNoCodec);
        }

        // Only formats `is_decodable` vouches for reach here. The catch-all keeps the match
        // total without a panicking stub: every other format module is uninhabited and its
        // `decode_from_bytes` returns exactly this error, so a table drift degrades to a
        // `DecodeFailed` warning instead of aborting the import.
        let decoded = match format {
            ImageFormat::Jpeg => {
                crate::media::image::formats::jpeg::JpegImage::decode_from_bytes(bytes)
                    .map(|img| img.get_buffer())
            }
            ImageFormat::Png => {
                crate::media::image::formats::png::PngImage::decode_from_bytes(bytes)
                    .map(|img| img.get_buffer())
            }
            other => Err(ImageError::UnsupportedFormat {
                format: other,
                op: FormatOp::Decode,
            }),
        };

        match decoded {
            Ok(buffer) => (Some(buffer), DerivativeStatus::Decoded),
            Err(error) => {
                tracing::warn!(
                    path = %src.display(),
                    ?format,
                    %error,
                    "derivatives: a supported format failed to decode; the original is imported \
                     signed and encrypted, but without a thumbnail/preview or LQIP"
                );
                (None, DerivativeStatus::DecodeFailed)
            }
        }
    }

    /// Compute the sidecar LQIP (chromahash + versioned fallback color) from a decoded buffer.
    fn lqip_from_buffer(
        buffer: &crate::media::image::buffer::ImageBuffer,
    ) -> Option<crate::sidecar::sidecar_v1::Lqip> {
        let rgba = buffer.to_rgba8().ok()?;
        let lqip = crate::media::image::lqip::LQIP::from_rgba_buffer(&rgba).ok()?;
        lqip.to_sidecar().ok()
    }

    /// Decode the still once and derive: pixel `dimensions`, the sidecar `lqip`, and the signed
    /// thumbnail/preview derivatives (empty when no encoder is attached). All are attached before
    /// the sidecar is sealed / after the manifest is signed, per the pipeline's Execute step.
    ///
    /// **Never fails the import.** A still this build cannot decode still commits as a signed,
    /// encrypted original — it just falls back to EXIF dimensions with no LQIP and no
    /// derivatives. The returned [`DerivativeStatus`] says whether that happened and why, so
    /// the caller can report the gap instead of it being invisible.
    pub(super) fn prepare_still(
        &self,
        plaintext: &[u8],
        ext: &str,
        src: &Path,
        exif: &crate::exif::extract::ExifExtract,
        asset_id: Uuid,
        album_id: Uuid,
    ) -> Result<PreparedStill> {
        let exif_dimensions = exif
            .width
            .zip(exif.height)
            .map(|(width, height)| Dimensions { width, height });

        let (buffer, status) = self.decode_still(plaintext, ext, src);
        let Some(buffer) = buffer else {
            // Undecodable / unsupported still: EXIF dimensions only, no LQIP or derivatives.
            // `status` carries which of the two it was; `decode_still` already logged it.
            return Ok(PreparedStill {
                dimensions: exif_dimensions,
                lqip: None,
                derivatives: Vec::new(),
                status,
            });
        };

        let dimensions = Some(Dimensions {
            width: buffer.width as u32,
            height: buffer.height as u32,
        });
        let lqip = Self::lqip_from_buffer(&buffer);

        let derivatives = match self.still_encoder.as_ref() {
            Some(encoder) => {
                let album = self.album(&album_id)?;
                let ctx = DerivativeContext {
                    source_asset_id: asset_id,
                    crypto_suite_id: CRYPTO_SUITE_ID,
                    protocol_version: PROTOCOL_VERSION.into(),
                    amk_version: AmkVersion(album.current_epoch),
                    generated_by_device: self.account.device.device_id,
                    generated_by_client: self.client_version.clone(),
                    generated_at: now_rfc3339(),
                    device_signer: self.device_signer.as_ref(),
                    write_tier_signer: &album.write_tier,
                };
                generate_still_derivatives(
                    &buffer,
                    plaintext,
                    &[DerivativeTier::Thumbnail, DerivativeTier::Preview],
                    encoder.as_ref(),
                    &ctx,
                )
                .map_err(|e| LifecycleError::Io(format!("derivative generation: {e}")))?
            }
            None => Vec::new(),
        };
        Ok(PreparedStill {
            dimensions,
            lqip,
            derivatives,
            status,
        })
    }

    /// Write the generated derivative bytes + their signed manifest bundle under the asset's
    /// media directory (`derivatives/{uuid}.{role}.{ext}` and `{uuid}.derivatives.cbor`).
    pub(super) fn persist_derivatives(
        &self,
        asset: &AssetState,
        derivatives: &[GeneratedDerivative],
    ) -> Result<()> {
        if derivatives.is_empty() {
            return Ok(());
        }
        let dir = media_dir(&self.root, asset.capture_utc).join("derivatives");
        fs::create_dir_all(&dir).map_err(|e| LifecycleError::Io(e.to_string()))?;
        let stem = asset.asset_id.simple();

        let mut manifests = Vec::with_capacity(derivatives.len());
        for d in derivatives {
            let format_ext = match d.format {
                DerivativeFormat::Jxl => "jxl",
                DerivativeFormat::Avif => "avif",
                DerivativeFormat::WebP => "webp",
                DerivativeFormat::Original => asset.ext.as_str(),
            };
            let role = match d.tier {
                DerivativeTier::Thumbnail => "thumbnail",
                DerivativeTier::Preview => "preview",
            };
            fs::write(dir.join(format!("{stem}.{role}.{format_ext}")), &d.bytes)
                .map_err(|e| LifecycleError::Io(e.to_string()))?;
            manifests.push(d.manifest.clone());
        }
        let bundle =
            cbor::to_canonical_vec(&manifests).map_err(|e| LifecycleError::Cbor(e.to_string()))?;
        fs::write(dir.join(format!("{stem}.derivatives.cbor")), bundle)
            .map_err(|e| LifecycleError::Io(e.to_string()))?;
        Ok(())
    }
}
