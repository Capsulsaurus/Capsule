use thiserror::Error;

use crate::media::image::buffer::{ComponentType, ImageBuffer, ImageBufferError, PixelFormat};
use crate::media::image::resize_to_max_dimension;
use crate::media::metadata::ColorSpace;
use crate::sidecar::sidecar_v1::Lqip as SidecarLqip;

/// The current LQIP chromahash format version written into the sidecar (SSoT: Thumbnails —
/// LQIP). A decoder that does not recognize this version falls back to the solid
/// `dominant_color` fill (see [`render_sidecar_lqip`]) rather than misrendering, so a future
/// chromahash revision is a versioned change, never a silent break.
pub const LQIP_FORMAT_V1: u16 = 1;

/// LQIP (thumbhash) struct
pub struct LQIP(Vec<u8>);

impl LQIP {
    pub fn from_bytes(bytes: Vec<u8>) -> LQIP {
        LQIP(bytes)
    }

    /// Generate a LQIP (thumbhash) from an ImageBuffer
    ///
    /// The buffer MUST be RGBA.
    /// You do not need to resize as it will be done to input internally.
    /// Returns a byte sequence
    pub async fn from_image_buffer<T>(buffer: T) -> Result<LQIP, LQIPError>
    where
        T: AsRef<ImageBuffer>,
    {
        Self::from_rgba_buffer(buffer.as_ref())
    }

    /// Generate an LQIP (thumbhash) from an RGBA8 [`ImageBuffer`] synchronously.
    ///
    /// The thumbhash computation is pure CPU work with no I/O; this is the blocking sibling of
    /// [`from_image_buffer`](Self::from_image_buffer) for callers already off the async path
    /// (e.g. the signed import executor). The buffer MUST be RGBA8; it is downsized internally.
    pub fn from_rgba_buffer(buffer: &ImageBuffer) -> Result<LQIP, LQIPError> {
        // Downsize so the longest edge is at most MAX_SIZE px before hashing.
        const MAX_SIZE: usize = 100;

        if buffer.format != PixelFormat::Rgba || buffer.component_type != ComponentType::U8 {
            return Err(LQIPError::UnsupportedFormat);
        }

        let resized_buffer;
        let work_buffer = if buffer.width > MAX_SIZE || buffer.height > MAX_SIZE {
            let (new_width, new_height) =
                resize_to_max_dimension(buffer.width, buffer.height, MAX_SIZE);
            resized_buffer = buffer.resize(new_width, new_height)?;
            &resized_buffer
        } else {
            buffer
        };

        let bytes =
            thumbhash::rgba_to_thumb_hash(work_buffer.width, work_buffer.height, &work_buffer.data);
        Ok(LQIP(bytes))
    }

    /// Extracts the approximate aspect ratio of the original image
    pub fn approx_aspect_ratio(&self) -> Result<f32, LQIPError> {
        thumbhash::thumb_hash_to_approximate_aspect_ratio(&self.0)
            .map_err(|()| LQIPError::InvalidHash)
    }

    /// Extracts the average color (r,g,b,a) from a ThumbHash
    pub fn average_rgba(&self) -> Result<[f32; 4], LQIPError> {
        let (r, g, b, a) =
            thumbhash::thumb_hash_to_average_rgba(&self.0).map_err(|()| LQIPError::InvalidHash)?;
        Ok([r, g, b, a])
    }

