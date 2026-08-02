use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct IccProfile {
    // Binary payload for color conversion.
    pub data: Vec<u8>,

    // Metadata extracted from the profile header for decision making
    pub description: Option<String>, // e.g., "Display P3"
    pub device_class: IccDeviceClass,
    pub color_space: String, // "RGB ", "CMYK", "Gray" // TODO: Make enum
    pub pcs: String,         // Profile Connection Space (XYZ / Lab) // TODO: Make enum
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum IccDeviceClass {
    Display,
    Input,
    Output,
    Link,
    ColorSpace,
    Abstract,
    NamedColor,
}
