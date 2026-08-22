use std::path::Path;

use thiserror::Error;
use tokio::fs;

use crate::media::core::types::MediaType;
use crate::media::image::formats::avif::AvifImage;
use crate::media::image::formats::bmp::BmpImage;
use crate::media::image::formats::gif::GifImage;
use crate::media::image::formats::heif::HeifImage;
use crate::media::image::formats::jpeg::JpegImage;
use crate::media::image::formats::jxl::JxlImage;
use crate::media::image::formats::png::PngImage;
use crate::media::image::formats::raw::RawImage;
use crate::media::image::formats::tiff::TiffImage;
use crate::media::image::formats::webp::WebpImage as WebPImage;
use crate::media::image::types::ImageFormat;
use crate::media::image::{ImageFile, ImageReader, ImageWithMetadata};
use crate::media::video::VideoFile;
use crate::media::video::types::VideoFormat;

pub mod ext;

async fn is_path_file(path: &Path) -> Result<bool, std::io::Error> {
    let metadata = fs::metadata(path).await?;
    Ok(metadata.is_file())
}

/// Reads a media file from the given path and returns a MediaFile enum.
pub async fn read(file_path: &Path) -> Result<MediaFile, ReadMediaError> {
    // Verify it is a file
    if !is_path_file(file_path).await? {
        return Err(ReadMediaError::NotAFile);
    }

    let media_type: MediaType = ext::detect_media_type(file_path)
        .await?
        .ok_or(ReadMediaError::UnknownFormat)?;

    // Parse based on media type
    let mf = match media_type {
        MediaType::Image(t) => MediaFile::Image(read_image(file_path, t).await?),
        MediaType::Video(t) => MediaFile::Video(read_video(file_path, t).await?),
    };

    Ok(mf)
}

/// Reads an image file from the given path and returns an ImageFile enum.
async fn read_image(file_path: &Path, t: ImageFormat) -> Result<ImageFile, ReadMediaError> {
    let image: Box<dyn ImageWithMetadata> = match t {
        ImageFormat::Jpeg => Box::new(JpegImage::from_path(file_path).await?),
        ImageFormat::Jxl => Box::new(JxlImage::from_path(file_path).await?),
        ImageFormat::Heic => Box::new(HeifImage::from_path(file_path).await?),
        ImageFormat::Png => Box::new(PngImage::from_path(file_path).await?),
        ImageFormat::Tiff => Box::new(TiffImage::from_path(file_path).await?),
        ImageFormat::Avif => Box::new(AvifImage::from_path(file_path).await?),
        ImageFormat::WebP => Box::new(WebPImage::from_path(file_path).await?),
        ImageFormat::Gif => Box::new(GifImage::from_path(file_path).await?),
        ImageFormat::Bmp => Box::new(BmpImage::from_path(file_path).await?),
        ImageFormat::Raw(t) => Box::new(RawImage::from_path(file_path, t).await?),
    };

    Ok(ImageFile {
        source_path: file_path.to_path_buf(),
        image,
    })
}

/// Reads a video file from the given path and returns a VideoFile enum.
///
/// No video demuxer is linked into this build (slice `S-B13`), so every call returns
/// [`ReadMediaError::UnsupportedVideoFormat`] naming the detected container. The dispatch in
/// [`read`] is unchanged — `?` now propagates a typed error where it used to abort the process.
async fn read_video(_file_path: &Path, t: VideoFormat) -> Result<VideoFile, ReadMediaError> {
    Err(ReadMediaError::UnsupportedVideoFormat(t))
}

