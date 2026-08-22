//! Still-derivative generation for the signed import path — the only `lifecycle` file that
//! reaches [`crate::media`].

use std::fs;

use uuid::Uuid;

use super::{AssetState, LifecycleError, Result, Workspace, media_dir, now_rfc3339};
use crate::cbor;
use crate::crypto::keys::AmkVersion;
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::media::image::derivative::{
    DerivativeContext, DerivativeFormat, DerivativeTier, GeneratedDerivative,
    generate_still_derivatives,
};
use crate::sidecar::sidecar_v1::Dimensions;

/// Still-derivative generation for the signed import path (S-B1 → S-B2). Compiled only with the
/// `media` feature: decode the still, compute its LQIP, and generate + sign the thumbnail/preview
/// [`DerivativeManifest`](crate::crypto::provenance::DerivativeManifest)s through the injected
/// [`StillEncoder`](crate::media::image::derivative::StillEncoder).
#[cfg(feature = "media")]
impl Workspace {
    /// Decode a still into an in-memory pixel buffer, dispatching on extension. Unsupported or
    /// undecodable bytes yield `None` (the import proceeds signed-original-only).
    fn decode_still(
        &self,
        bytes: &[u8],
        ext: &str,
    ) -> Option<crate::media::image::buffer::ImageBuffer> {
        use crate::media::image::{Image, ImageDecode};
        match ext {
            "jpg" | "jpeg" => {
                crate::media::image::formats::jpeg::JpegImage::decode_from_bytes(bytes)
                    .ok()
                    .map(|img| img.get_buffer())
            }
            "png" => crate::media::image::formats::png::PngImage::decode_from_bytes(bytes)
                .ok()
                .map(|img| img.get_buffer()),
            _ => None,
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
    pub(super) fn prepare_still(
        &self,
        plaintext: &[u8],
        ext: &str,
        exif: &crate::exif::extract::ExifExtract,
        asset_id: Uuid,
        album_id: Uuid,
    ) -> Result<(
        Option<Dimensions>,
        Option<crate::sidecar::sidecar_v1::Lqip>,
        Vec<GeneratedDerivative>,
    )> {
        let exif_dimensions = exif
            .width
            .zip(exif.height)
            .map(|(width, height)| Dimensions { width, height });

        let Some(buffer) = self.decode_still(plaintext, ext) else {
            // Undecodable / unsupported still: EXIF dimensions only, no LQIP or derivatives.
            return Ok((exif_dimensions, None, Vec::new()));
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
        Ok((dimensions, lqip, derivatives))
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
