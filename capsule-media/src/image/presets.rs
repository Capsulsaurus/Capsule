//! Image encoding presets for various use cases.
//!
//! This module provides pre-configured encoding settings optimized for different
//! scenarios such as web streaming, archival storage, thumbnail generation, and more.
//!
//! # Use Case Categories
//!
//! - **Web/Streaming**: Optimized for fast loading and bandwidth efficiency
//! - **Original/Archive**: Maximum quality preservation for long-term storage
//! - **Download**: Good balance of quality and file size for offline viewing
//! - **Thumbnail**: Small, fast-loading preview images
//! - **Social**: Optimized for social media platforms
//!
//! # Example
//!
//! ```rust
//! use capsule_media::image::presets::ImagePresets;
//! use capsule_media::image::types::ImageOutputSettings;
//!
//! // Get a preset for web streaming in JXL format
//! let settings = ImagePresets::web_streaming_jxl();
//! ```

use crate::image::types::{
    AvifSettings, ChromaSubsampling, ImageOutputSettings, ImageResolution, JpegSettings,
    JxlSettings, PngSettings, StandardImageSize, TiffCompression, TiffSettings, WebPSettings,
};

/// Image encoding presets for various use cases.
///
/// Each preset is optimized for specific scenarios and provides
/// sensible defaults based on the target format's characteristics.
#[derive(Debug, Clone, Copy)]
pub struct ImagePresets;

impl ImagePresets {
    // ========================================================================
    // JPEG Presets
    // ========================================================================

    /// JPEG preset for web streaming - optimized for fast loading.
    ///
    /// - Quality: 80 (good visual quality with reasonable compression)
    /// - Progressive: true (allows gradual loading)
    /// - Chroma subsampling: 420 (reduces file size)
    pub fn web_streaming_jpeg() -> ImageOutputSettings {
        ImageOutputSettings::Jpeg(JpegSettings {
            quality: 80,
            chroma_subsampling: ChromaSubsampling::Cs420,
            progressive: true,
            resolution: Some(ImageResolution::Standard(StandardImageSize::WebLarge)),
        })
    }

    /// JPEG preset for high quality web viewing.
    ///
    /// - Quality: 90
    /// - Progressive: true
    /// - Chroma subsampling: 422 (better color preservation)
    pub fn web_high_quality_jpeg() -> ImageOutputSettings {
        ImageOutputSettings::Jpeg(JpegSettings {
            quality: 90,
            chroma_subsampling: ChromaSubsampling::Cs422,
            progressive: true,
            resolution: None,
        })
    }

    /// JPEG preset for maximum quality (download/archive).
    ///
    /// - Quality: 95
    /// - Progressive: false (smallest file for archival)
    /// - Chroma subsampling: 444 (full color information)
    pub fn archive_jpeg() -> ImageOutputSettings {
        ImageOutputSettings::Jpeg(JpegSettings {
            quality: 95,
            chroma_subsampling: ChromaSubsampling::Cs444,
            progressive: false,
            resolution: None,
        })
    }

    /// JPEG preset for thumbnails - optimized for tiny file sizes.
    ///
    /// - Quality: 70
    /// - Progressive: false
    /// - Chroma subsampling: 420
    pub fn thumbnail_jpeg() -> ImageOutputSettings {
        ImageOutputSettings::Jpeg(JpegSettings {
            quality: 70,
            chroma_subsampling: ChromaSubsampling::Cs420,
            progressive: false,
            resolution: Some(ImageResolution::Standard(StandardImageSize::MediumThumb)),
        })
    }

    /// JPEG preset for social media sharing.
    ///
    /// - Quality: 85 (good quality for recompression)
    /// - Progressive: true
    /// - Chroma subsampling: 420
    pub fn social_jpeg() -> ImageOutputSettings {
        ImageOutputSettings::Jpeg(JpegSettings {
            quality: 85,
            chroma_subsampling: ChromaSubsampling::Cs420,
            progressive: true,
            resolution: Some(ImageResolution::Standard(StandardImageSize::SocialShare)),
        })
    }

    // ========================================================================
    // JPEG XL (JXL) Presets
    // ========================================================================

    /// JXL preset for web streaming - excellent compression with fast decode.
    ///
    /// - Distance: 1.0 (high quality, ~90 quality equivalent)
    /// - Effort: 7 (good balance of encode speed and compression)
    pub fn web_streaming_jxl() -> ImageOutputSettings {
        ImageOutputSettings::Jxl(JxlSettings {
            distance: 1.0,
            effort: 7,
            resolution: None,
        })
    }

