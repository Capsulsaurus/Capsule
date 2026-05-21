use serde::{Deserialize, Serialize};

// Re-export image types for backward compatibility
pub use crate::image::types::*;
// Re-export video types for backward compatibility
pub use crate::video::types::*;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum MediaType {
    Image(ImageFormat),
    Video(VideoFormat),
}
