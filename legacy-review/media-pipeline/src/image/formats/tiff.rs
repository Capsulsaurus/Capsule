use std::io::Write;
use std::path::Path;

use crate::media::image::buffer::ImageBuffer;
use crate::media::image::metadata::exposure::CaptureSettings;
use crate::media::image::metadata::iptc::IptcData;
use crate::media::image::metadata::motion::{AuxiliaryImage, MotionPhotoInfo};
use crate::media::image::metadata::raw::RawSensorInfo;
use crate::media::image::metadata::{ContentMetadata, ImageMetadataExtractor};
use crate::media::image::types::ImageFormat;
use crate::media::image::{FormatOp, Image, ImageDecode, ImageEncode, ImageError, ImageMetadata};
use crate::media::metadata::c2pa::C2PAManifest;
use crate::media::metadata::exif::ExifData;
use crate::media::metadata::geo::GpsLocation;
use crate::media::metadata::icc::IccProfile;
use crate::media::metadata::xmp::XmpData;
use crate::media::metadata::{ColorSpace, DeviceMetadata};

/// TIFF codec — **not implemented in this build** (slice `S-B13`).
///
/// This type is *uninhabited*: an `enum` with no variants, so no value of it can ever exist.
/// Every `&self` method below is therefore a total `match *self {}` — an empty match the
/// compiler proves unreachable at zero runtime cost (the `std::convert::Infallible` idiom).
/// There is no panicking stub left anywhere on this path.
///
/// The only two ways to obtain a value are [`ImageDecode::decode_from_bytes`] and
/// [`Image::from_raw_parts`] (which every `ConvertImage::convert_from*` helper funnels
/// through), and both return [`ImageError::UnsupportedFormat`] instead. Callers therefore see a
/// typed, propagatable error rather than an aborted process.
///
/// Implementing the codec means replacing this `enum` with a real `struct` **and** adding
/// [`ImageFormat::Tiff`] to [`SUPPORTED_IMAGE_FORMATS`](crate::media::image::types::SUPPORTED_IMAGE_FORMATS);
/// the regression gate in `media::fs` fails until both move together.
#[derive(Debug, Clone)]
pub enum TiffImage {}

impl TiffImage {
    /// The format this codec would handle, used for the [`ImageError::UnsupportedFormat`]
    /// payload so callers can report *which* format they asked for.
    pub const FORMAT: ImageFormat = ImageFormat::Tiff;
}

impl ImageMetadataExtractor for TiffImage {
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

impl Image for TiffImage {
    fn get_format(&self) -> ImageFormat {
        match *self {}
    }

    fn get_buffer(&self) -> ImageBuffer {
        match *self {}
    }

    /// Always [`ImageError::UnsupportedFormat`]: this build has no TIFF encoder, so a
    /// buffer + metadata pair cannot be materialized into a TIFF image. This is the funnel
    /// every `ConvertImage::convert_from*` / `convert_to*` helper goes through, hence
    /// [`FormatOp::Convert`].
    fn from_raw_parts(_buffer: ImageBuffer, _metadata: ImageMetadata) -> Result<Self, ImageError> {
        Err(ImageError::UnsupportedFormat {
            format: Self::FORMAT,
            op: FormatOp::Convert,
        })
    }
}

impl ImageDecode for TiffImage {
    /// Always [`ImageError::UnsupportedFormat`]: this build has no TIFF decoder.
    fn decode_from_bytes(_bytes: &[u8]) -> Result<Self, ImageError> {
        Err(ImageError::UnsupportedFormat {
            format: Self::FORMAT,
            op: FormatOp::Decode,
        })
    }
}

impl ImageEncode for TiffImage {
    fn encode<W: Write>(&self, _writer: &mut W) -> Result<(), ImageError> {
        match *self {}
    }

    async fn save(&self, _path: &Path) -> Result<(), ImageError> {
        match *self {}
    }
}