    /// JXL preset for visually lossless quality.
    ///
    /// - Distance: 0.5 (near-lossless, imperceptible differences)
    /// - Effort: 7
    pub fn visually_lossless_jxl() -> ImageOutputSettings {
        ImageOutputSettings::Jxl(JxlSettings {
            distance: 0.5,
            effort: 7,
            resolution: None,
        })
    }

    /// JXL preset for mathematically lossless compression.
    ///
    /// - Distance: 0.0 (perfectly lossless)
    /// - Effort: 9 (maximum compression)
    pub fn lossless_jxl() -> ImageOutputSettings {
        ImageOutputSettings::Jxl(JxlSettings {
            distance: 0.0,
            effort: 9,
            resolution: None,
        })
    }

    /// JXL preset for archival - best compression with maximum quality.
    ///
    /// - Distance: 0.0 (lossless)
    /// - Effort: 9 (maximum effort for smallest file)
    pub fn archive_jxl() -> ImageOutputSettings {
        ImageOutputSettings::Jxl(JxlSettings {
            distance: 0.0,
            effort: 9,
            resolution: None,
        })
    }

    /// JXL preset for fast encoding (preview/draft).
    ///
    /// - Distance: 2.0 (good quality for previews)
    /// - Effort: 3 (fast encoding)
    pub fn fast_jxl() -> ImageOutputSettings {
        ImageOutputSettings::Jxl(JxlSettings {
            distance: 2.0,
            effort: 3,
            resolution: None,
        })
    }

    /// JXL preset for thumbnails.
    ///
    /// - Distance: 2.5
    /// - Effort: 5
    pub fn thumbnail_jxl() -> ImageOutputSettings {
        ImageOutputSettings::Jxl(JxlSettings {
            distance: 2.5,
            effort: 5,
            resolution: None,
        })
    }

    /// JXL preset for social media - good quality with reasonable encoding time.
    ///
    /// - Distance: 1.5
    /// - Effort: 6
    pub fn social_jxl() -> ImageOutputSettings {
        ImageOutputSettings::Jxl(JxlSettings {
            distance: 1.5,
            effort: 6,
            resolution: None,
        })
    }

    // ========================================================================
    // PNG Presets
    // ========================================================================

    /// PNG preset for web use - balanced compression.
    ///
    /// - Compression level: 6 (default, good balance)
    /// - Bit depth: 8
    pub fn web_png() -> ImageOutputSettings {
        ImageOutputSettings::Png(PngSettings {
            compression_level: 6,
            bit_depth: 8,
            resolution: None,
        })
    }

    /// PNG preset for lossless archival with maximum compression.
    ///
    /// - Compression level: 9 (maximum)
    /// - Bit depth: 16 (preserve all data)
    pub fn archive_png() -> ImageOutputSettings {
        ImageOutputSettings::Png(PngSettings {
            compression_level: 9,
            bit_depth: 16,
            resolution: None,
        })
    }

    /// PNG preset for fast encoding.
    ///
    /// - Compression level: 1
    /// - Bit depth: 8
    pub fn fast_png() -> ImageOutputSettings {
        ImageOutputSettings::Png(PngSettings {
            compression_level: 1,
            bit_depth: 8,
            resolution: None,
        })
    }

    /// PNG preset for screenshots/UI elements.
    ///
    /// - Compression level: 6
    /// - Bit depth: 8
    pub fn screenshot_png() -> ImageOutputSettings {
        ImageOutputSettings::Png(PngSettings {
            compression_level: 6,
            bit_depth: 8,
            resolution: None,
        })
    }

    /// PNG preset for graphics with transparency.
    ///
    /// - Compression level: 7
    /// - Bit depth: 8
    pub fn transparent_png() -> ImageOutputSettings {
        ImageOutputSettings::Png(PngSettings {
            compression_level: 7,
            bit_depth: 8,
            resolution: None,
        })
    }

    // ========================================================================
    // AVIF Presets
    // ========================================================================

    /// AVIF preset for web streaming - excellent compression.
    ///
    /// - Quality: 50 (equivalent to ~JPEG 80)
    /// - Alpha quality: 50
    /// - Speed: 6 (balanced)
    pub fn web_streaming_avif() -> ImageOutputSettings {
        ImageOutputSettings::Avif(AvifSettings {
            quality: 50,
            alpha_quality: 50,
            speed: 6,
            resolution: None,
        })
    }

