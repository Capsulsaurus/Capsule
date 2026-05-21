use std::path::Path;

use thiserror::Error;
use tokio::fs;

use crate::{
    core::types::MediaType,
    image::{
        ImageFile, ImageReader, ImageWithMetadata,
        formats::{
            avif::AvifImage, bmp::BmpImage, gif::GifImage, heif::HeifImage, jpeg::JpegImage,
            jxl::JxlImage, png::PngImage, raw::RawImage, tiff::TiffImage,
            webp::WebpImage as WebPImage,
        },
        types::ImageFormat,
    },
    video::{VideoFile, types::VideoFormat},
};

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
async fn read_video(_file_path: &Path, _t: VideoFormat) -> Result<VideoFile, ReadMediaError> {
    unimplemented!()
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
    Image(#[from] crate::image::ImageError),
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
        _ => Err(ReadImageError::NotAnImage(media_type)),
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
    ImageError(#[from] crate::image::ImageError),
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
