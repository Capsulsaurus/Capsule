use capsule_core::crypto::hash;
use capsule_core::crypto::provenance::DerivativeManifest;
use capsule_core::derivative_format::{DerivativeFormat, verify_still_format};
use capsule_core::media::Decoder as _;
use capsule_core::sidecar::sidecar_v1::{SIDECAR_SCHEMA_V1, SidecarV1};
use rawshift_image::core::metadata::{ImageInfo, ImageMetadata};
use rawshift_image::core::{BitDepth, MetadataEmbedOptions};
use rawshift_image::formats::encode_rgb_image_to_vec;
use rawshift_image::formats::export::{
    CommonEncodeOptions, EncodeOptions, JpegEncEncodeConfig, ZunePngEncodeConfig,
};
use tempfile::TempDir;

use super::super::{DerivativeStatus, SignedImportOptions, Workspace, fast_workspace};
use super::*;

/// Delivery formats this build encodes: JXL, plus AVIF under `capsule-core`'s
/// `media-linked-codecs`.
///
/// Read from what core generates rather than from a `cfg!` here: the codec feature is
/// `capsule-core`'s, and a workspace build can turn it on through another dependent (the CLI)
/// without turning on this crate's pass-through of the same name. The preview tier is
/// generated exactly when the linked codecs are.
fn delivery_formats_encoded() -> usize {
    if DerivativeTier::GENERATED.contains(&DerivativeTier::Preview) {
        2
    } else {
        1
    }
}

/// Manifests a still larger than the thumbnail cap yields: every generated tier times every
/// encoded delivery format.
fn encoded_pairs_per_large_still() -> usize {
    DerivativeTier::GENERATED.len() * delivery_formats_encoded()
}

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
pub(super) fn jpeg(width: u32, height: u32, orientation: Option<u16>) -> Vec<u8> {
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
        }),
    )
    .expect("the fixture PNG encodes")
}

/// A workspace with fast Argon2 params and its default album created.
pub(super) fn workspace(dir: &Path) -> (Workspace, Uuid) {
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

/// **The acceptance case, and the first production caller of `capsule_core::lqip`.**
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
        "two (tier, format) pairs have no encoder: the thumbnail's AVIF and WebP without \
             `media-linked-codecs`, or WebP for both the thumbnail and the preview with it"
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
    assert_eq!(lqip.format_version, capsule_core::lqip::LQIP_FORMAT_V1);
    assert!(
        Lqip::from_bytes(&lqip.chromahash).is_ok(),
        "the stored payload is a structurally valid chromahash"
    );

    assert!(
        sidecar.verify(&ws.user_ik_public()),
        "the sidecar signature covers the placeholder it now carries"
    );
}

