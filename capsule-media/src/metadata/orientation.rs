use serde::{Deserialize, Serialize};

/// Represents the orientation of the pixel data relative to the camera sensor.
/// Conversion logic: If you rotate pixels, you MUST reset this to `TopLeft`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum Orientation {
    TopLeft = 1,
    TopRight = 2,
    BottomRight = 3,
    BottomLeft = 4,
    LeftTop = 5,
    RightTop = 6,
    RightBottom = 7,
    LeftBottom = 8,
}
