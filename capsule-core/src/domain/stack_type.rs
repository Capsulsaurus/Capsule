use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StackType {
    RawJpeg,
    Burst,
    LivePhoto,
    Portrait,
    SmartSelection,
    HdrBracket,
    FocusStack,
    PixelShift,
    Panorama,
    Proxy,
    Chaptered,
    DualAudio,
    Custom,
}

#[cfg(test)]
mod tests;