    /// AVIF preset for high quality web viewing.
    ///
    /// - Quality: 40 (higher quality)
    /// - Alpha quality: 40
    /// - Speed: 5
    pub fn web_high_quality_avif() -> ImageOutputSettings {
        ImageOutputSettings::Avif(AvifSettings {
            quality: 40,
            alpha_quality: 40,
            speed: 5,
            resolution: None,
        })
    }

    /// AVIF preset for maximum quality (near-lossless).
    ///
    /// - Quality: 20
    /// - Alpha quality: 20
    /// - Speed: 3
    pub fn high_quality_avif() -> ImageOutputSettings {
        ImageOutputSettings::Avif(AvifSettings {
            quality: 20,
            alpha_quality: 20,
            speed: 3,
            resolution: None,
        })
    }

    /// AVIF preset for lossless compression.
    ///
    /// - Quality: 0 (lossless)
    /// - Alpha quality: 0
    /// - Speed: 0 (slowest, best compression)
    pub fn lossless_avif() -> ImageOutputSettings {
        ImageOutputSettings::Avif(AvifSettings {
            quality: 0,
            alpha_quality: 0,
            speed: 0,
            resolution: None,
        })
    }

    /// AVIF preset for thumbnails - fast encoding with small files.
    ///
    /// - Quality: 58
    /// - Alpha quality: 58
    /// - Speed: 8
    pub fn thumbnail_avif() -> ImageOutputSettings {
        ImageOutputSettings::Avif(AvifSettings {
            quality: 58,
            alpha_quality: 58,
            speed: 8,
            resolution: None,
        })
    }

    /// AVIF preset for social media.
    ///
    /// - Quality: 45
    /// - Alpha quality: 45
    /// - Speed: 6
    pub fn social_avif() -> ImageOutputSettings {
        ImageOutputSettings::Avif(AvifSettings {
            quality: 45,
            alpha_quality: 45,
            speed: 6,
            resolution: None,
        })
    }

    // ========================================================================
    // WebP Presets
    // ========================================================================

    /// WebP preset for web streaming.
    ///
    /// - Quality: 80
    /// - Lossless: false
    /// - Effort: 4
    pub fn web_streaming_webp() -> ImageOutputSettings {
        ImageOutputSettings::WebP(WebPSettings {
            quality: 80,
            lossless: false,
            effort: 4,
            resolution: None,
        })
    }

    /// WebP preset for high quality web viewing.
    ///
    /// - Quality: 90
    /// - Lossless: false
    /// - Effort: 5
    pub fn web_high_quality_webp() -> ImageOutputSettings {
        ImageOutputSettings::WebP(WebPSettings {
            quality: 90,
            lossless: false,
            effort: 5,
            resolution: None,
        })
    }

    /// WebP preset for lossless compression.
    ///
    /// - Quality: 100
    /// - Lossless: true
    /// - Effort: 6 (maximum for WebP)
    pub fn lossless_webp() -> ImageOutputSettings {
        ImageOutputSettings::WebP(WebPSettings {
            quality: 100,
            lossless: true,
            effort: 6,
            resolution: None,
        })
    }

    /// WebP preset for thumbnails.
    ///
    /// - Quality: 70
    /// - Lossless: false
    /// - Effort: 3
    pub fn thumbnail_webp() -> ImageOutputSettings {
        ImageOutputSettings::WebP(WebPSettings {
            quality: 70,
            lossless: false,
            effort: 3,
            resolution: None,
        })
    }

    /// WebP preset for animated images (GIF replacement).
    ///
    /// - Quality: 75
    /// - Lossless: false
    /// - Effort: 4
    pub fn animated_webp() -> ImageOutputSettings {
        ImageOutputSettings::WebP(WebPSettings {
            quality: 75,
            lossless: false,
            effort: 4,
            resolution: None,
        })
    }

    /// WebP preset for social media.
    ///
    /// - Quality: 85
    /// - Lossless: false
    /// - Effort: 4
    pub fn social_webp() -> ImageOutputSettings {
        ImageOutputSettings::WebP(WebPSettings {
            quality: 85,
            lossless: false,
            effort: 4,
            resolution: None,
        })
    }

    // ========================================================================
    // TIFF Presets
    // ========================================================================

    /// TIFF preset for archival - uncompressed for maximum compatibility.
    pub fn archive_uncompressed_tiff() -> ImageOutputSettings {
        ImageOutputSettings::Tiff(TiffSettings {
            compression: TiffCompression::None,
            resolution: None,
        })
    }

    /// TIFF preset for archival with LZW lossless compression.
    pub fn archive_lzw_tiff() -> ImageOutputSettings {
        ImageOutputSettings::Tiff(TiffSettings {
            compression: TiffCompression::Lzw,
            resolution: None,
        })
    }

