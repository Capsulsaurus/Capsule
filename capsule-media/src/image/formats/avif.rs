use std::io::Write;
use std::path::Path;

use crate::image::metadata::{
    ContentMetadata, ImageMetadataExtractor,
    exposure::CaptureSettings,
    iptc::IptcData,
    motion::{AuxiliaryImage, MotionPhotoInfo},
    raw::RawSensorInfo,
};
use crate::image::{
    Image, ImageDecode, ImageEncode, ImageError, ImageMetadata, buffer::ImageBuffer,
};
use crate::metadata::{
    ColorSpace, DeviceMetadata, c2pa::C2PAManifest, exif::ExifData, geo::GpsLocation,
    icc::IccProfile, xmp::XmpData,
};

#[derive(Debug, Clone)]
pub struct AvifImage {}

impl ImageMetadataExtractor for AvifImage {
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

impl Image for AvifImage {
    fn get_format(&self) -> crate::core::types::ImageFormat {
        crate::core::types::ImageFormat::Avif
    }

    fn get_buffer(&self) -> ImageBuffer {
        unimplemented!()
    }

    fn from_raw_parts(_buffer: ImageBuffer, _metadata: ImageMetadata) -> Result<Self, ImageError> {
        unimplemented!()
    }
}

impl ImageDecode for AvifImage {
    fn decode_from_bytes(_bytes: &[u8]) -> Result<Self, ImageError> {
        unimplemented!()
    }
}

impl ImageEncode for AvifImage {
    fn encode<W: Write>(&self, _writer: &mut W) -> Result<(), ImageError> {
        unimplemented!()
    }

    async fn save(&self, _path: &Path) -> Result<(), ImageError> {
        unimplemented!()
    }
}
