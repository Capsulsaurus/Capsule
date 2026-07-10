use std::io::Write;
use std::path::Path;

use crate::media::image::buffer::{ComponentType, ImageBuffer, PixelFormat};
use crate::media::image::metadata::exposure::CaptureSettings;
use crate::media::image::metadata::iptc::IptcData;
use crate::media::image::metadata::motion::{AuxiliaryImage, MotionPhotoInfo};
use crate::media::image::metadata::raw::RawSensorInfo;
use crate::media::image::metadata::{ContentMetadata, ImageMetadata, ImageMetadataExtractor};
use crate::media::image::types::{ImageFormat, RawImageFormat};
use crate::media::image::{Image, ImageDecode, ImageEncode, ImageError};
use crate::media::metadata::c2pa::C2PAManifest;
use crate::media::metadata::exif::ExifData;
use crate::media::metadata::geo::GpsLocation;
use crate::media::metadata::icc::IccProfile;
use crate::media::metadata::xmp::XmpData;
use crate::media::metadata::{ColorSpace, DeviceMetadata};

#[derive(Debug, Clone)]
pub struct RawImage {
    pub kind: RawImageFormat,
    pub buffer: ImageBuffer,
    // TODO: Add anything else necessary based on raw decoding implementation
}

impl RawImage {
    pub async fn from_path(
        _path: impl AsRef<Path>,
        kind: RawImageFormat,
    ) -> Result<Self, ImageError> {
        // Placeholder for potentially reading metadata or validation
        Ok(Self {
            kind,
            buffer: ImageBuffer::new(
                vec![],
                0,
                0,
                PixelFormat::Rgb,
                ComponentType::U8,
                ColorSpace::Srgb,
            )?,
        })
    }
}

impl ImageMetadataExtractor for RawImage {
    fn get_date_taken(&self) -> Option<jiff::civil::DateTime> {
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

impl Image for RawImage {
    fn get_format(&self) -> ImageFormat {
        ImageFormat::Raw(self.kind.clone())
    }

    fn get_buffer(&self) -> ImageBuffer {
        self.buffer.clone()
    }

    fn from_raw_parts(_buffer: ImageBuffer, _metadata: ImageMetadata) -> Result<Self, ImageError> {
        unimplemented!()
    }
}

impl ImageDecode for RawImage {
    fn decode_from_bytes(_bytes: &[u8]) -> Result<Self, ImageError> {
        unimplemented!()
    }
}

impl ImageEncode for RawImage {
    fn encode<W: Write>(&self, _writer: &mut W) -> Result<(), ImageError> {
        unimplemented!()
    }

    async fn save(&self, _path: &Path) -> Result<(), ImageError> {
        unimplemented!()
    }
}
