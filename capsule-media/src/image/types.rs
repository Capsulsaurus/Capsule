use serde::{Deserialize, Serialize};

/// Image Format
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ImageFormat {
    Jpeg,
    Jxl,
    Heic,
    Png,
    Tiff,
    Avif,
    WebP,
    Gif,
    Bmp,
    Raw(RawImageFormat),
}

/// RAW Image Format
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum RawImageFormat {
    /// Adobe Digital Negative
    Dng,
    /// Sony ARW
    Arw,
    /// Canon CR2
    Cr2,
    /// Canon CR3
    Cr3,
    /// Nikon NEF
    Nef,
    /// Fujifilm RAF
    Raf,
}

// ============================================================================
// Image Output Settings
// ============================================================================

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum ImageOutputSettings {
    Jpeg(JpegSettings),
    Jxl(JxlSettings),
    // Heic(HeicSettings),
    Png(PngSettings),
    Tiff(TiffSettings),
    Avif(AvifSettings),
    WebP(WebPSettings),
    // Gif(GifSettings),
    // Dng(DngSettings),
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JpegSettings {
    pub quality: u8, // 1-100
    pub chroma_subsampling: ChromaSubsampling,
    pub progressive: bool,
    /// Target resolution (None = keep original)
    pub resolution: Option<ImageResolution>,
}

/// JPEG chroma subsampling modes.
///
/// Chroma subsampling reduces the color information in an image to reduce file size.
/// Human eyes are more sensitive to brightness than color, so this can significantly
/// reduce file size with minimal visible quality loss.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChromaSubsampling {
    /// 4:4:4 - No chroma subsampling (full color resolution)
    /// Best quality, largest file size
    Cs444,
    /// 4:2:2 - Horizontal chroma subsampling
    /// Good balance of quality and size
    Cs422,
    /// 4:2:0 - Horizontal and vertical chroma subsampling
    /// Most common for web, smallest file size
    Cs420,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JxlSettings {
    pub distance: f32, // 0.0 (lossless) to 15.0
    pub effort: u8,    // 1-9
    /// Target resolution (None = keep original)
    pub resolution: Option<ImageResolution>,
}

// #[derive(Serialize, Deserialize, Debug, Clone)]
// pub struct HeicSettings {
//     pub crf: u8,   // 0-51
//     pub speed: u8, // 0-9
// }

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct PngSettings {
    pub compression_level: u8, // 0-9
    pub bit_depth: u8,         // 8 or 16
    /// Target resolution (None = keep original)
    pub resolution: Option<ImageResolution>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TiffSettings {
    pub compression: TiffCompression,
    /// Target resolution (None = keep original)
    pub resolution: Option<ImageResolution>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum TiffCompression {
    None,
    Lzw,
    Deflate,
    Jpeg(u8), // Quality
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct AvifSettings {
    pub quality: u8,       // 0-63
    pub alpha_quality: u8, // 0-63
    pub speed: u8,         // 0-10
    /// Target resolution (None = keep original)
    pub resolution: Option<ImageResolution>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WebPSettings {
    pub quality: u8, // 0-100
    pub lossless: bool,
    pub effort: u8, // 0-6
    /// Target resolution (None = keep original)
    pub resolution: Option<ImageResolution>,
}

// ============================================================================
// Image Resolution Settings
// ============================================================================

/// Target image resolution/scaling.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageResolution {
    /// Exact dimensions (width, height)
    Exact { width: u32, height: u32 },
    /// Scale to fit within max dimension while preserving aspect ratio
    /// The larger dimension will be at most this value
    MaxDimension(u32),
    /// Scale by width, height calculated to preserve aspect ratio
    ScaleToWidth(u32),
    /// Scale by height, width calculated to preserve aspect ratio
    ScaleToHeight(u32),
    /// Square crop and resize (for avatars, thumbnails)
    /// Crops to square center then scales to this dimension
    Square(u32),
    /// Standard image size preset
    Standard(StandardImageSize),
}

/// Standard image size presets.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardImageSize {
    // Thumbnail sizes
    /// 64x64 - Tiny icon
    Tiny,
    /// 128x128 - Small thumbnail
    SmallThumb,
    /// 256x256 - Medium thumbnail
    MediumThumb,
    /// 512x512 - Large thumbnail
    LargeThumb,

    // Social media sizes
    /// 1080x1080 - Instagram square
    InstagramSquare,
    /// 1080x1350 - Instagram portrait
    InstagramPortrait,
    /// 1200x630 - Facebook/Twitter link preview
    SocialShare,
    /// 1500x500 - Twitter header
    TwitterHeader,

    // Web sizes
    /// 800px max dimension
    WebSmall,
    /// 1200px max dimension
    WebMedium,
    /// 1920px max dimension (Full HD)
    WebLarge,
    /// 2560px max dimension (2K)
    WebExtraLarge,

    // Print sizes (at 300 DPI)
    /// 1200x1800 - 4x6 inch
    Print4x6,
    /// 1500x2100 - 5x7 inch
    Print5x7,
    /// 2400x3600 - 8x12 inch
    Print8x12,
}

impl StandardImageSize {
    /// Get the dimensions or max dimension for this size.
    /// Returns (width, height, is_exact) where is_exact indicates if both dimensions are fixed.
    pub fn dimensions(&self) -> (u32, u32, bool) {
        match self {
            // Thumbnails (exact squares)
            Self::Tiny => (64, 64, true),
            Self::SmallThumb => (128, 128, true),
            Self::MediumThumb => (256, 256, true),
            Self::LargeThumb => (512, 512, true),

            // Social (exact)
            Self::InstagramSquare => (1080, 1080, true),
            Self::InstagramPortrait => (1080, 1350, true),
            Self::SocialShare => (1200, 630, true),
            Self::TwitterHeader => (1500, 500, true),

            // Web (max dimension, aspect ratio preserved)
            Self::WebSmall => (800, 800, false),
            Self::WebMedium => (1200, 1200, false),
            Self::WebLarge => (1920, 1920, false),
            Self::WebExtraLarge => (2560, 2560, false),

            // Print (exact)
            Self::Print4x6 => (1200, 1800, true),
            Self::Print5x7 => (1500, 2100, true),
            Self::Print8x12 => (2400, 3600, true),
        }
    }

    /// Get as an ImageResolution variant.
    pub fn to_image_resolution(&self) -> ImageResolution {
        ImageResolution::Standard(*self)
    }
}

impl ImageResolution {
    /// Create an exact resolution.
    pub fn exact(width: u32, height: u32) -> Self {
        Self::Exact { width, height }
    }

    /// Create a square resolution (crop and resize).
    pub fn square(size: u32) -> Self {
        Self::Square(size)
    }

    /// Create a max dimension resolution.
    pub fn max(dimension: u32) -> Self {
        Self::MaxDimension(dimension)
    }

    // Thumbnail helpers
    pub fn tiny() -> Self {
        Self::Standard(StandardImageSize::Tiny)
    }

    pub fn small_thumb() -> Self {
        Self::Standard(StandardImageSize::SmallThumb)
    }

    pub fn medium_thumb() -> Self {
        Self::Standard(StandardImageSize::MediumThumb)
    }

    pub fn large_thumb() -> Self {
        Self::Standard(StandardImageSize::LargeThumb)
    }

    // Social media helpers
    pub fn instagram_square() -> Self {
        Self::Standard(StandardImageSize::InstagramSquare)
    }

    pub fn instagram_portrait() -> Self {
        Self::Standard(StandardImageSize::InstagramPortrait)
    }

    pub fn social_share() -> Self {
        Self::Standard(StandardImageSize::SocialShare)
    }

    // Web helpers
    pub fn web_small() -> Self {
        Self::Standard(StandardImageSize::WebSmall)
    }

    pub fn web_medium() -> Self {
        Self::Standard(StandardImageSize::WebMedium)
    }

    pub fn web_large() -> Self {
        Self::Standard(StandardImageSize::WebLarge)
    }
}
