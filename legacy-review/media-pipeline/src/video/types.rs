use serde::{Deserialize, Serialize};

/// Supported video container formats.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat {
    Mp4,
    Webm,
    Mov,
    Avi,
    Mkv,
}

// ============================================================================
// Video Output Settings
// ============================================================================

/// Output settings for video encoding.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum VideoOutputSettings {
    Mp4(Mp4Settings),
    Webm(WebmSettings),
}

/// MP4 container settings with H.264/H.265 (AVC/HEVC) codecs.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Mp4Settings {
    /// Video codec (H.264/AVC or H.265/HEVC)
    pub codec: Mp4Codec,
    /// Rate control mode (CRF or bitrate-based)
    pub rate_control: RateControl,
    /// Encoder speed/quality preset (x264/x265 naming)
    pub preset: X264Preset,
    /// Target resolution (None = keep original)
    pub resolution: Option<VideoResolution>,
    /// Target frame rate (None = keep original)
    pub frame_rate: Option<FrameRate>,
    /// Audio settings (None = copy audio stream)
    pub audio: Option<AudioSettings>,
}

/// WebM container settings with VP9/AV1 codecs.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WebmSettings {
    /// Video codec (VP9 or AV1)
    pub codec: WebmCodec,
    /// Constant Rate Factor (0-63 for VP9, 0-63 for AV1)
    /// Lower values = higher quality
    pub crf: u8,
    /// Encoding deadline/speed (affects quality vs speed tradeoff)
    pub deadline: VpxDeadline,
    /// CPU usage / speed preset for AV1 (0-8 for libaom-av1)
    /// Lower = slower/better quality, Higher = faster/lower quality
    pub cpu_used: Option<u8>,
    /// Target resolution (None = keep original)
    pub resolution: Option<VideoResolution>,
    /// Target frame rate (None = keep original)
    pub frame_rate: Option<FrameRate>,
    /// Audio settings (None = use Opus for WebM)
    pub audio: Option<AudioSettings>,
}

// ============================================================================
// Resolution Settings
// ============================================================================

/// Target video resolution.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoResolution {
    /// Exact dimensions (width, height)
    Exact { width: u32, height: u32 },
    /// Scale to fit within max dimension while preserving aspect ratio
    /// The larger dimension will be at most this value
    MaxDimension(u32),
    /// Scale by width, height calculated to preserve aspect ratio
    ScaleToWidth(u32),
    /// Scale by height, width calculated to preserve aspect ratio
    ScaleToHeight(u32),
    /// Standard resolution preset
    Standard(StandardResolution),
}

/// Standard video resolution presets.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardResolution {
    /// 3840x2160 (4K UHD)
    Uhd4K,
    /// 2560x1440 (2K QHD)
    Qhd1440p,
    /// 1920x1080 (Full HD)
    Fhd1080p,
    /// 1280x720 (HD)
    Hd720p,
    /// 854x480 (SD)
    Sd480p,
    /// 640x360 (Low)
    Low360p,
}

impl StandardResolution {
    /// Get the width for this resolution.
    pub fn width(&self) -> u32 {
        match self {
            Self::Uhd4K => 3840,
            Self::Qhd1440p => 2560,
            Self::Fhd1080p => 1920,
            Self::Hd720p => 1280,
            Self::Sd480p => 854,
            Self::Low360p => 640,
        }
    }

    /// Get the height for this resolution.
    pub fn height(&self) -> u32 {
        match self {
            Self::Uhd4K => 2160,
            Self::Qhd1440p => 1440,
            Self::Fhd1080p => 1080,
            Self::Hd720p => 720,
            Self::Sd480p => 480,
            Self::Low360p => 360,
        }
    }

    /// Get the total pixel count.
    pub fn pixels(&self) -> u64 {
        self.width() as u64 * self.height() as u64
    }
}

impl VideoResolution {
    /// Create an exact resolution.
    pub fn exact(width: u32, height: u32) -> Self {
        Self::Exact { width, height }
    }

    /// Create a 4K resolution.
    pub fn uhd_4k() -> Self {
        Self::Standard(StandardResolution::Uhd4K)
    }

    /// Create a 1080p resolution.
    pub fn fhd_1080p() -> Self {
        Self::Standard(StandardResolution::Fhd1080p)
    }

    /// Create a 720p resolution.
    pub fn hd_720p() -> Self {
        Self::Standard(StandardResolution::Hd720p)
    }

    /// Create a 480p resolution.
    pub fn sd_480p() -> Self {
        Self::Standard(StandardResolution::Sd480p)
    }

