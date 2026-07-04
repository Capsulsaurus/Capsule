use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct MotionPhotoInfo {
    pub format: MotionFormat,

    /// Offset from the end of the file (common in MicroVideo)
    pub data_offset: Option<u64>,

    /// Length of the embedded video stream
    pub data_length: Option<u64>,

    /// Apple Live Photo UUID (connects HEIC to a separate .MOV file)
    pub content_identifier: Option<String>,
    // TODO: If you support file extraction, add a method to stream
    // this data out to a separate .mp4 file.
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum MotionFormat {
    GoogleMicroVideo, // JPEG embedded
    AppleLivePhoto,   // External file link
    SamsungMotion,    // SEF (Samsung Extended Format)
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AuxiliaryImage {
    pub type_: AuxType,

    // The width/height often differs from the main image (e.g. Depth is lower res)
    pub width: u32,
    pub height: u32,

    // The raw pixel data of the map (grayscale 8-bit or 16-bit)
    pub data: Vec<u8>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum AuxType {
    DepthMap,
    GainMap,    // For HDR display (ISO 21496-1)
    AlphaMatte, // Portrait mode transparency mask
    Disparity,  // Stereo disparity
}
