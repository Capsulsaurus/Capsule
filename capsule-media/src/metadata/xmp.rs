use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct XmpData {
    // --- RAW PACKET ---
    // The raw XML string. Required because XMP allows custom user-defined namespaces
    // that your struct might not cover.
    pub raw_xml: String,

    // --- PARSED NAMESPACES ---
    pub dublin_core: Option<XmpDublinCore>, // Title, Description, Rights
    pub photoshop: Option<XmpPhotoshop>,    // Color mode, ICC name
    pub crs: Option<XmpCameraRaw>,          // Adobe Lightroom edits
    pub google_depth: Option<GDepth>,       // Android Depth
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct XmpDublinCore {
    pub format: Option<String>,
    pub title: Option<HashMap<String, String>>, // Lang -> Text
    pub description: Option<HashMap<String, String>>, // Lang -> Text
    pub creator: Vec<String>,
    pub rights: Option<HashMap<String, String>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GDepth {
    pub format: String, // e.g., "RangeLinear", "RangeInverse"
    pub near: f32,
    pub far: f32,
    pub mime: String, // "image/jpeg", "image/png"
    // TODO: The actual depth data in XMP is often base64 encoded.
    // In a converter, you might want to decode this to an `AuxiliaryImage`.
    pub data_base64: Option<String>,
}

// TODO: Add `XmpCameraRaw` struct. It is massive (hundreds of fields like
// "Exposure2012", "Highlights2012"). For a generic library,
// usually storing it as a key-value map is safer than a rigid struct.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct XmpCameraRaw {
    pub settings: HashMap<String, String>,
}

/// The "http://ns.adobe.com/photoshop/1.0/" namespace.
/// Crucial for determining how to interpret pixel data (especially CMYK).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct XmpPhotoshop {
    /// The color mode of the image.
    /// CRITICAL: If this is 4 (CMYK), you must apply a CMYK->RGB transform
    /// before saving to formats like WebP or standard JPEG.
    #[serde(rename = "ColorMode")]
    pub color_mode: Option<PhotoshopColorMode>,

    /// The name of the ICC profile (e.g., "U.S. Web Coated (SWOP) v2").
    /// Note: This is the *name*, not the profile data itself. Use this to verify
    /// against the embedded `IccProfile` block.
    #[serde(rename = "ICCProfile")]
    pub icc_profile_name: Option<String>,

    /// Often used for the job title of the person in the image, or the author's title.
    #[serde(rename = "AuthorsPosition")]
    pub authors_position: Option<String>,

    /// The person who added the metadata/caption.
    #[serde(rename = "CaptionWriter")]
    pub caption_writer: Option<String>,

    /// Urgency (1-8). Legacy newsroom priority flag.
    #[serde(rename = "Urgency")]
    pub urgency: Option<u8>,

    /// Original file name before any renaming.
    #[serde(rename = "History")]
    pub history: Option<String>,

    /// Date the intellectual content was created (different from file creation).
    #[serde(rename = "DateCreated")]
    pub date_created: Option<String>, // ISO 8601

    /// City, State, Country (Legacy fields, usually mirrored in IPTC/DublinCore)
    #[serde(rename = "City")]
    pub city: Option<String>,
    #[serde(rename = "State")]
    pub state: Option<String>,
    #[serde(rename = "Country")]
    pub country: Option<String>,

    /// Identifies the sidecar file if this XMP is external (e.g. "CR3").
    #[serde(rename = "SidecarForExtension")]
    pub sidecar_for_extension: Option<String>,
}

/// Adobe Photoshop Color Modes.
/// Mapped from standard integers used in XMP.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum PhotoshopColorMode {
    Bitmap = 0,
    Grayscale = 1,
    Indexed = 2,
    RGB = 3,
    CMYK = 4,         // WARNING: Web browsers often fail to display this. Convert to RGB.
    MultiChannel = 5, // Specialized printing
    Duotone = 6,
    Lab = 7, // Device independent
    Color = 8,
    Background = 9,
    Raw = 10,
}
