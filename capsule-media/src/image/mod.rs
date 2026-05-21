use std::{
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

use thiserror::Error;

use crate::image::{
    buffer::ImageBuffer,
    metadata::{ImageMetadata, ImageMetadataProvider},
    types::ImageFormat,
};

pub mod buffer;
pub mod formats;
pub mod lqip;
pub mod metadata;
pub mod presets;
pub mod types;

#[derive(Debug)]
pub struct ImageFile {
    pub source_path: PathBuf,
    pub image: Box<dyn ImageWithMetadata>,
}

pub trait Image: std::fmt::Debug + Send + Sync {
    /// Returns image format
    fn get_format(&self) -> ImageFormat;

    /// Returns the raw pixel buffer of the image.
    ///
    /// This method allows access to the underlying pixel buffer. The format and characteristics
    /// of the buffer can be inspected via the `ImageBuffer` fields.
    fn get_buffer(&self) -> ImageBuffer;

    /// Creates a new image from a raw pixel buffer and metadata.
    ///
    /// This allows initializing an image directly from its components (pixel data and metadata),
    /// useful for format conversion or generating images programmatically.
    fn from_raw_parts(buffer: ImageBuffer, metadata: ImageMetadata) -> Result<Self, ImageError>
    where
        Self: Sized;
}

pub trait ImageWithMetadata: Image + ImageMetadataProvider {}
impl<T: Image + ImageMetadataProvider> ImageWithMetadata for T {}

/// Trait for converting between different Image types.
///
/// This trait provides methods to convert from one Image type to another by extracting
/// the buffer and metadata from the source image and reconstructing the target image.
///
/// All types implementing `Image` automatically get this trait.
pub trait ConvertImage: ImageWithMetadata {
    /// Convert this image into another image type.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let jpeg_image: JpegImage = /* ... */;
    /// let png_image: PngImage = jpeg_image.convert_to()?;
    /// ```
    fn convert_to<U: Image>(self) -> Result<U, ImageError>
    where
        Self: Sized,
    {
        let buffer = self.get_buffer();
        let metadata = self.get_metadata();
        U::from_raw_parts(buffer, metadata)
    }

    /// Convert this image (by reference) into another image type.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let jpeg_image: &JpegImage = /* ... */;
    /// let png_image: PngImage = jpeg_image.convert_to_ref()?;
    /// ```
    fn convert_to_ref<U: Image + ImageMetadataProvider>(&self) -> Result<U, ImageError> {
        let buffer = self.get_buffer();
        let metadata = self.get_metadata();
        U::from_raw_parts(buffer, metadata)
    }

    /// Create this image type from another image type.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let jpeg_image: JpegImage = /* ... */;
    /// let png_image = PngImage::convert_from(jpeg_image)?;
    /// ```
    fn convert_from<T: Image + ImageMetadataProvider>(source: T) -> Result<Self, ImageError>
    where
        Self: Sized,
    {
        let buffer = source.get_buffer();
        let metadata = source.get_metadata();
        Self::from_raw_parts(buffer, metadata)
    }

    /// Create this image type from a reference to another image type.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let jpeg_image: &JpegImage = /* ... */;
    /// let png_image = PngImage::convert_from_ref(&jpeg_image)?;
    /// ```
    fn convert_from_ref<T: Image + ImageMetadataProvider>(source: &T) -> Result<Self, ImageError>
    where
        Self: Sized,
    {
        let buffer = source.get_buffer();
        let metadata = source.get_metadata();
        Self::from_raw_parts(buffer, metadata)
    }

    /// Create this image type from a boxed trait object.
    ///
    /// This is useful when working with `Box<dyn Image>` from dynamic dispatch scenarios.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let image: Box<dyn Image> = /* ... */;
    /// let png_image = PngImage::convert_from_boxed(image)?;
    /// ```
    fn convert_from_boxed(source: Box<dyn ImageWithMetadata>) -> Result<Self, ImageError>
    where
        Self: Sized,
    {
        let buffer = source.get_buffer();
        let metadata = source.get_metadata();
        Self::from_raw_parts(buffer, metadata)
    }
}

/// Blanket implementation of ConvertImage for all types implementing Image.
impl<T: ImageWithMetadata> ConvertImage for T {}

#[derive(Error, Debug)]
pub enum ImageError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Decoding error: {0}")]
    Decode(String),
    #[error("Encoding error: {0}")]
    Encode(String),
    #[error("Image buffer error: {0}")]
    ImageBuffer(#[from] crate::image::buffer::ImageBufferError),
}

pub trait ImageDecode: Sized + Image + 'static {
    /// Decodes an image directly from a byte slice.
    ///
    /// # Arguments
    ///
    /// * `data` - A byte slice containing the encoded image data.
    ///
    /// # Returns
    ///
    /// * `Result<Self, ImageError>` - The decoded image instance or an error.
    fn decode_from_bytes(data: &[u8]) -> Result<Self, ImageError>;
}