#[derive(Error, Debug)]
pub enum ReadMediaError {
    #[error("Path is not a file")]
    NotAFile,
    #[error("Unknown media format")]
    UnknownFormat,
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Join error: {0}")]
    JoinError(#[from] tokio::task::JoinError),
    #[error("Image error: {0}")]
    Image(#[from] crate::media::image::ImageError),
    /// The container was recognized but this build has no video decoder for it. The video
    /// counterpart of
    /// [`ImageError::UnsupportedFormat`](crate::media::image::ImageError::UnsupportedFormat).
    #[error("Video format {0:?} is not implemented in this build")]
    UnsupportedVideoFormat(VideoFormat),
}

#[derive(Debug)]
pub enum MediaFile {
    Image(ImageFile),
    Video(VideoFile),
}

/// Detects the image type from a path
///
/// Returns [ReadImageError] if the path is not an image file.
pub async fn detect_image_type(path: &Path) -> Result<ImageFormat, ReadImageError> {
    // Verify it is a file
    if !is_path_file(path).await? {
        return Err(ReadImageError::NotAFile);
    }

    let media_type: MediaType = ext::detect_media_type(path)
        .await?
        .ok_or(ReadImageError::UnknownFormat)?;

    match media_type {
        MediaType::Image(t) => Ok(t),
        MediaType::Video(_) => Err(ReadImageError::NotAnImage(media_type)),
    }
}

#[derive(Error, Debug)]
pub enum ReadImageError {
    #[error("Path is not a file")]
    NotAFile,
    #[error("Unknown media format")]
    UnknownFormat,
    #[error("Not an image")]
    NotAnImage(MediaType),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Error, Debug)]
pub enum ImageParseError {
    #[error("Read image error: {0}")]
    ReadImageError(#[from] ReadImageError),
    #[error("Image error: {0}")]
    ImageError(#[from] crate::media::image::ImageError),
    #[error("Image data error: {0}")]
    DataError(String),
}

impl From<String> for ImageParseError {
    fn from(s: String) -> Self {
        ImageParseError::DataError(s)
    }
}

/// Load an image into memory
pub async fn load_image(path: &Path) -> Result<Box<dyn ImageWithMetadata>, ImageParseError> {
    // Identify the image type
    let image_type = detect_image_type(path).await?;

    // Parse the image bytes
    let image: Box<dyn ImageWithMetadata> = match image_type {
        ImageFormat::Jpeg => Box::new(JpegImage::from_path(path).await?),
        ImageFormat::Jxl => Box::new(JxlImage::from_path(path).await?),
        ImageFormat::Heic => Box::new(HeifImage::from_path(path).await?),
        ImageFormat::Png => Box::new(PngImage::from_path(path).await?),
        ImageFormat::Tiff => Box::new(TiffImage::from_path(path).await?),
        ImageFormat::Avif => Box::new(AvifImage::from_path(path).await?),
        ImageFormat::WebP => Box::new(WebPImage::from_path(path).await?),
        ImageFormat::Gif => Box::new(GifImage::from_path(path).await?),
        ImageFormat::Bmp => Box::new(BmpImage::from_path(path).await?),
        ImageFormat::Raw(t) => Box::new(RawImage::from_path(path, t).await?),
    };

    Ok(image)
}

// ── Regression gate: undecodable formats error, never panic (slice `S-B13`) ──────────────────

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;
    use crate::media::image::types::{ALL_IMAGE_FORMATS, RawImageFormat, SUPPORTED_IMAGE_FORMATS};
    use crate::media::image::{FormatOp, ImageDecode, ImageError};

    /// Compile-time exhaustiveness guard. Adding a variant to [`ImageFormat`] breaks this match,
    /// which forces whoever added it to also extend [`ALL_IMAGE_FORMATS`], the `decode_as`
    /// dispatch below, and [`ImageFormat::is_decodable`] — the three tables the gate compares.
    #[expect(dead_code, reason = "compile-time exhaustiveness guard, never called")]
    fn assert_image_format_table_is_exhaustive(format: ImageFormat) {
        match format {
            ImageFormat::Jpeg
            | ImageFormat::Jxl
            | ImageFormat::Heic
            | ImageFormat::Png
            | ImageFormat::Tiff
            | ImageFormat::Avif
            | ImageFormat::WebP
            | ImageFormat::Gif
            | ImageFormat::Bmp => {}
            ImageFormat::Raw(kind) => match kind {
                RawImageFormat::Dng
                | RawImageFormat::Arw
                | RawImageFormat::Cr2
                | RawImageFormat::Cr3
                | RawImageFormat::Nef
                | RawImageFormat::Raf => {}
            },
        }
    }

    /// Byte-for-byte the same arms as [`read_image`] / [`load_image`], but through
    /// [`ImageDecode::decode_from_bytes`] so the in-memory entry point is covered too.
    fn decode_as(
        format: ImageFormat,
        bytes: &[u8],
    ) -> Result<Box<dyn ImageWithMetadata>, ImageError> {
        Ok(match format {
            ImageFormat::Jpeg => Box::new(JpegImage::decode_from_bytes(bytes)?),
            ImageFormat::Jxl => Box::new(JxlImage::decode_from_bytes(bytes)?),
            ImageFormat::Heic => Box::new(HeifImage::decode_from_bytes(bytes)?),
            ImageFormat::Png => Box::new(PngImage::decode_from_bytes(bytes)?),
            ImageFormat::Tiff => Box::new(TiffImage::decode_from_bytes(bytes)?),
            ImageFormat::Avif => Box::new(AvifImage::decode_from_bytes(bytes)?),
            ImageFormat::WebP => Box::new(WebPImage::decode_from_bytes(bytes)?),
            ImageFormat::Gif => Box::new(GifImage::decode_from_bytes(bytes)?),
            ImageFormat::Bmp => Box::new(BmpImage::decode_from_bytes(bytes)?),
            ImageFormat::Raw(_) => Box::new(RawImage::decode_from_bytes(bytes)?),
        })
    }

    /// Junk bytes: no format can decode them, so the *only* difference between a supported and
    /// an unsupported format is **which** error comes back.
    const JUNK: &[u8] = b"not a real image, just some bytes";

    /// **(a)** Every format outside [`SUPPORTED_IMAGE_FORMATS`] returns
    /// [`ImageError::UnsupportedFormat`] from `decode_from_bytes` — a value, not a panic. The
    /// supported ones fail too (the bytes are junk) but must *not* claim to be unsupported,
    /// which is what keeps this from passing vacuously once a codec lands.
    #[test]
    fn undecodable_formats_return_unsupported_format_instead_of_panicking() {
        for &format in ALL_IMAGE_FORMATS {
            let err = decode_as(format, JUNK).expect_err("junk bytes never decode into an image");

            if format.is_decodable() {
                assert!(
                    !matches!(err, ImageError::UnsupportedFormat { .. }),
                    "{format:?} is listed as decodable but reports UnsupportedFormat: {err}"
                );
                continue;
            }

            let ImageError::UnsupportedFormat {
                format: reported,
                op,
            } = err
            else {
                panic!("{format:?} has no codec but returned {err} instead of UnsupportedFormat");
            };
            assert_eq!(op, FormatOp::Decode, "{format:?} reported the wrong op");
            // RAW shares one uninhabited type across every flavour, so `decode_from_bytes`
            // cannot know which flavour was asked for; the path-based constructor can, and is
            // covered by the dispatch test below.
            if !matches!(format, ImageFormat::Raw(_)) {
                assert_eq!(reported, format, "UnsupportedFormat named the wrong format");
            }
        }
    }

    /// **(b)** [`ImageFormat::is_decodable`] agrees with [`SUPPORTED_IMAGE_FORMATS`] *and* with
    /// what the real [`read_image`] dispatch actually does. Adding a codec without updating
    /// both tables fails here.
    #[tokio::test]
    async fn is_decodable_agrees_with_the_fs_dispatch_table() {
        // The predicate and the table are two spellings of the same set.
        for &format in ALL_IMAGE_FORMATS {
            assert_eq!(
                format.is_decodable(),
                SUPPORTED_IMAGE_FORMATS.contains(&format),
                "is_decodable and SUPPORTED_IMAGE_FORMATS disagree about {format:?}"
            );
        }

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("junk.bin");
        let mut file = std::fs::File::create(&path).expect("create");
        file.write_all(JUNK).expect("write");
        drop(file);

        for &format in ALL_IMAGE_FORMATS {
            let err = read_image(&path, format)
                .await
                .expect_err("junk bytes never read as an image");
            let is_unsupported = matches!(
                err,
                ReadMediaError::Image(ImageError::UnsupportedFormat { .. })
            );
            assert_eq!(
                is_unsupported,
                !format.is_decodable(),
                "media::fs dispatch for {format:?} disagrees with is_decodable(): {err}"
            );
        }
    }

    /// The video read path is under the same contract: a typed error, not a panicking stub.
    #[tokio::test]
    async fn video_read_reports_unsupported_video_format() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("junk.mp4");
        std::fs::write(&path, JUNK).expect("write");

        let err = read_video(&path, VideoFormat::Mp4)
            .await
            .expect_err("no video decoder is linked into this build");
        assert!(matches!(
            err,
            ReadMediaError::UnsupportedVideoFormat(VideoFormat::Mp4)
        ));
    }
}
