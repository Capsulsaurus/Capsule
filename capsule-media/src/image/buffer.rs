use crate::metadata::ColorSpace;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    Gray,
    Rgb,
    Rgba,
    Cmyk,
    // Add others as needed
}

impl PixelFormat {
    /// Returns the number of components per pixel for this format.
    pub fn num_components(&self) -> usize {
        match self {
            PixelFormat::Gray => 1,
            PixelFormat::Rgb => 3,
            PixelFormat::Rgba => 4,
            PixelFormat::Cmyk => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentType {
    U8,
    U16,
    F32,
}

impl ComponentType {
    /// Returns the number of bytes per component for this type.
    pub fn bytes_per_component(&self) -> usize {
        match self {
            ComponentType::U8 => 1,
            ComponentType::U16 => 2,
            ComponentType::F32 => 4,
        }
    }
}

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImageBufferError {
    #[error("Invalid data size: expected {expected}, got {actual}")]
    InvalidDataSize { expected: usize, actual: usize },
    #[error("Resize currently only supported for U8 data (got {0:?})")]
    ResizeUnsupported(ComponentType),
    #[error("Target dimensions must be non-zero")]
    InvalidDimensions,
    #[error("Operation not supported for component type {0:?}")]
    UnsupportedOperation(ComponentType),
    #[error("Conversion from {0:?} to RGBA8 not implemented")]
    FormatConversionNotImplemented(PixelFormat),
}

#[derive(Debug, Clone)]
pub struct ImageBuffer {
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub format: PixelFormat,
    pub component_type: ComponentType,
    pub color_space: ColorSpace,
    // Stride could be added later if needed, assuming packed for now
}

impl AsRef<ImageBuffer> for ImageBuffer {
    fn as_ref(&self) -> &ImageBuffer {
        self
    }
}

impl ImageBuffer {
    pub fn new(
        data: Vec<u8>,
        width: usize,
        height: usize,
        format: PixelFormat,
        component_type: ComponentType,
        color_space: ColorSpace,
    ) -> Result<Self, ImageBufferError> {
        let expected_len =
            width * height * format.num_components() * component_type.bytes_per_component();
        if data.len() != expected_len {
            return Err(ImageBufferError::InvalidDataSize {
                expected: expected_len,
                actual: data.len(),
            });
        }

        Ok(Self {
            data,
            width,
            height,
            format,
            component_type,
            color_space,
        })
    }

    /// Returns total bytes per pixel
    pub fn pixel_stride(&self) -> usize {
        self.format.num_components() * self.component_type.bytes_per_component()
    }

    /// Resize the image using Nearest Neighbor algorithm.
    /// Currently only supports U8 component type.
    pub fn resize(
        &self,
        new_width: usize,
        new_height: usize,
    ) -> Result<ImageBuffer, ImageBufferError> {
        // TODO: Reimplement real implementation. Perhaps use zune-procs for this.
        // For upscaling, give option for Bicubic and Bilinear
        // For downscaling, give option for Lanczos3 and Mitchell-Netravali
        // implementation could use image or zune-image

        if self.component_type != ComponentType::U8 {
            return Err(ImageBufferError::ResizeUnsupported(self.component_type));
        }
        if new_width == 0 || new_height == 0 {
            return Err(ImageBufferError::InvalidDimensions);
        }

        let stride = self.pixel_stride();
        let mut new_data = Vec::with_capacity(new_width * new_height * stride);

        // Nearest neighbor
        // x_ratio = original_width / new_width
        // y_ratio = original_height / new_height
        // src_x = (dest_x * x_ratio).floor() ... simple scaling

        // Use fixed point or float logic. Float is fine.
        let x_scale = self.width as f32 / new_width as f32;
        let y_scale = self.height as f32 / new_height as f32;

        for y in 0..new_height {
            let src_y = ((y as f32) * y_scale).floor() as usize;
            let src_y = src_y.min(self.height - 1); // Clamp
            let y_offset = src_y * self.width * stride;

            for x in 0..new_width {
                let src_x = ((x as f32) * x_scale).floor() as usize;
                let src_x = src_x.min(self.width - 1); // Clamp

                let pixel_offset = y_offset + (src_x * stride);
                let pixel = &self.data[pixel_offset..pixel_offset + stride];
                new_data.extend_from_slice(pixel);
            }
        }

        ImageBuffer::new(
            new_data,
            new_width,
            new_height,
            self.format,
            self.component_type,
            self.color_space,
        )
    }

    /// Consumes the buffer and returns an RGBA8 buffer.
    /// If the buffer is already RGBA8, it is returned directly.
    /// Otherwise, a new buffer is created.
    pub fn into_rgba8(self) -> Result<ImageBuffer, ImageBufferError> {
        if self.component_type != ComponentType::U8 {
            return Err(ImageBufferError::UnsupportedOperation(self.component_type));
        }

        if self.format == PixelFormat::Rgba {
            return Ok(self);
        }

        self.to_rgba8()
    }

    /// Creates a new RGBA8 buffer from this buffer.
    /// If this buffer is already RGBA8, it returns a clone.
    pub fn to_rgba8(&self) -> Result<ImageBuffer, ImageBufferError> {
        if self.component_type != ComponentType::U8 {
            return Err(ImageBufferError::UnsupportedOperation(self.component_type));
        }

        match self.format {
            PixelFormat::Rgba => Ok(self.clone()),
            PixelFormat::Rgb => {
                let num_pixels = self.width * self.height;
                let mut new_data = Vec::with_capacity(num_pixels * 4);
                for chunk in self.data.chunks_exact(3) {
                    new_data.extend_from_slice(chunk);
                    new_data.push(255); // Alpha
                }
                ImageBuffer::new(
                    new_data,
                    self.width,
                    self.height,
                    PixelFormat::Rgba,
                    ComponentType::U8,
                    self.color_space,
                )
            }
            PixelFormat::Gray => {
                let num_pixels = self.width * self.height;
                let mut new_data = Vec::with_capacity(num_pixels * 4);
                for &v in &self.data {
                    new_data.extend_from_slice(&[v, v, v, 255]);
                }
                ImageBuffer::new(
                    new_data,
                    self.width,
                    self.height,
                    PixelFormat::Rgba,
                    ComponentType::U8,
                    self.color_space,
                )
            }
            _ => Err(ImageBufferError::FormatConversionNotImplemented(
                self.format,
            )),
        }
    }
}
