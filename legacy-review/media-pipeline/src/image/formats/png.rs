use std::io::Write;
use std::path::Path;

use crate::media::image::buffer::ImageBuffer;
use crate::media::image::metadata::exposure::CaptureSettings;
use crate::media::image::metadata::iptc::IptcData;
use crate::media::image::metadata::motion::{AuxiliaryImage, MotionPhotoInfo};
use crate::media::image::metadata::raw::RawSensorInfo;
use crate::media::image::metadata::{ContentMetadata, ImageMetadataExtractor};
use crate::media::image::{Image, ImageDecode, ImageEncode, ImageError, ImageMetadata};
use crate::media::metadata::c2pa::C2PAManifest;
use crate::media::metadata::exif::ExifData;
use crate::media::metadata::geo::GpsLocation;
use crate::media::metadata::icc::IccProfile;
use crate::media::metadata::xmp::XmpData;
use crate::media::metadata::{ColorSpace, DeviceMetadata};

/// An in-memory PNG image.
///
/// PNG is **not** a committed derivative format in the [tier table] (deliberately — see the
/// Thumbnails doc); it exists here only as a conversion target/source for the shared
/// `ConvertImage` plumbing (`from_raw_parts` → `get_buffer`). Byte-level PNG codec support is
/// not wired: per the S-B1 note "grow decoders only as the tier table needs", PNG encode/
/// decode return an [`ImageError`] rather than pulling a codec dependency the tier table does
/// not require.
///
/// [tier table]: https://docs/design/thumbnails/#thumbnail-and-preview-formats
#[derive(Debug, Clone)]
pub struct PngImage {
    buffer: ImageBuffer,
    metadata: ImageMetadata,
}

impl ImageMetadataExtractor for PngImage {
    fn get_date_taken(&self) -> Option<jiff::civil::DateTime> {
        self.metadata.date_taken
    }
    fn get_dimensions(&self) -> (u32, u32) {
        (self.buffer.width as u32, self.buffer.height as u32)
    }
    fn get_bit_depth(&self) -> u8 {
        self.metadata.bit_depth
    }
    fn get_color_space(&self) -> ColorSpace {
        self.buffer.color_space
    }
    fn get_file_size(&self) -> u64 {
        self.metadata.file_size_bytes
    }
    fn get_device_metadata(&self) -> Option<DeviceMetadata> {
        self.metadata.device.clone()
    }
    fn get_capture_settings(&self) -> Option<CaptureSettings> {
        self.metadata.capture_settings.clone()
    }
    fn get_location(&self) -> Option<GpsLocation> {
        self.metadata.location.clone()
    }
    fn get_content(&self) -> Option<ContentMetadata> {
        self.metadata.content.clone()
    }
    fn raw_info(&self) -> Option<RawSensorInfo> {
        self.metadata.raw_info.clone()
    }
    fn exif(&self) -> Option<ExifData> {
        self.metadata.exif.clone()
    }
    fn xmp(&self) -> Option<XmpData> {
        self.metadata.xmp.clone()
    }
    fn iptc(&self) -> Option<IptcData> {
        self.metadata.iptc.clone()
    }
    fn icc_profile(&self) -> Option<IccProfile> {
        self.metadata.icc_profile.clone()
    }
    fn motion_metadata(&self) -> Option<MotionPhotoInfo> {
        self.metadata.motion_metadata.clone()
    }
    fn auxiliary_images(&self) -> Vec<AuxiliaryImage> {
        self.metadata.auxiliary_images.clone()
    }
    fn c2pa_manifest(&self) -> Option<C2PAManifest> {
        self.metadata.c2pa_manifest.clone()
    }
}

impl Image for PngImage {
    fn get_format(&self) -> crate::media::core::types::ImageFormat {
        crate::media::core::types::ImageFormat::Png
    }

    fn get_buffer(&self) -> ImageBuffer {
        self.buffer.clone()
    }

    fn from_raw_parts(buffer: ImageBuffer, metadata: ImageMetadata) -> Result<Self, ImageError> {
        Ok(Self { buffer, metadata })
    }
}

impl ImageDecode for PngImage {
    fn decode_from_bytes(_bytes: &[u8]) -> Result<Self, ImageError> {
        Err(ImageError::Decode(
            "PNG decode is not wired — PNG is not a committed derivative format (S-B1)".into(),
        ))
    }
}

impl ImageEncode for PngImage {
    fn encode<W: Write>(&self, _writer: &mut W) -> Result<(), ImageError> {
        Err(ImageError::Encode(
            "PNG encode is not wired — PNG is not a committed derivative format (S-B1)".into(),
        ))
    }

    async fn save(&self, _path: &Path) -> Result<(), ImageError> {
        Err(ImageError::Encode(
            "PNG encode is not wired — PNG is not a committed derivative format (S-B1)".into(),
        ))
    }
}