pub trait ImageReader: ImageDecode {
    /// Decodes an image from a buffered reader.
    ///
    /// This is the core decoding method. It accepts any type implementing `BufRead` (e.g., `BufReader<File>`, `Cursor<Vec<u8>>`),
    /// allowing for flexible input sources.
    ///
    /// # Arguments
    ///
    /// * `reader` - A buffered reader providing the image data.
    ///
    /// # Returns
    ///
    /// * `Result<Self, ImageError>` - The decoded image instance or an error.
    fn decode<R: BufRead>(mut reader: R) -> Result<Self, ImageError> {
        // We need to decode the whole blob so we first read it all into memory.
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).map_err(ImageError::Io)?;
        Self::decode_from_bytes(&buf)
    }

    /// Asynchronously decodes an image from a file path.
    ///
    /// This method reads the file at the specified path asynchronously and then decodes it.
    ///
    /// # Arguments
    ///
    /// * `path` - The path to the image file.
    ///
    /// # Returns
    ///
    /// * `impl Future<Output = Result<Self, ImageError>>` - A future resolving to the decoded image or an error.
    fn from_path(path: impl AsRef<Path>) -> impl Future<Output = Result<Self, ImageError>> + Send {
        let path = path.as_ref().to_owned();
        async move {
            // Use spawn_blocking to protect the executor from disk latency
            tokio::task::spawn_blocking(move || {
                let file = std::fs::File::open(&path).map_err(ImageError::Io)?;
                let meta = file.metadata().map_err(ImageError::Io)?;

                // Heuristic: Mmap large files, read small ones
                if meta.len() > 16 * 1024 {
                    // File is larger than 16KB
                    // TODO: Validate this heuristic is optimal
                    let mmap = unsafe { memmap2::Mmap::map(&file).map_err(ImageError::Io)? };
                    Self::decode_from_bytes(&mmap)
                } else {
                    let mut buf = Vec::with_capacity(meta.len() as usize);
                    std::io::Read::read_to_end(&mut &file, &mut buf).map_err(ImageError::Io)?;
                    Self::decode_from_bytes(&buf)
                }
            })
            .await
            .map_err(|e| ImageError::Decode(format!("JoinError: {e:?}")))?
        }
    }
}

impl<T: ImageDecode> ImageReader for T {}

pub trait ImageEncode: Image + Sync {
    /// Encodes the image to a writer.
    ///
    /// This is the core encoding method. It writes the encoded image data to the provided writer.
    ///
    /// # Arguments
    ///
    /// * `writer` - A writable target (e.g., `File`, `Vec<u8>`, `TcpStream`).
    ///
    /// # Returns
    ///
    /// * `Result<(), ImageError>` - `Ok(())` on success, or an error on failure.
    fn encode<W: Write>(&self, writer: &mut W) -> Result<(), ImageError>;

    /// Encodes the image to a vector of bytes.
    ///
    /// A convenience wrapper around `encode` that returns the encoded data as a `Vec<u8>`.
    ///
    /// # Returns
    ///
    /// * `Result<Vec<u8>, ImageError>` - The encoded image bytes or an error.
    fn encode_to_bytes(&self) -> Result<Vec<u8>, ImageError> {
        let mut buffer = Vec::new();
        self.encode(&mut buffer)?;
        Ok(buffer)
    }

