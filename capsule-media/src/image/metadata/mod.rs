use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::{
    core::types::ImageFormat,
    image::{
        Image,
        metadata::{
            exposure::CaptureSettings,
            iptc::IptcData,
            motion::{AuxiliaryImage, MotionPhotoInfo},
            raw::RawSensorInfo,
        },
    },
    metadata::{
        ColorSpace, DeviceMetadata, c2pa::C2PAManifest, exif::ExifData, geo::GpsLocation,
        icc::IccProfile, xmp::XmpData,
    },
};

pub mod exposure;
pub mod iptc;
pub mod motion;
pub mod raw;

/// Trait for types that can extract standard metadata blocks.
pub trait ImageMetadataExtractor {
    // Basic Geometry
    fn get_dimensions(&self) -> (u32, u32);
    fn get_bit_depth(&self) -> u8;
    fn get_color_space(&self) -> ColorSpace;
    fn get_file_size(&self) -> u64;

    // Generic typed metadata
    fn get_date_taken(&self) -> Option<DateTime<Utc>>;
    fn get_device_metadata(&self) -> Option<DeviceMetadata>;
    fn get_capture_settings(&self) -> Option<CaptureSettings>;
    fn get_location(&self) -> Option<GpsLocation>;
    fn get_content(&self) -> Option<ContentMetadata>;
    fn raw_info(&self) -> Option<RawSensorInfo>;

    // Standard Blocks
    fn exif(&self) -> Option<ExifData>;
    fn xmp(&self) -> Option<XmpData>;
    fn iptc(&self) -> Option<IptcData>;
    fn icc_profile(&self) -> Option<IccProfile>;

    // Modern / Computational
    fn motion_metadata(&self) -> Option<MotionPhotoInfo>;
    fn auxiliary_images(&self) -> Vec<AuxiliaryImage>;
    fn c2pa_manifest(&self) -> Option<C2PAManifest>;
}

pub trait ImageMetadataProvider {
    fn get_metadata(&self) -> ImageMetadata;
}

impl<T: ImageMetadataExtractor + Image> ImageMetadataProvider for T {
    fn get_metadata(&self) -> ImageMetadata {
        let dimensions = self.get_dimensions();
        ImageMetadata {
            format: Some(self.get_format()),
            file_size_bytes: self.get_file_size(),
            width: dimensions.0,
            height: dimensions.1,
            bit_depth: self.get_bit_depth(),
            color_space: self.get_color_space(),

            date_taken: self.get_date_taken(),
            device: self.get_device_metadata(),
            capture_settings: self.get_capture_settings(),
            location: self.get_location(),
            content: self.get_content(),
            raw_info: self.raw_info(),

            exif: self.exif(),
            xmp: self.xmp(),
            iptc: self.iptc(),
            icc_profile: self.icc_profile(),

            motion_metadata: self.motion_metadata(),
            auxiliary_images: self.auxiliary_images(),
            c2pa_manifest: self.c2pa_manifest(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ImageMetadata {
    // --- Container & Geometry ---
    pub format: Option<ImageFormat>,
    pub file_size_bytes: u64,
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub color_space: ColorSpace,

    // --- Extracted Typed Metadata (From your original struct) ---
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_taken: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture_settings: Option<CaptureSettings>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<GpsLocation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<ContentMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw_info: Option<RawSensorInfo>,

    // --- The Raw Data Blocks (The Source of Truth) ---
    pub exif: Option<ExifData>,
    pub xmp: Option<XmpData>,
    pub iptc: Option<IptcData>,
    pub icc_profile: Option<IccProfile>,

    // --- Modern Computational Extensions ---
    pub motion_metadata: Option<MotionPhotoInfo>,
    pub auxiliary_images: Vec<AuxiliaryImage>,
    /// C2PA Block
    pub c2pa_manifest: Option<C2PAManifest>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContentMetadata {
    pub title: Option<String>,
    pub description: Option<String>,
    pub creator: Option<String>,
    pub copyright: Option<String>,
    pub rating: Option<u8>, // 0-5 stars
    pub keywords: Vec<String>,
}