/// **Issue #588, through the real import.** With `media-avif-decode` an AVIF original is a
/// decodable still: it imports `Decoded`, with pixel dimensions, a placeholder and a JXL
/// thumbnail, where without the feature it would be `DeferredNoCodec`. The source AVIF comes
/// from ravif (`media-linked-codecs`) because the suite commits no binary fixtures.
#[cfg(all(feature = "media-avif-decode", feature = "media-linked-codecs"))]
#[test]
fn an_avif_original_imports_with_dimensions_an_lqip_and_derivatives() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (mut ws, album) = workspace(lib.path());

    let avif = encode_rgb_image_to_vec(
        &frame(512, 384),
        &ImageMetadata::default(),
        &EncodeOptions::AvifRavif(rawshift_image::formats::export::RavifEncodeConfig {
            common: common(MetadataEmbedOptions::none()),
            quality: 90,
            speed: 6,
        }),
    )
    .expect("the fixture AVIF encodes");
    let receipt = import(&mut ws, album, src.path(), "photo.avif", &avif);
    assert_eq!(receipt.derivatives, DerivativeStatus::Decoded);

    let sidecar = sidecar_of(lib.path(), receipt.asset_id);
    assert_eq!(sidecar.content_type, "image/avif");
    let dimensions = sidecar.dimensions.as_ref().expect("pixel dimensions");
    assert_eq!((dimensions.width, dimensions.height), (512, 384));
    assert!(
        sidecar.lqip.is_some(),
        "the placeholder came from the AVIF's pixels"
    );
    assert!(sidecar.verify(&ws.user_ik_public()));

    let stem = receipt.asset_id.simple().to_string();
    let thumb = derivatives_dir(lib.path(), receipt.asset_id).join(format!("{stem}.thumbnail.jxl"));
    let decoded = capsule_core::media::RawshiftDecoder
        .decode(&fs::read(&thumb).unwrap(), "jxl")
        .expect("the thumbnail decodes");
    assert_eq!((decoded.width(), decoded.height()), (256, 192));
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
    // One manifest per generated (tier, format) pair: the thumbnail JXL alone without
    // `media-linked-codecs`, thumbnail and preview in JXL and AVIF with it. The thumbnail
    // JXL is first either way.
    assert_eq!(manifests.len(), encoded_pairs_per_large_still());
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
    let decoded = capsule_core::media::RawshiftDecoder
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
    // The sentinel satisfies the thumbnail tier, so nothing is deferred there. The preview
    // tier (generated under `media-linked-codecs`) has no cap, so it is a re-encode and
    // defers whatever this build cannot encode — WebP.
    let preview_deferred: u32 = if DerivativeTier::GENERATED.contains(&DerivativeTier::Preview) {
        (DerivativeFormat::STILL_DELIVERY_ORDER.len() - delivery_formats_encoded()) as u32
    } else {
        0
    };
    assert_eq!(
        receipt.deferred_formats, preview_deferred,
        "the sentinel satisfies the thumbnail tier, so nothing was deferred for it"
    );

    let dir = derivatives_dir(lib.path(), receipt.asset_id);
    let stem = receipt.asset_id.simple().to_string();

    let files: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| !name.contains(".preview."))
        .collect();
    assert_eq!(
        files,
        vec![format!("{stem}.derivatives.cbor")],
        "the sentinel writes its manifest and no thumbnail bytes"
    );

    let manifests: Vec<DerivativeManifest> =
        cbor::from_slice(&fs::read(dir.join(format!("{stem}.derivatives.cbor"))).unwrap())
            .expect("the bundle decodes");
    let manifests: Vec<DerivativeManifest> = manifests
        .into_iter()
        .filter(|m| m.core.role == DerivativeRole::Thumbnail)
        .collect();
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

/// **Decision 22's degradation path, through the real import.** A codec that refuses a frame
/// the decoder accepted costs this asset its thumbnail and **nothing else**: the original
/// commits, signed and self-verifying, with real pixel dimensions and a real placeholder,
/// and the run reports `DecodeFailed` so somebody can look at it.
///
/// Reverting `prepare_still`'s match arm to a bare `?` must fail this test — that is what it
/// is for. Before the review round the code did exactly that, and because the failure
/// happened *before* `write_asset_files`, an encoder refusal lost the original from the
/// backup outright.
#[test]
fn a_codec_refusal_costs_the_thumbnail_and_not_the_backup() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (mut ws, album) = workspace(lib.path());
    let path = src.path().join("photo.png");
    fs::write(&path, png(512, 384)).unwrap();

    let receipt = super::with_sealer_fault(super::SealerFault::Refuse, || {
        ws.import_asset_with(album, &path, &SignedImportOptions::default())
            .expect("a codec refusal must never fail the import")
    });

    assert_eq!(
        receipt.derivatives,
        DerivativeStatus::DecodeFailed,
        "reported as a real problem rather than an expected gap"
    );
    assert_eq!(receipt.deferred_formats, 0);

    // The half that must not be lost: a signed, encrypted, self-verifying original.
    assert_eq!(
        ws.verify(&receipt.asset_id).unwrap(),
        capsule_core::crypto::verify_asset::VerifyOutcome::Accept
    );
    let sidecar = sidecar_of(lib.path(), receipt.asset_id);
    let dimensions = sidecar.dimensions.as_ref().expect("real pixel dimensions");
    assert_eq!((dimensions.width, dimensions.height), (512, 384));
    assert_eq!(
        sidecar.lqip.as_ref().map(|l| l.chromahash.len()),
        Some(32),
        "the placeholder came from the decode, which succeeded"
    );
    assert!(
        !derivatives_dir(lib.path(), receipt.asset_id).exists(),
        "and no derivative was written"
    );
}

