//! The one `lifecycle` file that reaches [`crate::media`]: decode a still once at import and
//! derive everything that needs pixels (slices `S-B1`, `S-B13`).
//!
//! # Why this is not feature-gated
//!
//! `capsule-core`'s `native` feature *implies* `media`, and the whole `lifecycle` module is
//! `native`-gated, so a build that compiles this file always has the codec stack. The two builds
//! that drop `media` — `capsule-server` and `capsule-wasm`, both
//! `default-features = false` — drop `lifecycle` with it. A `#[cfg(feature = "media")]` here
//! would therefore guard nothing while making every signature read as optional; if the
//! implication is ever removed, this file fails to compile, which is the right way for that
//! decision to surface.
//!
//! # Never fails the import
//!
//! Capsule is a backup tool. Every path below degrades to "the original is imported signed,
//! encrypted and `verify_asset`-accepting, without a placeholder or a thumbnail" and records
//! **why** in the returned [`DerivativeStatus`]. The only errors that propagate are the ones
//! that mean the *workspace* is broken — a missing album, a signer that refused — not the ones
//! that mean the pixels were unreadable.

use std::fs;
use std::path::Path;

use uuid::Uuid;

use super::{AssetState, DerivativeStatus, Result, Workspace, media_dir};
use crate::cbor;
use crate::crypto::encryption::encrypt_asset_rekey;
use crate::crypto::encryption::stream::AssetEncryption;
use crate::crypto::keys::{Amk, AmkVersion};
use crate::crypto::primitives::{CRYPTO_SUITE_ID, PROTOCOL_VERSION};
use crate::exif::extract::ExifExtract;
use crate::lqip::Lqip;
use crate::media::{
    DecodedImage, DerivativeContext, DerivativeSealer, DerivativeTier, GeneratedDerivative,
    MediaError, RawshiftDecoder, SealedDerivative, StillFormat, decode_guarded,
    generate_still_derivatives, guarded,
};
use crate::sidecar::sidecar_v1::{Dimensions, Lqip as SidecarLqip};

/// The file under import, as [`prepare_still`](Workspace::prepare_still) needs to see it.
///
/// A parameter object rather than four positional arguments: these four are one thing — the
/// bytes on the way in and what the scanner already learned about them — and they are always
/// passed together. The alternative was silencing `clippy::too_many_arguments`, which would have
/// hidden that the signature had grown two *kinds* of input (the file, and the crypto identity
/// it commits under) without saying so.
pub(super) struct StillSource<'a> {
    /// The file's bytes.
    pub(super) plaintext: &'a [u8],
    /// Its lowercase extension without the dot, `""` when it has none.
    pub(super) ext: &'a str,
    /// Where it came from — for logs only; the bytes above are authoritative.
    pub(super) src: &'a Path,
    /// What `capsule_core::exif` read off it, the fallback for dimensions.
    pub(super) exif: &'a ExifExtract,
}

/// Everything one still yields in a single decode pass: the sidecar fields, the signed
/// derivatives to persist after the durable commit, and the reason for anything missing.
pub(super) struct PreparedStill {
    /// The format detection identified, if the bytes are a still Capsule models. Drives the
    /// sidecar's `content_type` from the *header* rather than from the file name.
    pub(super) format: Option<StillFormat>,
    /// Pixel dimensions when the still decoded, EXIF dimensions otherwise, `None` if neither.
    ///
    /// Decoded pixels win over EXIF because they are post-orientation: a quarter-turned JPEG's
    /// EXIF `PixelXDimension` is its *stored* width, which is transposed relative to what a
    /// viewer shows.
    pub(super) dimensions: Option<Dimensions>,
    /// The sidecar LQIP, present only when the still decoded.
    pub(super) lqip: Option<SidecarLqip>,
    /// Signed thumbnail derivatives, to be written after the asset's own files.
    pub(super) derivatives: Vec<GeneratedDerivative>,
    /// How many `(tier, format)` pairs the tier table commits to and this build cannot encode.
    pub(super) deferred_formats: usize,
    /// Whether derivatives were generated, and if not, why.
    pub(super) status: DerivativeStatus,
}

