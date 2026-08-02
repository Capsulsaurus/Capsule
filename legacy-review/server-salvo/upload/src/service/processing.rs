use std::path::Path;

use capsule_core::media::fs::{ImageParseError, load_image};
use jiff::Timestamp;

/// Service for processing uploaded assets
#[derive(Clone)]
pub(crate) struct ProcessingService;

pub(crate) struct ExtractedMetadata {
    pub width: i32,
    pub height: i32,
    pub date: Option<Timestamp>,
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
        // EXIF capture time carries no offset; interpret the civil datetime as UTC, matching
        // the prior behavior of this extraction path.
        let date = metadata
            .date_taken
            .and_then(|dt| dt.to_zoned(jiff::tz::TimeZone::UTC).ok())
            .map(|z| z.timestamp());

        Ok(ExtractedMetadata {
            width: metadata.width as i32,
            height: metadata.height as i32,
            date,
        })
    }
}
