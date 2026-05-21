//! This module provides generic metadata structs related to RAW image formats.
//!
//!

use serde::{Deserialize, Serialize};

// TODO: Expand these to match what existing raw libraries have vv

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct RawSensorInfo {
    /// Color Filter Array pattern, e.g., RGGB, BGGR, XTrans
    pub cfa_pattern: Option<CfaPattern>,

    /// The geometric active area of the sensor
    pub active_area: Option<Rect>,

    /// Black level (per channel). Subtract this before processing.
    pub black_levels: Vec<u16>,

    /// White level (saturation point).
    pub white_level: u16,

    /// Default crop origin/size (RAWs often have garbage pixels on edges)
    pub default_crop: Option<Rect>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum CfaPattern {
    RGGB,
    BGGR,
    GRBG,
    GBRG,
    XTrans, // Fujifilm 6x6
    Monochrome,
    Other(String),
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}