    /// TIFF preset for archival with Deflate (zlib) compression.
    ///
    /// Better compression ratio than LZW for most images.
    pub fn archive_deflate_tiff() -> ImageOutputSettings {
        ImageOutputSettings::Tiff(TiffSettings {
            compression: TiffCompression::Deflate,
            resolution: None,
        })
    }

    /// TIFF preset with JPEG compression for smaller files.
    ///
    /// - Quality: 90
    pub fn jpeg_compressed_tiff() -> ImageOutputSettings {
        ImageOutputSettings::Tiff(TiffSettings {
            compression: TiffCompression::Jpeg(90),
            resolution: None,
        })
    }

    /// TIFF preset for print production.
    ///
    /// Uses LZW compression which is widely supported in print workflows.
    pub fn print_tiff() -> ImageOutputSettings {
        ImageOutputSettings::Tiff(TiffSettings {
            compression: TiffCompression::Lzw,
            resolution: None,
        })
    }

    // ========================================================================
    // Use Case-Based Presets (Format-Agnostic Recommendations)
    // ========================================================================

    /// Best preset for modern web streaming (uses JXL).
    ///
    /// JXL offers the best compression ratio for web delivery while maintaining
    /// excellent quality. Falls back to AVIF or WebP for broader browser support.
    pub fn web_streaming_best() -> ImageOutputSettings {
        Self::web_streaming_jxl()
    }

    /// Best preset for web streaming with broad compatibility (uses WebP).
    ///
    /// WebP has excellent browser support and good compression.
    pub fn web_streaming_compatible() -> ImageOutputSettings {
        Self::web_streaming_webp()
    }

    /// Best preset for legacy browser support (uses JPEG).
    pub fn web_streaming_legacy() -> ImageOutputSettings {
        Self::web_streaming_jpeg()
    }

    /// Best preset for archival storage (uses JXL lossless).
    ///
    /// JXL lossless provides the best compression ratio among lossless formats.
    pub fn archive_best() -> ImageOutputSettings {
        Self::lossless_jxl()
    }

    /// Best preset for photos with minimal quality loss (uses JXL).
    pub fn photo_optimized() -> ImageOutputSettings {
        Self::visually_lossless_jxl()
    }

    /// Best preset for thumbnails (uses AVIF).
    ///
    /// AVIF provides excellent compression at small sizes.
    pub fn thumbnail_best() -> ImageOutputSettings {
        Self::thumbnail_avif()
    }

    /// Best preset for social media (uses JPEG for maximum compatibility).
    pub fn social_compatible() -> ImageOutputSettings {
        Self::social_jpeg()
    }

    /// Fast encoding preset for previews (uses JXL with low effort).
    pub fn preview_fast() -> ImageOutputSettings {
        Self::fast_jxl()
    }
}

/// Preset quality tiers for easy selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityTier {
    /// Lowest quality, smallest file size - suitable for thumbnails
    Thumbnail,
    /// Low quality - suitable for quick previews
    Preview,
    /// Balanced quality and file size - suitable for web streaming
    Web,
    /// High quality with reasonable file size - suitable for high-res web viewing
    HighQuality,
    /// Maximum lossy quality - near indistinguishable from original
    VisuallyLossless,
    /// Mathematically lossless - perfect reproduction
    Lossless,
}

/// Encoding speed preference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedPreference {
    /// Fastest encoding, may sacrifice some compression efficiency
    Fast,
    /// Balanced encoding speed and compression
    Balanced,
    /// Slowest encoding for maximum compression
    Maximum,
}

/// Helper to get appropriate settings for a quality tier and format.
pub fn get_jpeg_for_tier(tier: QualityTier) -> ImageOutputSettings {
    match tier {
        QualityTier::Thumbnail => ImagePresets::thumbnail_jpeg(),
        QualityTier::Preview => ImageOutputSettings::Jpeg(JpegSettings {
            quality: 65,
            chroma_subsampling: ChromaSubsampling::Cs420,
            progressive: false,
            resolution: None,
        }),
        QualityTier::Web => ImagePresets::web_streaming_jpeg(),
        QualityTier::HighQuality => ImagePresets::web_high_quality_jpeg(),
        QualityTier::VisuallyLossless | QualityTier::Lossless => ImagePresets::archive_jpeg(),
    }
}