impl PreparedStill {
    /// The outcome for bytes that yielded no pixels: EXIF dimensions only, no LQIP, no
    /// derivatives, and `status` carrying which of the reasons it was.
    fn undecoded(
        format: Option<StillFormat>,
        exif_dimensions: Option<Dimensions>,
        status: DerivativeStatus,
    ) -> Self {
        Self {
            format,
            dimensions: exif_dimensions,
            lqip: None,
            derivatives: Vec::new(),
            deferred_formats: 0,
            status,
        }
    }
}

/// Map a decode failure onto the status the run summary counts, logging the distinction that
/// makes the two reasons useful (slice `S-B13`).
fn classify(error: &MediaError, src: &Path, format: Option<StillFormat>) -> DerivativeStatus {
    match error {
        MediaError::UnsupportedFormat { format, op } => {
            tracing::warn!(
                path = %src.display(),
                %format,
                op = op.as_str(),
                supported = ?crate::media::SUPPORTED_STILL_FORMATS,
                "derivatives: no codec for this format in this build; the original is imported \
                 signed and encrypted, but without a thumbnail or LQIP until the codec lands. \
                 Derivatives are backfillable from the stored original (S-B13)"
            );
            DerivativeStatus::DeferredNoCodec
        }
        MediaError::NotAStillImage => {
            tracing::debug!(
                path = %src.display(),
                "derivatives: not a still image Capsule models; nothing to decode"
            );
            DerivativeStatus::NotAKnownStill
        }
        // Everything else is a format we *do* support failing on these particular bytes — a
        // real problem worth investigating, not an expected gap.
        error => {
            tracing::warn!(
                path = %src.display(),
                ?format,
                %error,
                "derivatives: a supported format failed to decode; the original is imported \
                 signed and encrypted, but without a thumbnail or LQIP"
            );
            DerivativeStatus::DecodeFailed
        }
    }
}

/// Compute the sidecar LQIP from a decoded frame.
///
/// From the **full-resolution, orientation-applied** frame, never from the thumbnail:
/// chromahash consumes the whole frame and band-limits on the read side via `decode_capped`, so
/// pre-resizing would silently cap fidelity the format can carry ([`crate::lqip`]).
fn lqip_from(decoded: &DecodedImage, src: &Path) -> Option<SidecarLqip> {
    // Guarded because `chromahash` is pre-1.0 too and its own `encode` panics on a zero
    // dimension or a length mismatch. `Lqip::encode` checks both, so this is belt and braces —
    // but it is what makes the module's "no codec can abort an import" claim true rather than
    // nearly true.
    let encoded = guarded("lqip", || {
        Lqip::encode(
            decoded.width(),
            decoded.height(),
            &decoded.image.rgba,
            decoded.gamut,
        )
        .map_err(|e| MediaError::Decode {
            format: decoded.format,
            detail: format!("LQIP encode: {e}"),
        })
    });
    match encoded {
        Ok(lqip) => Some(lqip.to_sidecar()),
        Err(error) => {
            // A decoded frame satisfies both of `encode`'s preconditions by construction, so
            // this is unreachable rather than expected — logged as such, and never fatal.
            tracing::warn!(
                path = %src.display(),
                %error,
                "derivatives: a decoded frame was rejected by the LQIP encoder; importing \
                 without a placeholder"
            );
            None
        }
    }
}

