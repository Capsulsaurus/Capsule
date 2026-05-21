use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct CaptureSettings {
    pub iso: u32,
    pub aperture: f32,         // f/1.8
    pub shutter_speed: String, // "1/250" (stored as string to preserve fraction)
    pub focal_length: f32,     // 35.0
    pub focal_length_35mm: Option<f32>,
    pub exposure_bias: f32,           // 0.0, -1.0, etc.
    pub white_balance: String,        // "Auto", "Manual", "5500K"
    pub capture_time: Option<String>, // ISO 8601 string
}
