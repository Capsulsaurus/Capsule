use serde::{Deserialize, Serialize};

pub mod c2pa;
pub mod exif;
pub mod geo;
pub mod icc;
pub mod orientation;
pub mod xmp;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, Default)]
pub enum ColorSpace {
    #[default]
    Srgb,
    AdobeRgb,
    DisplayP3,
    ProPhoto,
    Linear,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DeviceMetadata {
    pub make: String,         // e.g., "Sony"
    pub model: String,        // e.g., "A7IV"
    pub lens: Option<String>, // e.g., "FE 35mm F1.4 GM"
    pub serial_number: Option<String>,
    pub software_version: Option<String>,
}