/// The album-key half of derivative generation: `media` produces the bytes, this encrypts them.
///
/// One `encrypt_asset_rekey` per derivative under the **source asset's** `file_id` and the
/// album's current AMK, with a fresh CSPRNG nonce prefix each time — so every derivative of an
/// asset gets its own file key, per the encryption doc's per-file key derivation. The ciphertext
/// is deliberately dropped: the client keeps the plaintext derivative on disk (the local gallery
/// paints it) and re-derives the ciphertext at push time from the recorded prefix, exactly as it
/// already does for the original.
struct AlbumSealer<'a> {
    amk: &'a Amk,
    asset_id: Uuid,
}

impl DerivativeSealer for AlbumSealer<'_> {
    fn seal(&self, plaintext: &[u8]) -> std::result::Result<SealedDerivative, MediaError> {
        let (enc, _ciphertext, _file_key) =
            encrypt_asset_rekey(self.amk, &self.asset_id, plaintext, None).map_err(|e| {
                MediaError::Encode {
                    format: crate::media::DerivativeFormat::Original,
                    detail: format!("sealing the derivative: {e}"),
                }
            })?;
        Ok(SealedDerivative {
            ciphertext_hash: enc.ciphertext_hash,
            nonce_prefix: enc.nonce_prefix,
        })
    }
}