/// **Decision 23's guard, through the real import.** A codec that *panics* on a frame the
/// decoder accepted is caught, and the import still commits.
///
/// Driven through `import_asset_with` rather than by calling `guarded` directly: a test that
/// calls the guard itself stays green even if every production call site is deleted, which
/// is precisely the hole this replaces. Deleting the guard around generation makes this test
/// abort the process rather than fail — which is the failure mode it exists to prevent.
#[test]
fn a_codec_panic_is_caught_and_the_import_still_commits() {
    let lib = TempDir::new().unwrap();
    let src = TempDir::new().unwrap();
    let (mut ws, album) = workspace(lib.path());
    let path = src.path().join("photo.png");
    fs::write(&path, png(512, 384)).unwrap();

    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let receipt = super::with_sealer_fault(super::SealerFault::Panic, || {
        ws.import_asset_with(album, &path, &SignedImportOptions::default())
            .expect("a codec panic must never fail the import")
    });
    std::panic::set_hook(previous);

    assert_eq!(receipt.derivatives, DerivativeStatus::DecodeFailed);
    assert_eq!(
        ws.verify(&receipt.asset_id).unwrap(),
        capsule_core::crypto::verify_asset::VerifyOutcome::Accept,
        "one panicking photo does not cost the asset, let alone the rest of the run"
    );
    let sidecar = sidecar_of(lib.path(), receipt.asset_id);
    assert!(sidecar.lqip.is_some(), "the placeholder still landed");
}

/// A format with no codec here, and bytes that are no still at all: both import as signed,
/// verifiable originals, with EXIF-or-nothing dimensions, no placeholder, no derivative
/// files, and the reason recorded.
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

    // Not a still at all: a video, whose derivatives are deferred until video codecs land
    // rather than inapplicable (issue #695).
    let video = import(
        &mut ws,
        album,
        src.path(),
        "clip.mp4",
        b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom",
    );
    assert_eq!(video.derivatives, DerivativeStatus::DeferredNoCodec);
    assert_eq!(
        sidecar_of(lib.path(), video.asset_id).content_type,
        "video/mp4",
        "the extension table still types a video"
    );

    // Both are signed, encrypted, self-verifying backups regardless.
    for id in [deferred.asset_id, video.asset_id] {
        assert_eq!(
            ws.verify(&id).unwrap(),
            capsule_core::crypto::verify_asset::VerifyOutcome::Accept
        );
    }
}

/// Seal `bytes` as a guest drop claiming `content_type`, stage it, and adopt it.
fn adopt(
    ws: &mut Workspace,
    album: Uuid,
    bytes: &[u8],
    content_type: &str,
) -> (capsule_core::drop::AdoptedDrop, Vec<u8>) {
    use capsule_core::drop::{DropAdopter, LinkCaps, UploadLinkIssuer, seal_drop};

    let link = UploadLinkIssuer::create_link(ws, LinkCaps::default(), None).unwrap();
    let sealed = seal_drop(bytes, &link.drop_pubkey, content_type).unwrap();
    let ciphertext = sealed.ciphertext.clone();
    let drop_id = ws.receive_drop(link.link_id, sealed).unwrap();
    let adopted = DropAdopter::adopt(ws, drop_id, album).expect("the adoption commits");
    (adopted, ciphertext)
}

