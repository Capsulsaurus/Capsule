use std::io::Write;
use std::path::Path;

use crate::media::image::buffer::ImageBuffer;
use crate::media::image::metadata::exposure::CaptureSettings;
use crate::media::image::metadata::iptc::IptcData;
use crate::media::image::metadata::motion::{AuxiliaryImage, MotionPhotoInfo};
use crate::media::image::metadata::raw::RawSensorInfo;
use crate::media::image::metadata::{ContentMetadata, ImageMetadata, ImageMetadataExtractor};
use crate::media::image::types::{ImageFormat, RawImageFormat};
use crate::media::image::{FormatOp, Image, ImageDecode, ImageEncode, ImageError};
use crate::media::metadata::c2pa::C2PAManifest;
use crate::media::metadata::exif::ExifData;
use crate::media::metadata::geo::GpsLocation;
use crate::media::metadata::icc::IccProfile;
use crate::media::metadata::xmp::XmpData;
use crate::media::metadata::{ColorSpace, DeviceMetadata};

/// Camera RAW codec — **not implemented in this build** (slice `S-B13`).
///
/// This type is *uninhabited*: an `enum` with no variants, so no value of it can ever exist.
/// Every `&self` method below is therefore a total `match *self {}` — an empty match the
/// compiler proves unreachable at zero runtime cost (the `std::convert::Infallible` idiom).
/// There is no panicking stub left anywhere on this path.
///
/// Previously [`from_path`](Self::from_path) handed back a `RawImage` holding an empty
/// zero-by-zero buffer, which then panicked the process the moment anything asked it for
/// metadata. Every constructor now returns [`ImageError::UnsupportedFormat`] up front instead,
/// naming the concrete [`RawImageFormat`] that was requested.
///
/// Implementing a RAW decoder means replacing this `enum` with a real `struct` **and** adding
/// the corresponding [`ImageFormat::Raw`] variants to
/// [`SUPPORTED_IMAGE_FORMATS`](crate::media::image::types::SUPPORTED_IMAGE_FORMATS); the
/// regression gate in `media::fs` fails until both move together.
#[derive(Debug, Clone)]
pub enum RawImage {}

impl RawImage {
    /// Always [`ImageError::UnsupportedFormat`]: no RAW decoder is linked into this build.
    ///
    /// Kept as an inherent `async` constructor (rather than deferring to
    /// [`ImageReader::from_path`](crate::media::image::ImageReader::from_path)) because the
    /// caller in [`media::fs`](crate::media::fs) already knows the concrete
    /// [`RawImageFormat`] and the error needs to name it.
    pub async fn from_path(
        _path: impl AsRef<Path>,
        kind: RawImageFormat,
    ) -> Result<Self, ImageError> {
        Err(ImageError::UnsupportedFormat {
            format: ImageFormat::Raw(kind),
            op: FormatOp::Decode,
        })
    }
}

impl ImageMetadataExtractor for RawImage {
    fn get_date_taken(&self) -> Option<jiff::civil::DateTime> {
        match *self {}
    }
    fn get_dimensions(&self) -> (u32, u32) {
        match *self {}
    }
    fn get_bit_depth(&self) -> u8 {
        match *self {}
    }
    fn get_color_space(&self) -> ColorSpace {
        match *self {}
    }
    fn get_file_size(&self) -> u64 {
        match *self {}
    }
    fn get_device_metadata(&self) -> Option<DeviceMetadata> {
        match *self {}
    }
    fn get_capture_settings(&self) -> Option<CaptureSettings> {
        match *self {}
    }
    fn get_location(&self) -> Option<GpsLocation> {
        match *self {}
    }
    fn get_content(&self) -> Option<ContentMetadata> {
        match *self {}
    }
    fn raw_info(&self) -> Option<RawSensorInfo> {
        match *self {}
    }
    fn exif(&self) -> Option<ExifData> {
        match *self {}
    }
    fn xmp(&self) -> Option<XmpData> {
        match *self {}
    }
    fn iptc(&self) -> Option<IptcData> {
        match *self {}
    }
    fn icc_profile(&self) -> Option<IccProfile> {
        match *self {}
    }
    fn motion_metadata(&self) -> Option<MotionPhotoInfo> {
        match *self {}
    }
    fn auxiliary_images(&self) -> Vec<AuxiliaryImage> {
        match *self {}
    }
    fn c2pa_manifest(&self) -> Option<C2PAManifest> {
        match *self {}
    }
}

impl Image for RawImage {
    fn get_format(&self) -> ImageFormat {
        match *self {}
    }

    fn get_buffer(&self) -> ImageBuffer {
        match *self {}
    }

    /// Always [`ImageError::UnsupportedFormat`]. Unlike the single-format stubs there is no
    /// single `RawImageFormat` to name here — the caller only supplied a buffer — so the error
    /// reports the DNG variant as the canonical stand-in for "some RAW flavour".
    fn from_raw_parts(_buffer: ImageBuffer, _metadata: ImageMetadata) -> Result<Self, ImageError> {
        Err(ImageError::UnsupportedFormat {
            format: ImageFormat::Raw(RawImageFormat::Dng),
            op: FormatOp::Convert,
        })
    }
}

impl ImageDecode for RawImage {
    /// Always [`ImageError::UnsupportedFormat`]. Prefer [`RawImage::from_path`], which names the
    /// concrete [`RawImageFormat`] the caller asked for.
    fn decode_from_bytes(_bytes: &[u8]) -> Result<Self, ImageError> {
        Err(ImageError::UnsupportedFormat {
            format: ImageFormat::Raw(RawImageFormat::Dng),
            op: FormatOp::Decode,
        })
    }
}

impl ImageEncode for RawImage {
    fn encode<W: Write>(&self, _writer: &mut W) -> Result<(), ImageError> {
        match *self {}
    }

    async fn save(&self, _path: &Path) -> Result<(), ImageError> {
        match *self {}
    }
}