impl Workspace {
    /// Decode the still once and derive: the `content_type`, pixel `dimensions`, the sidecar
    /// `lqip`, and the signed thumbnail derivatives. All are attached before the sidecar is
    /// sealed, per the pipeline's Execute step.
    ///
    /// **Never fails over unreadable pixels.** A still this build cannot decode still commits as
    /// a signed, encrypted original — it falls back to EXIF dimensions with no LQIP and no
    /// derivatives, and the returned [`DerivativeStatus`] says which reason applied so the
    /// caller can report the gap instead of it being invisible.
    #[tracing::instrument(
        level = "debug",
        skip_all,
        fields(asset_id = %asset_id, src = %source.src.display(), bytes = source.plaintext.len())
    )]
    pub(super) fn prepare_still(
        &self,
        source: &StillSource<'_>,
        asset_id: Uuid,
        album_id: Uuid,
        amk: &Amk,
        original: &AssetEncryption,
    ) -> Result<PreparedStill> {
        let StillSource {
            plaintext,
            ext,
            src,
            exif,
        } = *source;
        let exif_dimensions = exif
            .width
            .zip(exif.height)
            .map(|(width, height)| Dimensions { width, height });
        // Detected here as well as inside the decoder so the sidecar's `content_type` is
        // header-derived even for a format with no codec: a HEIC is `image/heic` in the sidecar
        // whether or not this build can read its pixels.
        let sniffed = StillFormat::detect(plaintext, ext);

        let decoded = match decode_guarded(&RawshiftDecoder, plaintext, ext) {
            Ok(decoded) => decoded,
            Err(error) => {
                let status = classify(&error, src, sniffed);
                return Ok(PreparedStill::undecoded(sniffed, exif_dimensions, status));
            }
        };

        let dimensions = Some(Dimensions {
            width: decoded.width(),
            height: decoded.height(),
        });
        if let (Some(pixels), Some(exif_dims)) = (dimensions.as_ref(), exif_dimensions.as_ref())
            && pixels != exif_dims
        {
            // Not an error: EXIF dimensions are pre-orientation and are frequently stale after
            // an edit. Logged because a surprising sidecar dimension is otherwise unexplainable
            // after the fact.
            tracing::debug!(
                asset_id = %asset_id,
                pixel_width = pixels.width,
                pixel_height = pixels.height,
                exif_width = exif_dims.width,
                exif_height = exif_dims.height,
                orientation = decoded.orientation_applied,
                "derivatives: decoded dimensions differ from EXIF; the pixels are authoritative"
            );
        }
        let lqip = lqip_from(&decoded, src);

        let album = self.album(&album_id)?;
        let ctx = DerivativeContext {
            source_asset_id: asset_id,
            crypto_suite_id: CRYPTO_SUITE_ID,
            protocol_version: PROTOCOL_VERSION.into(),
            amk_version: AmkVersion(album.current_epoch),
            generated_by_device: self.account.device.device_id,
            generated_by_client: self.client_version.clone(),
            generated_at: super::now_rfc3339(),
            device_signer: self.device_signer.as_ref(),
            write_tier_signer: album.write_tier_signer()?,
            sealer: &AlbumSealer { amk, asset_id },
            // The `original` sentinel references the original blob rather than encrypting
            // anything, so it signs what the original's own manifest signs.
            original: SealedDerivative {
                ciphertext_hash: original.ciphertext_hash,
                nonce_prefix: original.nonce_prefix,
            },
        };
        // Guarded, and **not** propagated on failure. A codec refusing a frame the decoder just
        // produced is a real defect, but it is this asset's derivative that is broken, not the
        // workspace: the signed original, its dimensions and its placeholder are all still
        // right, and failing the import would trade a missing thumbnail for a missing backup.
        // Reported as `DecodeFailed` — the "a supported path produced no derivative and somebody
        // should look at it" bucket — so the run summary counts it instead of staying silent.
        let generated = guarded("derivatives", || {
            generate_still_derivatives(&decoded, &DerivativeTier::GENERATED, &ctx)
        });
        let derivatives = match generated {
            Ok(derivatives) => derivatives,
            Err(error) => {
                tracing::warn!(
                    asset_id = %asset_id,
                    path = %src.display(),
                    format = %decoded.format,
                    width = decoded.width(),
                    height = decoded.height(),
                    %error,
                    "derivatives: the still decoded but no derivative could be produced from it; \
                     the original, its dimensions and its placeholder are committed regardless"
                );
                return Ok(PreparedStill {
                    format: Some(decoded.format),
                    dimensions,
                    lqip,
                    derivatives: Vec::new(),
                    deferred_formats: 0,
                    status: DerivativeStatus::DecodeFailed,
                });
            }
        };

        Ok(PreparedStill {
            format: Some(decoded.format),
            dimensions,
            lqip,
            deferred_formats: derivatives.deferred.len(),
            derivatives: derivatives.generated,
            status: DerivativeStatus::Decoded,
        })
    }

    /// Write the generated derivative bytes plus their signed manifest bundle under the asset's
    /// media directory: `derivatives/{uuid}.{role}.{ext}` and `{uuid}.derivatives.cbor`.
    ///
    /// The layout is the one the upload bundle reader already looks for
    /// ([`Workspace::upload_bundle`](Workspace::upload_bundle) finds a derivative's bytes by the
    /// `{uuid}.{role}.` prefix), so persisting here needs no change on the read side.
    ///
    /// Called **after** the asset's own files are durable: a derivative is regenerable and must
    /// never be able to fail an import that has already committed. A write error is therefore
    /// logged and swallowed rather than propagated.
    pub(super) fn persist_derivatives(
        &self,
        asset: &AssetState,
        derivatives: &[GeneratedDerivative],
    ) {
        if derivatives.is_empty() {
            return;
        }
        let dir = media_dir(&self.root, asset.capture_utc).join("derivatives");
        if let Err(error) = fs::create_dir_all(&dir) {
            tracing::warn!(
                asset_id = %asset.asset_id,
                dir = %dir.display(),
                %error,
                "derivatives: could not create the derivative directory; the asset is committed \
                 and its derivatives are regenerable"
            );
            return;
        }
        let stem = asset.asset_id.simple();

        let mut manifests = Vec::with_capacity(derivatives.len());
        for derivative in derivatives {
            // The `original` sentinel has no bytes of its own: its manifest *references* the
            // original, whose content address it signs. Writing a byte-for-byte copy under a
            // thumbnail's name would duplicate a file two directories up and re-expose the
            // original's EXIF — GPS included — as a derivative, where a re-encoded thumbnail is
            // metadata-free by construction. Its manifest still goes into the bundle: that
            // signed marker is the difference between "the original *is* the thumbnail" and
            // "the thumbnail is missing, rebuild it".
            if let Some(format_ext) = derivative.format.extension() {
                let path = dir.join(format!(
                    "{stem}.{}.{format_ext}",
                    derivative.tier.role_name()
                ));
                if let Err(error) = fs::write(&path, &derivative.bytes) {
                    tracing::warn!(
                        asset_id = %asset.asset_id,
                        path = %path.display(),
                        %error,
                        "derivatives: could not write a derivative; skipping it"
                    );
                    continue;
                }
            } else {
                debug_assert!(
                    derivative.bytes.is_empty(),
                    "only the byte-free `original` sentinel has no extension"
                );
            }
            manifests.push(derivative.manifest.clone());
        }

        if manifests.is_empty() {
            return;
        }
        match cbor::to_canonical_vec(&manifests) {
            Ok(bundle) => {
                let path = dir.join(format!("{stem}.derivatives.cbor"));
                if let Err(error) = fs::write(&path, bundle) {
                    tracing::warn!(
                        asset_id = %asset.asset_id,
                        path = %path.display(),
                        %error,
                        "derivatives: could not write the manifest bundle; the bytes on disk are \
                         unusable without it and will be regenerated"
                    );
                    return;
                }
                tracing::debug!(
                    asset_id = %asset.asset_id,
                    count = manifests.len(),
                    dir = %dir.display(),
                    "derivatives: persisted with their signed manifest bundle"
                );
            }
            Err(error) => tracing::warn!(
                asset_id = %asset.asset_id,
                %error,
                "derivatives: the manifest bundle did not serialise; skipping persistence"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use rawshift_image::core::metadata::{ImageInfo, ImageMetadata};
    use rawshift_image::core::{BitDepth, MetadataEmbedOptions};
    use rawshift_image::formats::encode_rgb_image_to_vec;
    use rawshift_image::formats::export::{
        CommonEncodeOptions, EncodeOptions, JpegEncEncodeConfig, ZunePngEncodeConfig,
    };
    use tempfile::TempDir;

    use super::super::{DerivativeStatus, SignedImportOptions, Workspace, fast_workspace};
    use super::*;
    use crate::crypto::hash;
    use crate::crypto::provenance::DerivativeManifest;
    use crate::media::{Decoder as _, DerivativeFormat, verify_still_format};
    use crate::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};

    /// A deterministic RGB gradient, `width` x `height`, as interleaved RGB `u16`.
    fn frame(width: u32, height: u32) -> rawshift_image::core::image::RgbImage {
        let (w, h) = (width as usize, height as usize);
        let mut data = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            for x in 0..w {
                data.push(((x * 255 / w) as u16) * 257);
                data.push(((y * 255 / h) as u16) * 257);
                data.push((((x + y) * 255 / (w + h)) as u16) * 257);
            }
        }
        rawshift_image::core::image::RgbImage::with_color_space(
            width,
            height,
            data,
            rawshift_image::core::ColorSpace::Srgb,
        )
    }

    fn common(metadata: MetadataEmbedOptions) -> CommonEncodeOptions {
        CommonEncodeOptions {
            metadata,
            bit_depth: BitDepth::Eight,
        }
    }

    /// A JPEG carrying an EXIF orientation tag, so the decode path has a transform to apply and
    /// the sidecar's dimensions have to disagree with the stored ones.
    fn jpeg(width: u32, height: u32, orientation: Option<u16>) -> Vec<u8> {
        let metadata = ImageMetadata {
            image: ImageInfo {
                orientation,
                ..ImageInfo::default()
            },
            ..ImageMetadata::default()
        };
        let embed = MetadataEmbedOptions {
            embed_exif: orientation.is_some(),
            embed_icc: false,
            embed_xmp: false,
        };
        encode_rgb_image_to_vec(
            &frame(width, height),
            &metadata,
            &EncodeOptions::JpegJpegEnc(JpegEncEncodeConfig {
                common: common(embed),
                quality: 90,
            }),
        )
        .expect("the fixture JPEG encodes")
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        encode_rgb_image_to_vec(
            &frame(width, height),
            &ImageMetadata::default(),
            &EncodeOptions::PngZune(ZunePngEncodeConfig {
                common: common(MetadataEmbedOptions::none()),
                ..ZunePngEncodeConfig::default()
            }),
        )
        .expect("the fixture PNG encodes")
    }

    /// A workspace with fast Argon2 params and its default album created.
    fn workspace(dir: &Path) -> (Workspace, Uuid) {
        let mut ws = fast_workspace(dir);
        let album = ws.default_album_id();
        ws.create_album_with_id(album, "Imports").unwrap();
        (ws, album)
    }

    /// Write `bytes` into `dir` under `name` and import it, returning the receipt.
    fn import(
        ws: &mut Workspace,
        album: Uuid,
        dir: &Path,
        name: &str,
        bytes: &[u8],
    ) -> super::super::SignedImport {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        ws.import_asset_with(album, &path, &SignedImportOptions::default())
            .expect("the import commits")
    }

    /// Read back the signed sidecar an import wrote, from the library directory alone.
    fn sidecar_of(root: &Path, asset_id: Uuid) -> SidecarV1 {
        let mut stack = vec![root.join("media")];
        let name = format!("{}.cbor", asset_id.simple());
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if entry.file_name() == std::ffi::OsString::from(&name) {
                    let bytes = fs::read(&path).unwrap();
                    return SidecarV1::from_canonical_slice(&bytes, SIDECAR_SCHEMA_V1)
                        .expect("the sidecar decodes");
                }
            }
        }
        panic!("no sidecar for {asset_id}");
    }

    /// The derivatives directory for the bucket holding `asset_id`'s files.
    fn derivatives_dir(root: &Path, asset_id: Uuid) -> std::path::PathBuf {
        let mut stack = vec![root.join("media")];
        let name = format!("{}.cbor", asset_id.simple());
        while let Some(dir) = stack.pop() {
            for entry in fs::read_dir(&dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if entry.file_name() == std::ffi::OsString::from(&name) {
                    return dir.join("derivatives");
                }
            }
        }
        panic!("no bucket for {asset_id}");
    }

    /// **The `S-B14` acceptance case, and the first production caller of `capsule_core::lqip`.**
    ///
    /// A decodable still imports with real pixel dimensions and a 32-byte chromahash placeholder
    /// inside the *signed* sidecar, and the sidecar still verifies — the placeholder is
    /// signature-covered, so producing it is a signature-visible change and has to be checked as
    /// one.
    #[test]
    fn a_decodable_still_imports_with_pixel_dimensions_and_an_lqip() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        let receipt = import(
            &mut ws,
            album,
            src.path(),
            "photo.jpg",
            &jpeg(320, 240, None),
        );
        assert_eq!(receipt.derivatives, DerivativeStatus::Decoded);
        assert_eq!(
            receipt.deferred_formats, 2,
            "the JXL master and the AVIF delivery variant have no encoder in this build"
        );

        let sidecar = sidecar_of(lib.path(), receipt.asset_id);
        let dimensions = sidecar
            .dimensions
            .as_ref()
            .expect("dimensions from decoded pixels");
        assert_eq!((dimensions.width, dimensions.height), (320, 240));
        assert_eq!(
            sidecar.content_type, "image/jpeg",
            "the content type is header-derived"
        );

        let lqip = sidecar.lqip.as_ref().expect("the LQIP producer ran");
        assert_eq!(lqip.chromahash.len(), 32, "DEFAULT_TIER is 32 bytes");
        assert_eq!(lqip.format_version, crate::lqip::LQIP_FORMAT_V1);
        assert!(
            Lqip::from_bytes(&lqip.chromahash).is_ok(),
            "the stored payload is a structurally valid chromahash"
        );

        assert!(
            sidecar.verify(&ws.user_ik_public()),
            "the sidecar signature covers the placeholder it now carries"
        );
    }

    /// The sidecar's dimensions are the **upright** ones. A quarter-turned JPEG's stored width
    /// is its EXIF `PixelXDimension`, transposed relative to what a viewer shows, so taking the
    /// decoded pixels rather than the tag is the whole point.
    #[test]
    fn a_rotated_still_records_upright_dimensions() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        let receipt = import(
            &mut ws,
            album,
            src.path(),
            "portrait.jpg",
            &jpeg(320, 240, Some(6)),
        );
        assert_eq!(receipt.derivatives, DerivativeStatus::Decoded);

        let sidecar = sidecar_of(lib.path(), receipt.asset_id);
        let dimensions = sidecar.dimensions.as_ref().expect("dimensions");
        assert_eq!(
            (dimensions.width, dimensions.height),
            (240, 320),
            "orientation 6 is a quarter-turn, so the sidecar records the transposed pair"
        );
    }

    /// Thumbnail bytes and a signed manifest bundle land on disk, at the layout the upload
    /// bundle reader already looks for, and the bundle re-verifies against the bytes beside it.
    #[test]
    fn an_import_persists_thumbnail_bytes_and_a_verifying_manifest_bundle() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        let receipt = import(&mut ws, album, src.path(), "big.png", &png(512, 384));
        assert_eq!(receipt.derivatives, DerivativeStatus::Decoded);

        let dir = derivatives_dir(lib.path(), receipt.asset_id);
        let stem = receipt.asset_id.simple().to_string();
        let thumb = dir.join(format!("{stem}.thumbnail.jxl"));
        let bundle_path = dir.join(format!("{stem}.derivatives.cbor"));
        assert!(thumb.is_file(), "thumbnail bytes at {}", thumb.display());
        assert!(bundle_path.is_file(), "a manifest bundle beside them");

        let bytes = fs::read(&thumb).unwrap();
        let manifests: Vec<DerivativeManifest> =
            cbor::from_slice(&fs::read(&bundle_path).unwrap()).expect("the bundle decodes");
        assert_eq!(manifests.len(), 1);
        let core = &manifests[0].core;
        assert_ne!(
            core.ciphertext_hash,
            hash::hash_bytes(&bytes),
            "the manifest addresses the ciphertext, never the plaintext on disk"
        );
        assert_ne!(
            core.nonce_prefix, [0u8; 7],
            "and it records the prefix that ciphertext was produced under"
        );
        assert_eq!(core.source_asset_id, receipt.asset_id);
        assert_eq!(
            verify_still_format(&manifests[0]),
            Ok(Some(DerivativeFormat::Jxl)),
            "the persisted format is inside the closed set"
        );

        // The bytes really are a 256 px JXL.
        let decoded = crate::media::RawshiftDecoder
            .decode(&bytes, "jxl")
            .expect("the persisted thumbnail decodes");
        assert_eq!((decoded.width(), decoded.height()), (256, 192));
    }

    /// A still already inside the tier cap gets the signed `original` sentinel: a manifest that
    /// says `original` and content-addresses the source, and **no derivative bytes on disk**.
    ///
    /// The absent bytes are the point, and they are what "the tier *references* the original"
    /// means. Writing a copy would put the original — EXIF and GPS intact — in
    /// `derivatives/{uuid}.thumbnail.{ext}` and therefore into the derivative blob of the upload
    /// bundle, which is the one place a re-encoded thumbnail is metadata-free by construction.
    /// The signed marker is still there, so this stays distinct from an absent derivative, which
    /// means "rebuild me".
    #[test]
    fn a_small_still_persists_the_original_sentinel_without_copying_it() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        let original = png(128, 96);
        let receipt = import(&mut ws, album, src.path(), "small.png", &original);
        assert_eq!(receipt.derivatives, DerivativeStatus::Decoded);
        assert_eq!(
            receipt.deferred_formats, 0,
            "the sentinel satisfies the tier, so nothing was deferred"
        );

        let dir = derivatives_dir(lib.path(), receipt.asset_id);
        let stem = receipt.asset_id.simple().to_string();

        let files: Vec<String> = fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            files,
            vec![format!("{stem}.derivatives.cbor")],
            "the sentinel writes its manifest and no derivative bytes"
        );

        let manifests: Vec<DerivativeManifest> =
            cbor::from_slice(&fs::read(dir.join(format!("{stem}.derivatives.cbor"))).unwrap())
                .expect("the bundle decodes");
        assert_eq!(manifests.len(), 1);
        assert_eq!(manifests[0].core.format, "original");
        // The sentinel references the original *blob*: it signs the original manifest's own
        // ciphertext address and nonce prefix, not the plaintext's.
        let state = ws.asset(&receipt.asset_id).expect("the asset is held");
        let original_core = &state
            .chain
            .records()
            .last()
            .expect("a create record")
            .manifest
            .core;
        assert_eq!(
            manifests[0].core.ciphertext_hash, original_core.ciphertext_hash,
            "the sentinel points at the blob a receiver already holds"
        );
        assert_eq!(
            manifests[0].core.nonce_prefix, original_core.nonce_prefix,
            "under the same key the original was encrypted with"
        );
        assert_ne!(
            manifests[0].core.ciphertext_hash,
            hash::hash_bytes(&original),
            "which is not the plaintext's address"
        );
        assert_eq!(
            verify_still_format(&manifests[0]),
            Ok(Some(DerivativeFormat::Original)),
            "the sentinel is inside the closed set"
        );
    }

    /// A format with no codec here, and bytes that are no still at all: both import as signed,
    /// verifiable originals, with EXIF-or-nothing dimensions, no placeholder, no derivative
    /// files, and the reason recorded (slice `S-B13`).
    #[test]
    fn an_undecodable_original_still_imports_with_the_reason_recorded() {
        let lib = TempDir::new().unwrap();
        let src = TempDir::new().unwrap();
        let (mut ws, album) = workspace(lib.path());

        // A real HEIC header (ISO-BMFF `ftyp heic`) with no payload: recognised, no codec.
        let mut heic = vec![0, 0, 0, 0x20];
        heic.extend_from_slice(b"ftypheic");
        heic.extend_from_slice(&[0; 16]);
        let deferred = import(&mut ws, album, src.path(), "shot.heic", &heic);
        assert_eq!(deferred.derivatives, DerivativeStatus::DeferredNoCodec);
        assert_eq!(deferred.deferred_formats, 0, "nothing was attempted");

        let sidecar = sidecar_of(lib.path(), deferred.asset_id);
        assert!(sidecar.lqip.is_none(), "no pixels, no placeholder");
        assert!(
            sidecar.dimensions.is_none(),
            "no pixels and no EXIF dimensions"
        );
        assert_eq!(
            sidecar.content_type, "image/heic",
            "the header still names the format, codec or not"
        );
        assert!(
            !derivatives_dir(lib.path(), deferred.asset_id).exists(),
            "no derivative directory is created for an asset with no derivatives"
        );

        // Not a still at all.
        let video = import(
            &mut ws,
            album,
            src.path(),
            "clip.mp4",
            b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom",
        );
        assert_eq!(video.derivatives, DerivativeStatus::NotAKnownStill);
        assert_eq!(
            sidecar_of(lib.path(), video.asset_id).content_type,
            "video/mp4",
            "the extension table still types a video"
        );

        // Both are signed, encrypted, self-verifying backups regardless.
        for id in [deferred.asset_id, video.asset_id] {
            assert_eq!(
                ws.verify(&id).unwrap(),
                crate::crypto::verify_asset::VerifyOutcome::Accept
            );
        }
    }
}