/// **Issue #555's acceptance case.** An adopted drop runs the import's still pipeline: a
/// decodable JPEG gets pixel dimensions, a chromahash LQIP and a header-derived
/// `content_type` in its signed sidecar, and signed thumbnail derivatives on disk — under
/// their own AMK-derived keys, beside an original whose key is the guest's wrapped `K` —
/// and the asset still `verify`-accepts and pushes with those derivatives in its bundle.
#[test]
fn a_decodable_drop_adopts_with_dimensions_an_lqip_and_derivatives() {
    let lib = TempDir::new().unwrap();
    let (mut ws, album) = workspace(lib.path());

    // The guest claims a generic type; the header is what the sidecar records.
    let (adopted, staged) = adopt(
        &mut ws,
        album,
        &jpeg(512, 384, None),
        "application/octet-stream",
    );
    let asset_id = adopted.asset_id();
    assert_eq!(adopted.derivatives, DerivativeStatus::Decoded);
    assert_eq!(
        adopted.manifest.core.key_mode,
        capsule_core::crypto::provenance::KeyMode::Wrapped
    );

    let sidecar = sidecar_of(lib.path(), asset_id);
    let dimensions = sidecar.dimensions.as_ref().expect("dimensions from pixels");
    assert_eq!((dimensions.width, dimensions.height), (512, 384));
    assert_eq!(sidecar.content_type, "image/jpeg", "header-derived");
    let lqip = sidecar.lqip.as_ref().expect("the LQIP producer ran");
    assert_eq!(lqip.chromahash.len(), 32);
    assert!(sidecar.verify(&ws.user_ik_public()));

    // At least one signed derivative manifest, sealed under a prefix other than the
    // guest's, beside thumbnail bytes on disk.
    let dir = derivatives_dir(lib.path(), asset_id);
    let stem = asset_id.simple().to_string();
    let manifests: Vec<DerivativeManifest> =
        cbor::from_slice(&fs::read(dir.join(format!("{stem}.derivatives.cbor"))).unwrap())
            .expect("the bundle decodes");
    assert_eq!(manifests.len(), encoded_pairs_per_large_still());
    assert_eq!(manifests[0].core.source_asset_id, asset_id);
    assert_eq!(
        verify_still_format(&manifests[0]),
        Ok(Some(DerivativeFormat::Jxl))
    );
    assert_ne!(
        manifests[0].core.nonce_prefix,
        adopted.manifest.core.nonce_prefix
    );
    assert!(dir.join(format!("{stem}.thumbnail.jxl")).is_file());

    // The asset still verifies, its index row carries the dimensions, and a push bundle
    // carries the guest's ciphertext byte for byte plus derivatives whose re-derived
    // ciphertext matches their signed address (the bundle reader drops any that do not).
    assert_eq!(
        ws.verify(&asset_id).unwrap(),
        capsule_core::crypto::verify_asset::VerifyOutcome::Accept
    );
    let row = ws
        .db()
        .find_by_uuid(&asset_id.to_string())
        .unwrap()
        .expect("an index row");
    assert_eq!((row.width, row.height), (Some(512), Some(384)));
    let bundle = ws.upload_bundle(&asset_id).unwrap();
    assert_eq!(bundle.original.to_vec().unwrap(), staged);
    assert_eq!(bundle.content_type, "image/jpeg");
    assert!(
        !bundle.derivatives.is_empty(),
        "the derived-key derivatives ship beside the wrapped-key original"
    );
}

/// An undecodable drop still adopts: no placeholder, no derivatives, the reason reported in
/// [`AdoptedDrop::derivatives`](capsule_core::drop::AdoptedDrop::derivatives), and a
/// `verify`-accepting asset regardless.
#[test]
fn an_undecodable_drop_still_adopts_with_the_reason_reported() {
    let lib = TempDir::new().unwrap();
    let (mut ws, album) = workspace(lib.path());

    let mut heic = vec![0, 0, 0, 0x20];
    heic.extend_from_slice(b"ftypheic");
    heic.extend_from_slice(&[0; 16]);
    let cases: [(&[u8], &str, DerivativeStatus, &str); 3] = [
        // Recognised by its header, no codec in this build.
        (
            &heic,
            "image/jpeg",
            DerivativeStatus::DeferredNoCodec,
            "image/heic",
        ),
        // A JPEG header over bytes that do not decode.
        (
            b"\xFF\xD8\xFF not really a photo",
            "image/jpeg",
            DerivativeStatus::DecodeFailed,
            "image/jpeg",
        ),
        // No still at all: the guest's claimed type is the fallback.
        (
            b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom",
            "video/mp4",
            DerivativeStatus::NotAKnownStill,
            "video/mp4",
        ),
    ];
    for (bytes, claimed, status, content_type) in cases {
        let (adopted, _) = adopt(&mut ws, album, bytes, claimed);
        let asset_id = adopted.asset_id();
        assert_eq!(adopted.derivatives, status, "{content_type}");
        assert_eq!(adopted.deferred_formats, 0, "nothing was attempted");
        let sidecar = sidecar_of(lib.path(), asset_id);
        assert_eq!(sidecar.content_type, content_type);
        assert!(sidecar.lqip.is_none());
        assert!(sidecar.dimensions.is_none(), "no pixels and no EXIF");
        assert!(
            !derivatives_dir(lib.path(), asset_id)
                .join(format!("{}.derivatives.cbor", asset_id.simple()))
                .exists(),
            "no derivative bundle for an asset with no derivatives"
        );
        assert_eq!(
            ws.verify(&asset_id).unwrap(),
            capsule_core::crypto::verify_asset::VerifyOutcome::Accept
        );
    }
}