    /// Decodes a ThumbHash to an RGBA image buffer.
    pub fn thumb_hash_to_rgba(&self) -> Result<ImageBuffer, LQIPError> {
        let (width, height, rgba) =
            thumbhash::thumb_hash_to_rgba(&self.0).map_err(|()| LQIPError::InvalidHash)?;

        ImageBuffer::new(
            rgba,
            width,
            height,
            PixelFormat::Rgba,
            ComponentType::U8,
            ColorSpace::Srgb,
        )
        .map_err(|_| LQIPError::UnhandledState)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// Build the encrypted-sidecar [`Lqip`](SidecarLqip) record from this hash: the chromahash
    /// bytes, the current [`LQIP_FORMAT_V1`] version tag, and the `dominant_color` fallback
    /// derived from the hash's average color (the solid fill an older/newer decoder uses when
    /// it cannot decode this chromahash version).
    pub fn to_sidecar(&self) -> Result<SidecarLqip, LQIPError> {
        let [r, g, b, _a] = self.average_rgba()?;
        Ok(SidecarLqip {
            chromahash: self.0.clone(),
            format_version: LQIP_FORMAT_V1,
            dominant_color: [to_u8(r), to_u8(g), to_u8(b)],
        })
    }
}

/// Clamp a 0.0..=1.0 channel to an 8-bit value.
fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// Render a sidecar [`Lqip`](SidecarLqip) to a displayable RGBA placeholder.
///
/// If the record's `format_version` is recognized *and* its chromahash decodes, the decoded
/// placeholder tier is returned. Otherwise — an unknown future version, or corrupt bytes — the
/// caller gets a 1×1 solid `dominant_color` fill instead of a misrender, exactly the versioned
/// fallback the [contract](https://docs/design/thumbnails/#lqip) specifies.
pub fn render_sidecar_lqip(lqip: &SidecarLqip) -> ImageBuffer {
    if lqip.format_version == LQIP_FORMAT_V1
        && let Ok(buf) = LQIP::from_bytes(lqip.chromahash.clone()).thumb_hash_to_rgba()
    {
        return buf;
    }
    dominant_color_fill(lqip.dominant_color)
}

/// A 1×1 opaque RGBA buffer of the given color — the solid fallback fill.
fn dominant_color_fill([r, g, b]: [u8; 3]) -> ImageBuffer {
    ImageBuffer::new(
        vec![r, g, b, 255],
        1,
        1,
        PixelFormat::Rgba,
        ComponentType::U8,
        ColorSpace::Srgb,
    )
    .expect("1x1 RGBA buffer is always valid")
}

#[derive(Error, Debug)]
pub enum LQIPError {
    #[error("Invalid hash")]
    InvalidHash,
    #[error("Unhandled state")]
    UnhandledState,
    #[error("LQIP currently requires U8 RGBA buffers")]
    UnsupportedFormat,
    #[error("Resize error: {0}")]
    ResizeError(#[from] ImageBufferError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_solid_rgba(width: usize, height: usize, r: u8, g: u8, b: u8, a: u8) -> ImageBuffer {
        let mut data = Vec::with_capacity(width * height * 4);
        for _ in 0..(width * height) {
            data.extend_from_slice(&[r, g, b, a]);
        }
        ImageBuffer::new(
            data,
            width,
            height,
            PixelFormat::Rgba,
            ComponentType::U8,
            ColorSpace::Srgb,
        )
        .unwrap()
    }

    #[tokio::test]
    async fn test_approx_aspect_ratio_square() {
        let rgba = create_solid_rgba(50, 50, 255, 0, 0, 255);
        let lqip = LQIP::from_image_buffer(&rgba).await.unwrap();
        let ar = lqip.approx_aspect_ratio().unwrap();
        // Square image should have aspect ratio close to 1.0
        assert!(
            (ar - 1.0).abs() < 0.2,
            "Aspect ratio {} was not close to 1.0",
            ar
        );
    }

    #[tokio::test]
    async fn test_approx_aspect_ratio_landscape() {
        let rgba = create_solid_rgba(100, 50, 0, 255, 0, 255);
        let lqip = LQIP::from_image_buffer(&rgba).await.unwrap();
        let ar = lqip.approx_aspect_ratio().unwrap();
        // 2:1 landscape image
        assert!(
            (ar - 2.0).abs() < 0.4,
            "Aspect ratio {} was not close to 2.0",
            ar
        );
    }

    #[tokio::test]
    async fn test_approx_aspect_ratio_portrait() {
        let rgba = create_solid_rgba(50, 100, 0, 0, 255, 255);
        let lqip = LQIP::from_image_buffer(&rgba).await.unwrap();
        let ar = lqip.approx_aspect_ratio().unwrap();
        // 1:2 portrait image
        assert!(
            (ar - 0.5).abs() < 0.2,
            "Aspect ratio {} was not close to 0.5",
            ar
        );
    }

    #[tokio::test]
    async fn test_average_rgba_red() {
        let rgba = create_solid_rgba(80, 80, 255, 0, 0, 255);
        let lqip = LQIP::from_image_buffer(&rgba).await.unwrap();
        let avg = lqip.average_rgba().unwrap();
        // Red component should be high, others low
        assert!(avg[0] > 0.8, "Red component {} too low", avg[0]);
        assert!(avg[1] < 0.2, "Green component {} too high", avg[1]);
        assert!(avg[2] < 0.2, "Blue component {} too high", avg[2]);
        assert!(avg[3] > 0.9, "Alpha component {} too low", avg[3]);
    }

    #[tokio::test]
    async fn test_average_rgba_semi_transparent() {
        let rgba = create_solid_rgba(80, 80, 0, 255, 0, 128);
        let lqip = LQIP::from_image_buffer(&rgba).await.unwrap();
        let avg = lqip.average_rgba().unwrap();
        // Alpha should be around 0.5
        assert!(
            (avg[3] - 0.5).abs() < 0.1,
            "Alpha component {} not close to 0.5",
            avg[3]
        );
    }

