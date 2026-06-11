use std::io::Write;
use std::path::Path;

use crate::image::buffer::ImageBuffer;
use crate::image::metadata::exposure::CaptureSettings;
use crate::image::metadata::iptc::IptcData;
use crate::image::metadata::motion::{AuxiliaryImage, MotionPhotoInfo};
use crate::image::metadata::raw::RawSensorInfo;
use crate::image::metadata::{ContentMetadata, ImageMetadataExtractor};
use crate::image::{Image, ImageDecode, ImageEncode, ImageError, ImageMetadata};
use crate::metadata::c2pa::C2PAManifest;
use crate::metadata::exif::ExifData;
use crate::metadata::geo::GpsLocation;
use crate::metadata::icc::IccProfile;
use crate::metadata::xmp::XmpData;
use crate::metadata::{ColorSpace, DeviceMetadata};

#[derive(Debug, Clone)]
pub struct WebpImage {}

impl ImageMetadataExtractor for WebpImage {
    fn get_date_taken(&self) -> Option<chrono::DateTime<chrono::Utc>> {
        unimplemented!()
    }
    fn get_dimensions(&self) -> (u32, u32) {
        unimplemented!()
    }
    fn get_bit_depth(&self) -> u8 {
        unimplemented!()
    }
    fn get_color_space(&self) -> ColorSpace {
        unimplemented!()
    }
    fn get_file_size(&self) -> u64 {
        unimplemented!()
    }
    fn get_device_metadata(&self) -> Option<DeviceMetadata> {
        unimplemented!()
    }
    fn get_capture_settings(&self) -> Option<CaptureSettings> {
        unimplemented!()
    }
    fn get_location(&self) -> Option<GpsLocation> {
        unimplemented!()
    }
    fn get_content(&self) -> Option<ContentMetadata> {
        unimplemented!()
    }
    fn raw_info(&self) -> Option<RawSensorInfo> {
        unimplemented!()
    }
    fn exif(&self) -> Option<ExifData> {
        unimplemented!()
    }
    fn xmp(&self) -> Option<XmpData> {
        unimplemented!()
    }
    fn iptc(&self) -> Option<IptcData> {
        unimplemented!()
    }
    fn icc_profile(&self) -> Option<IccProfile> {
        unimplemented!()
    }
    fn motion_metadata(&self) -> Option<MotionPhotoInfo> {
        unimplemented!()
    }
    fn auxiliary_images(&self) -> Vec<AuxiliaryImage> {
        unimplemented!()
    }
    fn c2pa_manifest(&self) -> Option<C2PAManifest> {
        unimplemented!()
    }
}

impl Image for WebpImage {
    fn get_format(&self) -> crate::core::types::ImageFormat {
        crate::core::types::ImageFormat::WebP
    }

    fn get_buffer(&self) -> ImageBuffer {
        unimplemented!()
    }

    fn from_raw_parts(_buffer: ImageBuffer, _metadata: ImageMetadata) -> Result<Self, ImageError> {
        unimplemented!()
    }
}

impl ImageDecode for WebpImage {
    fn decode_from_bytes(_bytes: &[u8]) -> Result<Self, ImageError> {
        unimplemented!()
    }
}

impl ImageEncode for WebpImage {
    fn encode<W: Write>(&self, _writer: &mut W) -> Result<(), ImageError> {
        unimplemented!()
    }

    async fn save(&self, _path: &Path) -> Result<(), ImageError> {
        unimplemented!()
    }
}
