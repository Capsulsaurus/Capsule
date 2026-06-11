use std::path::Path;

use capsule_media::fs::{ImageParseError, load_image};
use chrono::{DateTime, Utc};

/// Service for processing uploaded assets
#[derive(Clone)]
pub(crate) struct ProcessingService;

pub(crate) struct ExtractedMetadata {
    pub width: i32,
    pub height: i32,
    pub date: Option<DateTime<Utc>>,
}

impl ProcessingService {
    pub(crate) fn new() -> Self {
        Self
    }

    pub(crate) async fn extract_metadata(
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