    /// Create a 360p resolution.
    pub fn low_360p() -> Self {
        Self::Standard(StandardResolution::Low360p)
    }
}

// ============================================================================
// Frame Rate Settings
// ============================================================================

/// Target frame rate.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum FrameRate {
    /// Exact frame rate (e.g., 30.0, 29.97, 23.976)
    Fps(f64),
    /// Frame rate as a fraction (numerator/denominator)
    /// Useful for precise rates like 30000/1001 = 29.97fps
    Fraction { num: u32, den: u32 },
    /// Common frame rate presets
    Standard(StandardFrameRate),
}

/// Standard frame rate presets.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandardFrameRate {
    /// 24 fps (film)
    Fps24,
    /// 23.976 fps (NTSC film)
    Fps23_976,
    /// 25 fps (PAL)
    Fps25,
    /// 29.97 fps (NTSC)
    Fps29_97,
    /// 30 fps
    Fps30,
    /// 50 fps (PAL high frame rate)
    Fps50,
    /// 59.94 fps (NTSC high frame rate)
    Fps59_94,
    /// 60 fps
    Fps60,
}

impl StandardFrameRate {
    /// Get the frame rate as a floating point value.
    pub fn as_fps(&self) -> f64 {
        match self {
            Self::Fps24 => 24.0,
            Self::Fps23_976 => 24000.0 / 1001.0,
            Self::Fps25 => 25.0,
            Self::Fps29_97 => 30000.0 / 1001.0,
            Self::Fps30 => 30.0,
            Self::Fps50 => 50.0,
            Self::Fps59_94 => 60000.0 / 1001.0,
            Self::Fps60 => 60.0,
        }
    }

    /// Get the frame rate as a fraction (numerator, denominator).
    pub fn as_fraction(&self) -> (u32, u32) {
        match self {
            Self::Fps24 => (24, 1),
            Self::Fps23_976 => (24000, 1001),
            Self::Fps25 => (25, 1),
            Self::Fps29_97 => (30000, 1001),
            Self::Fps30 => (30, 1),
            Self::Fps50 => (50, 1),
            Self::Fps59_94 => (60000, 1001),
            Self::Fps60 => (60, 1),
        }
    }
}

impl FrameRate {
    /// Get the frame rate as a floating point value.
    pub fn as_fps(&self) -> f64 {
        match self {
            Self::Fps(fps) => *fps,
            Self::Fraction { num, den } => *num as f64 / *den as f64,
            Self::Standard(std) => std.as_fps(),
        }
    }

    /// Create a 24fps frame rate.
    pub fn fps_24() -> Self {
        Self::Standard(StandardFrameRate::Fps24)
    }

    /// Create a 30fps frame rate.
    pub fn fps_30() -> Self {
        Self::Standard(StandardFrameRate::Fps30)
    }

    /// Create a 60fps frame rate.
    pub fn fps_60() -> Self {
        Self::Standard(StandardFrameRate::Fps60)
    }
}

// ============================================================================
// Audio Settings
// ============================================================================

/// Audio encoding settings.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub struct AudioSettings {
    /// Audio codec
    pub codec: AudioCodec,
    /// Audio bitrate in kbps (None = codec default)
    pub bitrate_kbps: Option<u32>,
    /// Sample rate in Hz (None = keep original, typically 44100 or 48000)
    pub sample_rate: Option<u32>,
    /// Number of audio channels (None = keep original)
    pub channels: Option<AudioChannels>,
}

impl AudioSettings {
    /// Create AAC audio settings with specified bitrate.
    pub fn aac(bitrate_kbps: u32) -> Self {
        Self {
            codec: AudioCodec::Aac,
            bitrate_kbps: Some(bitrate_kbps),
            sample_rate: None,
            channels: None,
        }
    }

    /// Create Opus audio settings with specified bitrate.
    pub fn opus(bitrate_kbps: u32) -> Self {
        Self {
            codec: AudioCodec::Opus,
            bitrate_kbps: Some(bitrate_kbps),
            sample_rate: None,
            channels: None,
        }
    }

    /// Create settings to copy audio without re-encoding.
    pub fn copy() -> Self {
        Self {
            codec: AudioCodec::Copy,
            bitrate_kbps: None,
            sample_rate: None,
            channels: None,
        }
    }

    /// Default high-quality AAC settings (192 kbps stereo).
    pub fn aac_high_quality() -> Self {
        Self {
            codec: AudioCodec::Aac,
            bitrate_kbps: Some(192),
            sample_rate: Some(48000),
            channels: Some(AudioChannels::Stereo),
        }
    }