    #[tokio::test]
    async fn test_round_trip_reconstruction() {
        let rgba = create_solid_rgba(64, 64, 100, 150, 200, 255);
        let lqip = LQIP::from_image_buffer(&rgba).await.unwrap();
        let decoded = lqip
            .thumb_hash_to_rgba()
            .expect("Should decode back to RGBA");
        assert_eq!(decoded.format, PixelFormat::Rgba);

        // Detailed equality is hard with ThumbHash lossy compression,
        // but we can check if average color remains similar.
        let lqip2 = LQIP::from_image_buffer(&decoded).await.unwrap();
        let avg1 = lqip.average_rgba().unwrap();
        let avg2 = lqip2.average_rgba().unwrap();

        for i in 0..4 {
            assert!(
                (avg1[i] - avg2[i]).abs() < 0.1,
                "Average color mismatch at index {}",
                i
            );
        }
    }

    #[test]
    fn test_invalid_hash_handling() {
        let lqip = LQIP::from_bytes(vec![0, 1, 2]); // Too short
        assert!(lqip.approx_aspect_ratio().is_err());
        assert!(lqip.average_rgba().is_err());
        assert!(lqip.thumb_hash_to_rgba().is_err());
    }

    #[tokio::test]
    async fn test_minimal_image() {
        let rgba = create_solid_rgba(1, 1, 255, 255, 255, 255);
        let lqip = LQIP::from_image_buffer(&rgba).await.unwrap();
        assert!(lqip.approx_aspect_ratio().is_ok());
    }

    #[tokio::test]
    async fn test_dimensions_preserved() {
        // Test that landscape aspect ratio is preserved in reconstructed image dimensions
        let rgba = create_solid_rgba(60, 30, 255, 255, 255, 255);
        let lqip = LQIP::from_image_buffer(&rgba).await.unwrap();
        let decoded = lqip.thumb_hash_to_rgba().unwrap();

        // ThumbHash might not return EXACTLY 60x30, but it should be a landscape
        assert!(
            decoded.width > decoded.height,
            "Reconstructed image should be landscape ({}x{})",
            decoded.width,
            decoded.height
        );
        let ar = decoded.width as f32 / decoded.height as f32;
        assert!(
            (ar - 2.0).abs() < 0.5,
            "Reconstructed aspect ratio {} should be near 2.0",
            ar
        );
    }

    #[tokio::test]
    async fn to_sidecar_carries_version_and_dominant_color() {
        // A mostly-red image: the dominant_color fallback should be red-dominant.
        let rgba = create_solid_rgba(64, 48, 220, 20, 30, 255);
        let lqip = LQIP::from_image_buffer(&rgba).await.unwrap();
        let sidecar = lqip.to_sidecar().unwrap();

        assert_eq!(sidecar.format_version, LQIP_FORMAT_V1);
        assert_eq!(sidecar.chromahash, lqip.as_bytes());
        let [r, g, b] = sidecar.dominant_color;
        assert!(r > g && r > b, "dominant color {r},{g},{b} should be red");
    }

    #[tokio::test]
    async fn render_recognized_version_decodes_placeholder() {
        let rgba = create_solid_rgba(80, 40, 40, 160, 200, 255);
        let lqip = LQIP::from_image_buffer(&rgba).await.unwrap();
        let sidecar = lqip.to_sidecar().unwrap();

        // A recognized version decodes to a non-trivial RGBA placeholder (it "renders").
        let rendered = render_sidecar_lqip(&sidecar);
        assert_eq!(rendered.format, PixelFormat::Rgba);
        assert!(rendered.width > 1 && rendered.height > 1);
        assert_eq!(
            rendered.data.len(),
            rendered.width * rendered.height * 4,
            "decoded buffer is well-formed RGBA"
        );
    }

    #[test]
    fn render_unrecognized_version_falls_back_to_dominant_color() {
        // A future/unknown chromahash version must not misrender: solid dominant_color fill.
        let sidecar = SidecarLqip {
            chromahash: vec![0xDE, 0xAD, 0xBE, 0xEF],
            format_version: LQIP_FORMAT_V1 + 999,
            dominant_color: [12, 34, 56],
        };
        let rendered = render_sidecar_lqip(&sidecar);
        assert_eq!(rendered.format, PixelFormat::Rgba);
        assert_eq!(rendered.data, vec![12, 34, 56, 255]);
    }
}