    /// Asynchronously saves the encoded image to a file path.
    ///
    /// # Arguments
    ///
    /// * `path` - The destination path for the image file.
    ///
    /// # Returns
    ///
    /// * `impl Future<Output = Result<(), ImageError>>` - A future resolving to `Ok(())` or an error.
    fn save(&self, path: &Path) -> impl Future<Output = Result<(), ImageError>> + Send;
}

/// Returns dimensions that maintain aspect ratio while ensuring the largest dimension is at most target_max
pub fn resize_to_max_dimension(w: usize, h: usize, target_max: usize) -> (usize, usize) {
    // Determine the scale factor based on the larger dimension
    let larger_dimension = if h > w { h } else { w };
    if larger_dimension <= target_max {
        return (w, h);
    }
    let scale = target_max as f64 / (larger_dimension as f64);

    // Apply scale and round to maintain aspect ratio integrity
    let w_resized = (w as f64 * scale).round() as usize;
    let h_resized = (h as f64 * scale).round() as usize;

    (w_resized, h_resized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resize_to_max_dimension() {
        // Landscape
        let (w, h) = resize_to_max_dimension(200, 100, 100);
        assert_eq!(w, 100);
        assert_eq!(h, 50);

        // Portrait
        let (w, h) = resize_to_max_dimension(100, 200, 100);
        assert_eq!(w, 50);
        assert_eq!(h, 100);

        // Square
        let (w, h) = resize_to_max_dimension(150, 150, 100);
        assert_eq!(w, 100);
        assert_eq!(h, 100);

        // Already smaller
        let (w, h) = resize_to_max_dimension(50, 50, 100);
        assert_eq!(w, 50);
        assert_eq!(h, 50);
    }

    #[test]
    #[ignore = "PngImage not yet implemented"]
    fn test_image_conversion() {
        use crate::image::formats::{jpeg::JpegImage, png::PngImage};

        // Create a sample JPEG image buffer
        let width = 100;
        let height = 100;
        let data = vec![255u8; width * height * 3]; // RGB data

        let buffer = ImageBuffer::new(
            data,
            width,
            height,
            buffer::PixelFormat::Rgb,
            buffer::ComponentType::U8,
            crate::metadata::ColorSpace::Srgb,
        )
        .unwrap();

        let metadata = ImageMetadata {
            format: Some(ImageFormat::Jpeg),
            file_size_bytes: 0,
            width: width as u32,
            height: height as u32,
            bit_depth: 8,
            color_space: crate::metadata::ColorSpace::Srgb,
            ..Default::default()
        };

        // Create JPEG image from raw parts
        let jpeg_image = JpegImage::from_raw_parts(buffer, metadata).unwrap();

        // Test convert_to method (consuming)
        let png_image: Result<PngImage, _> = jpeg_image.clone().convert_to();
        assert!(png_image.is_ok());

        // Test convert_to_ref method (borrowing)
        let png_image: Result<PngImage, _> = jpeg_image.convert_to_ref();
        assert!(png_image.is_ok());

        // Test convert_from method
        let jpeg_image2 = JpegImage::from_raw_parts(
            ImageBuffer::new(
                vec![255u8; width * height * 3],
                width,
                height,
                buffer::PixelFormat::Rgb,
                buffer::ComponentType::U8,
                crate::metadata::ColorSpace::Srgb,
            )
            .unwrap(),
            ImageMetadata {
                format: Some(ImageFormat::Jpeg),
                file_size_bytes: 0,
                width: width as u32,
                height: height as u32,
                bit_depth: 8,
                color_space: crate::metadata::ColorSpace::Srgb,
                ..Default::default()
            },
        )
        .unwrap();

        let png_image: Result<PngImage, _> = PngImage::convert_from(jpeg_image2);
        assert!(png_image.is_ok());
    }
}
