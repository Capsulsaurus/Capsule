use chrono::{DateTime, Utc};
use capsule_media::fs::{ImageParseError, load_image};
use std::path::Path;

/// Service for processing uploaded assets
#[derive(Clone)]
pub struct ProcessingService;

pub struct ExtractedMetadata {
    pub width: i32,
    pub height: i32,
    pub date: Option<DateTime<Utc>>,
}

impl ProcessingService {
    pub fn new() -> Self {
        Self
    }

    pub async fn extract_metadata(
        &self,
        path: &Path,
    ) -> Result<ExtractedMetadata, ImageParseError> {
        let image = load_image(path).await?;
        let metadata = image.get_metadata();
        let date = metadata.date_taken;

        Ok(ExtractedMetadata {
            width: metadata.width as i32,
            height: metadata.height as i32,
            date,
        })
    }
}
