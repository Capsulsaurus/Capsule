use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::image::types::{ImageFormat, ImageOutputSettings};
use crate::video::types::{VideoFormat, VideoOutputSettings};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum TranscodeTask {
    Image(ImageTranscodeTask),
    Video(VideoTranscodeTask),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ImageTranscodeTask {
    pub input: PathBuf,
    pub input_type: ImageFormat,
    pub output: PathBuf,
    pub output_settings: ImageOutputSettings,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct VideoTranscodeTask {
    pub input: PathBuf,
    pub input_type: VideoFormat,
    pub output: PathBuf,
    pub output_settings: VideoOutputSettings,
}