/// Helper to get appropriate JXL settings for a quality tier.
pub fn get_jxl_for_tier(tier: QualityTier) -> ImageOutputSettings {
    match tier {
        QualityTier::Thumbnail => ImagePresets::thumbnail_jxl(),
        QualityTier::Preview => ImagePresets::fast_jxl(),
        QualityTier::Web => ImagePresets::web_streaming_jxl(),
        QualityTier::HighQuality => ImageOutputSettings::Jxl(JxlSettings {
            distance: 0.8,
            effort: 7,
            resolution: None,
        }),
        QualityTier::VisuallyLossless => ImagePresets::visually_lossless_jxl(),
        QualityTier::Lossless => ImagePresets::lossless_jxl(),
    }
}

/// Helper to get appropriate AVIF settings for a quality tier.
pub fn get_avif_for_tier(tier: QualityTier) -> ImageOutputSettings {
    match tier {
        QualityTier::Thumbnail => ImagePresets::thumbnail_avif(),
        QualityTier::Preview => ImageOutputSettings::Avif(AvifSettings {
            quality: 55,
            alpha_quality: 55,
            speed: 8,
            resolution: None,
        }),
        QualityTier::Web => ImagePresets::web_streaming_avif(),
        QualityTier::HighQuality => ImagePresets::web_high_quality_avif(),
        QualityTier::VisuallyLossless => ImagePresets::high_quality_avif(),
        QualityTier::Lossless => ImagePresets::lossless_avif(),
    }
}

/// Helper to get appropriate WebP settings for a quality tier.
pub fn get_webp_for_tier(tier: QualityTier) -> ImageOutputSettings {
    match tier {
        QualityTier::Thumbnail => ImagePresets::thumbnail_webp(),
        QualityTier::Preview => ImageOutputSettings::WebP(WebPSettings {
            quality: 65,
            lossless: false,
            effort: 2,
            resolution: None,
        }),
        QualityTier::Web => ImagePresets::web_streaming_webp(),
        QualityTier::HighQuality => ImagePresets::web_high_quality_webp(),
        QualityTier::VisuallyLossless => ImageOutputSettings::WebP(WebPSettings {
            quality: 95,
            lossless: false,
            effort: 6,
            resolution: None,
        }),
        QualityTier::Lossless => ImagePresets::lossless_webp(),
    }
}

/// Helper to get appropriate PNG settings for a quality tier.
pub fn get_png_for_tier(tier: QualityTier, speed: SpeedPreference) -> ImageOutputSettings {
    // PNG is always lossless, so we vary compression level and bit depth
    let compression_level = match speed {
        SpeedPreference::Fast => 1,
        SpeedPreference::Balanced => 6,
        SpeedPreference::Maximum => 9,
    };

    let bit_depth = match tier {
        QualityTier::Thumbnail | QualityTier::Preview | QualityTier::Web => 8,
        QualityTier::HighQuality | QualityTier::VisuallyLossless | QualityTier::Lossless => 16,
    };

    ImageOutputSettings::Png(PngSettings {
        compression_level,
        bit_depth,
        resolution: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_jpeg_presets() {
        let settings = ImagePresets::web_streaming_jpeg();
        if let ImageOutputSettings::Jpeg(jpeg) = settings {
            assert_eq!(jpeg.quality, 80);
            assert!(jpeg.progressive);
        } else {
            panic!("Expected JPEG settings");
        }
    }

    #[test]
    fn test_jxl_presets() {
        let settings = ImagePresets::lossless_jxl();
        if let ImageOutputSettings::Jxl(jxl) = settings {
            assert_eq!(jxl.distance, 0.0);
            assert_eq!(jxl.effort, 9);
        } else {
            panic!("Expected JXL settings");
        }
    }

    #[test]
    fn test_quality_tier_jpeg() {
        let thumbnail = get_jpeg_for_tier(QualityTier::Thumbnail);
        let web = get_jpeg_for_tier(QualityTier::Web);

        if let (ImageOutputSettings::Jpeg(thumb), ImageOutputSettings::Jpeg(web_settings)) =
            (thumbnail, web)
        {
            assert!(thumb.quality < web_settings.quality);
        } else {
            panic!("Expected JPEG settings");
        }
    }

    #[test]
    fn test_quality_tier_jxl() {
        let lossless = get_jxl_for_tier(QualityTier::Lossless);
        let web = get_jxl_for_tier(QualityTier::Web);

        if let (ImageOutputSettings::Jxl(lossless_jxl), ImageOutputSettings::Jxl(web_jxl)) =
            (lossless, web)
        {
            assert_eq!(lossless_jxl.distance, 0.0);
            assert!(web_jxl.distance > 0.0);
        } else {
            panic!("Expected JXL settings");
        }
    }
}
