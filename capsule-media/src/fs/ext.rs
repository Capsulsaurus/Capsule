use std::path::Path;

use file_format::FileFormat;

use crate::core::types::MediaType;
use crate::image::types::ImageFormat;
use crate::video::types::VideoFormat;

macro_rules! img {
    ($variant:ident) => {
        Some(MediaType::Image(ImageFormat::$variant))
    };
}

macro_rules! vid {
    ($variant:ident) => {
        Some(MediaType::Video(VideoFormat::$variant))
    };
}

/// Returns the MediaType corresponding to the given file extension.
pub async fn detect_media_type(file_path: &Path) -> Result<Option<MediaType>, std::io::Error> {
    let path = file_path.to_path_buf();
    let fmt = tokio::task::spawn_blocking(move || FileFormat::from_file(path))
        .await
        .map_err(std::io::Error::other)??;

    let media_type = match fmt {
        // Image formats
        FileFormat::JointPhotographicExpertsGroup => img!(Jpeg),
        FileFormat::JpegXl => img!(Jxl),
        FileFormat::HighEfficiencyImageCoding | FileFormat::HighEfficiencyImageFileFormat => {
            img!(Heic)
        }
        FileFormat::PortableNetworkGraphics => img!(Png),
        FileFormat::TagImageFileFormat => img!(Tiff),
        FileFormat::Av1ImageFileFormat => img!(Avif),

        // Video formats
        FileFormat::Mpeg4Part14Video => vid!(Mp4),
        FileFormat::Webm => vid!(Webm),
        FileFormat::AppleQuicktime => vid!(Mov),
        FileFormat::AudioVideoInterleave => vid!(Avi),
        FileFormat::MatroskaVideo => vid!(Mkv),

        _ => None,
    };

    Ok(media_type)
}
