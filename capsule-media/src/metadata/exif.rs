use num_rational::Ratio;
use serde::{Deserialize, Serialize};

use crate::metadata::orientation::Orientation;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExifData {
    // --- RAW BLOB ---
    // If we are just copying data from JPG->JPG, use this raw buffer
    // to avoid parsing/re-serializing errors.
    #[serde(skip)]
    pub raw_bytes: Option<Vec<u8>>,

    // --- PARSED STANDARD TAGS ---
    pub ifd0: Ifd0Tags,
    pub exif_ifd: ExifSubIfdTags,
    pub gps: Option<GpsTags>,

    // --- PROPRIETARY ---
    // The "MakerNote" is a binary blob inside the Exif IFD.
    // TODO: Do not attempt to parse this into a struct unless you have a
    // manufacturer-specific decoder. It relies on absolute offsets.
    // Store it here to inject it back into the new EXIF block.
    pub makernote: Option<Vec<u8>>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Ifd0Tags {
    pub make: Option<String>,
    pub model: Option<String>,
    pub orientation: Option<Orientation>,
    pub software: Option<String>,
    pub datetime: Option<String>, // Format: YYYY:MM:DD HH:MM:SS
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ExifSubIfdTags {
    pub exposure_time: Option<Ratio<u32>>,
    pub f_number: Option<Ratio<u32>>,
    pub iso_speed: Option<u32>,
    pub date_time_original: Option<String>,
    pub date_time_digitized: Option<String>,
    pub shutter_speed_value: Option<Ratio<u32>>, // APEX value
    pub aperture_value: Option<Ratio<u32>>,      // APEX value
    pub brightness_value: Option<Ratio<u32>>,
    pub exposure_bias_value: Option<Ratio<u32>>,
    pub max_aperture_value: Option<Ratio<u32>>,
    pub focal_length: Option<Ratio<u32>>,
    pub lens_make: Option<String>,
    pub lens_model: Option<String>,
    // Sub-second timing is critical for burst shots sorting
    pub subsec_time: Option<String>,
    pub subsec_time_original: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct GpsTags {
    pub latitude: f64, // Converted from Ratio<u32> degrees/minutes/seconds
    pub longitude: f64,
    pub altitude: Option<f64>,
    pub timestamp: Option<String>, // UTC
    pub map_datum: Option<String>, // TODO: Add enum
}