    /// Default high-quality Opus settings (128 kbps stereo).
    pub fn opus_high_quality() -> Self {
        Self {
            codec: AudioCodec::Opus,
            bitrate_kbps: Some(128),
            sample_rate: Some(48000),
            channels: Some(AudioChannels::Stereo),
        }
    }
}

/// Audio codec.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    /// AAC - Most compatible, good for MP4
    Aac,
    /// Opus - Best quality/size ratio, required for WebM
    Opus,
    /// Vorbis - Legacy WebM audio codec
    Vorbis,
    /// MP3 - Legacy, wide compatibility
    Mp3,
    /// Copy audio stream without re-encoding
    Copy,
    /// No audio
    None,
}

/// Audio channel configuration.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioChannels {
    /// Mono (1 channel)
    Mono,
    /// Stereo (2 channels)
    Stereo,
    /// 5.1 surround (6 channels)
    Surround51,
    /// 7.1 surround (8 channels)
    Surround71,
    /// Custom channel count
    Custom(u8),
}

impl AudioChannels {
    /// Get the number of channels.
    pub fn count(&self) -> u8 {
        match self {
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::Surround51 => 6,
            Self::Surround71 => 8,
            Self::Custom(n) => *n,
        }
    }
}

// ============================================================================
// Codec Enums
// ============================================================================

/// MP4 video codecs (H.264/AVC and H.265/HEVC families).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mp4Codec {
    /// H.264/AVC - Most compatible, widely supported
    /// Encoder: libx264
    H264,
    /// H.265/HEVC - Better compression than H.264 (~40% smaller at same quality)
    /// Encoder: libx265
    H265,
}

/// WebM video codecs (VP9 and AV1).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebmCodec {
    /// VP9 - Good compression, wide browser support
    /// Encoder: libvpx-vp9
    Vp9,
    /// AV1 - Best compression (~30% better than VP9), newer browser support
    /// Encoder: libaom-av1 or libsvtav1
    Av1,
}

// ============================================================================
// Rate Control
// ============================================================================

/// Video rate control mode.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum RateControl {
    /// Constant Rate Factor (quality-based)
    /// For x264/x265: 0-51, where 0 is lossless, 18-28 is typical
    /// For VP9/AV1: 0-63, where 0 is lossless
    Crf(u8),
    /// Bitrate-based rate control with target and max bitrate (in kbps)
    Bitrate {
        /// Target bitrate in kbps
        target: u32,
        /// Maximum bitrate in kbps (for VBV/buffer compliance)
        max: u32,
    },
}

// ============================================================================
// Encoder Presets
// ============================================================================

/// x264/x265 encoder speed presets.
///
/// These presets control the encoding speed vs compression efficiency tradeoff.
/// Slower presets produce smaller files at the same quality but take longer to encode.
///
/// Named to match ffmpeg/x264/x265 `-preset` option.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum X264Preset {
    /// Fastest encoding, largest file size
    Ultrafast,
    Superfast,
    Veryfast,
    Faster,
    Fast,
    /// Default balance of speed and compression
    Medium,
    Slow,
    Slower,
    /// Slowest encoding, smallest file size
    Veryslow,
}

impl X264Preset {
    /// Returns the ffmpeg preset string.
    pub fn as_ffmpeg_str(&self) -> &'static str {
        match self {
            Self::Ultrafast => "ultrafast",
            Self::Superfast => "superfast",
            Self::Veryfast => "veryfast",
            Self::Faster => "faster",
            Self::Fast => "fast",
            Self::Medium => "medium",
            Self::Slow => "slow",
            Self::Slower => "slower",
            Self::Veryslow => "veryslow",
        }
    }
}

/// VP9/AV1 encoding deadline (speed preset).
///
/// Controls the encoding speed vs quality tradeoff for libvpx-vp9 and libaom-av1.
/// Named to match ffmpeg `-deadline` option for libvpx.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum VpxDeadline {
    /// Best quality, slowest encoding
    /// For production/final output
    Best,
    /// Good balance of quality and speed
    /// Recommended for most uses
    Good,
    /// Fastest encoding, may sacrifice quality
    /// For real-time/live streaming
    Realtime,
}

impl VpxDeadline {
    /// Returns the ffmpeg deadline string.
    pub fn as_ffmpeg_str(&self) -> &'static str {
        match self {
            Self::Best => "best",
            Self::Good => "good",
            Self::Realtime => "realtime",
        }
    }
}
