//! Video encoding presets for various use cases.
//!
//! This module provides pre-configured encoding settings optimized for different
//! scenarios such as web streaming, archival storage, social media sharing, and more.
//!
//! # Use Case Categories
//!
//! - **Streaming**: Optimized for online playback (low latency, adaptive bitrate friendly)
//! - **Download**: Balanced quality and file size for offline viewing
//! - **Archive/Original**: Maximum quality preservation for long-term storage
//! - **Social Media**: Platform-specific optimizations
//!
//! # Streaming Tiers
//!
//! Videos are commonly served in multiple quality levels for adaptive bitrate streaming:
//! - **4K (2160p)**: Ultra HD, ~15-25 Mbps
//! - **1080p**: Full HD, ~5-8 Mbps  
//! - **720p**: HD, ~2.5-5 Mbps
//! - **480p**: SD, ~1-2.5 Mbps
//! - **360p**: Low, ~0.5-1 Mbps
//!
//! # Example
//!
//! ```rust
//! use capsule_media::video::presets::VideoPresets;
//! use capsule_media::video::types::VideoOutputSettings;
//!
//! // Get a preset for 1080p streaming
//! let settings = VideoPresets::streaming_1080p_h264();
//! ```

use crate::video::types::{
    AudioSettings, Mp4Codec, Mp4Settings, RateControl, StandardResolution, VideoOutputSettings,
    VideoResolution, VpxDeadline, WebmCodec, WebmSettings, X264Preset,
};

/// Video encoding presets for various use cases.
///
/// Each preset is optimized for specific scenarios and provides
/// sensible defaults based on the target format and use case.
///
/// All presets set `resolution`, `frame_rate`, and `audio` to `None` by default,
/// which means the original values will be preserved. Use the builder pattern
/// or modify the returned settings if you need to change these.
#[derive(Debug, Clone, Copy)]
pub struct VideoPresets;

impl VideoPresets {
    // ========================================================================
    // MP4 (H.264/H.265) Streaming Presets
    // ========================================================================

    /// MP4/H.264 preset for 4K (2160p) streaming.
    ///
    /// - Codec: H.264
    /// - CRF: 23 (high quality)
    /// - Preset: Slow (better compression)
    /// - Resolution: 4K (3840x2160)
    ///
    /// Target bitrate: ~15-25 Mbps
    pub fn streaming_4k_h264() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Crf(23),
            preset: X264Preset::Slow,
            resolution: Some(VideoResolution::Standard(StandardResolution::Uhd4K)),
            frame_rate: None,
            audio: Some(AudioSettings::aac(192)),
        })
    }

    /// MP4/H.264 preset for 1080p (Full HD) streaming.
    ///
    /// Most common streaming quality level.
    ///
    /// - Codec: H.264
    /// - CRF: 23
    /// - Preset: Medium
    /// - Resolution: 1080p (1920x1080)
    ///
    /// Target bitrate: ~5-8 Mbps
    pub fn streaming_1080p_h264() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Crf(23),
            preset: X264Preset::Medium,
            resolution: Some(VideoResolution::Standard(StandardResolution::Fhd1080p)),
            frame_rate: None,
            audio: Some(AudioSettings::aac(192)),
        })
    }

    /// MP4/H.264 preset for 720p (HD) streaming.
    ///
    /// Good quality for mobile and slower connections.
    ///
    /// - Codec: H.264
    /// - CRF: 24
    /// - Preset: Medium
    /// - Resolution: 720p (1280x720)
    ///
    /// Target bitrate: ~2.5-5 Mbps
    pub fn streaming_720p_h264() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Crf(24),
            preset: X264Preset::Medium,
            resolution: Some(VideoResolution::Standard(StandardResolution::Hd720p)),
            frame_rate: None,
            audio: Some(AudioSettings::aac(128)),
        })
    }

    /// MP4/H.264 preset for 480p (SD) streaming.
    ///
    /// Lower quality for bandwidth-constrained scenarios.
    ///
    /// - Codec: H.264
    /// - CRF: 25
    /// - Preset: Fast
    /// - Resolution: 480p (854x480)
    ///
    /// Target bitrate: ~1-2.5 Mbps
    pub fn streaming_480p_h264() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Crf(25),
            preset: X264Preset::Fast,
            resolution: Some(VideoResolution::Standard(StandardResolution::Sd480p)),
            frame_rate: None,
            audio: Some(AudioSettings::aac(96)),
        })
    }

    /// MP4/H.264 preset for 360p (low quality) streaming.
    ///
    /// Minimum quality for very slow connections.
    ///
    /// - Codec: H.264
    /// - CRF: 26
    /// - Preset: Faster
    /// - Resolution: 360p (640x360)
    ///
    /// Target bitrate: ~0.5-1 Mbps
    pub fn streaming_360p_h264() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Crf(26),
            preset: X264Preset::Faster,
            resolution: Some(VideoResolution::Standard(StandardResolution::Low360p)),
            frame_rate: None,
            audio: Some(AudioSettings::aac(64)),
        })
    }

    // ========================================================================
    // MP4 (H.265/HEVC) Streaming Presets - Better compression than H.264
    // ========================================================================

    /// MP4/H.265 preset for 4K (2160p) streaming.
    ///
    /// H.265 provides ~40% better compression than H.264 at same quality.
    ///
    /// - Codec: H.265
    /// - CRF: 26 (equivalent to H.264 CRF 23)
    /// - Preset: Medium
    /// - Resolution: 4K (3840x2160)
    ///
    /// Target bitrate: ~10-15 Mbps
    pub fn streaming_4k_h265() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H265,
            rate_control: RateControl::Crf(26),
            preset: X264Preset::Medium,
            resolution: Some(VideoResolution::Standard(StandardResolution::Uhd4K)),
            frame_rate: None,
            audio: Some(AudioSettings::aac(192)),
        })
    }

    /// MP4/H.265 preset for 1080p (Full HD) streaming.
    ///
    /// - Codec: H.265
    /// - CRF: 26
    /// - Preset: Medium
    /// - Resolution: 1080p (1920x1080)
    ///
    /// Target bitrate: ~3-5 Mbps
    pub fn streaming_1080p_h265() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H265,
            rate_control: RateControl::Crf(26),
            preset: X264Preset::Medium,
            resolution: Some(VideoResolution::Standard(StandardResolution::Fhd1080p)),
            frame_rate: None,
            audio: Some(AudioSettings::aac(192)),
        })
    }

    /// MP4/H.265 preset for 720p streaming.
    ///
    /// - Codec: H.265
    /// - CRF: 27
    /// - Preset: Medium
    /// - Resolution: 720p (1280x720)
    ///
    /// Target bitrate: ~1.5-2.5 Mbps
    pub fn streaming_720p_h265() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H265,
            rate_control: RateControl::Crf(27),
            preset: X264Preset::Medium,
            resolution: Some(VideoResolution::Standard(StandardResolution::Hd720p)),
            frame_rate: None,
            audio: Some(AudioSettings::aac(128)),
        })
    }

    // ========================================================================
    // MP4 Bitrate-Controlled Presets (for strict bandwidth limits)
    // ========================================================================

    /// MP4 preset with constrained bitrate for live streaming.
    ///
    /// Uses VBV (Video Buffering Verifier) to ensure consistent bitrate.
    /// Resolution is not set - input resolution is preserved.
    ///
    /// - Target: 8 Mbps, Max: 10 Mbps
    /// - Preset: Veryfast (for real-time encoding)
    pub fn live_streaming_h264() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Bitrate {
                target: 8000,
                max: 10000,
            },
            preset: X264Preset::Veryfast,
            resolution: None, // Keep original for live streaming
            frame_rate: None,
            audio: Some(AudioSettings::aac(128)),
        })
    }

    /// MP4 preset for low-latency streaming.
    ///
    /// - Target: 4 Mbps, Max: 6 Mbps
    /// - Preset: Ultrafast
    pub fn low_latency_h264() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Bitrate {
                target: 4000,
                max: 6000,
            },
            preset: X264Preset::Ultrafast,
            resolution: None,
            frame_rate: None,
            audio: Some(AudioSettings::aac(128)),
        })
    }

    // ========================================================================
    // MP4 Archive/Download Presets
    // ========================================================================

    /// MP4 preset for high quality archival.
    ///
    /// Optimized for maximum quality with reasonable file size.
    /// Resolution is not set - original resolution is preserved.
    ///
    /// - Codec: H.265
    /// - CRF: 18 (visually lossless)
    /// - Preset: Slow
    pub fn archive_h265() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H265,
            rate_control: RateControl::Crf(18),
            preset: X264Preset::Slow,
            resolution: None, // Keep original for archival
            frame_rate: None,
            audio: Some(AudioSettings::aac_high_quality()),
        })
    }

    /// MP4 preset for maximum quality (near-lossless).
    ///
    /// - Codec: H.265
    /// - CRF: 15
    /// - Preset: Veryslow
    pub fn maximum_quality_h265() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H265,
            rate_control: RateControl::Crf(15),
            preset: X264Preset::Veryslow,
            resolution: None,
            frame_rate: None,
            audio: Some(AudioSettings::aac_high_quality()),
        })
    }

    /// MP4 preset for download - balanced quality and size.
    ///
    /// - Codec: H.264
    /// - CRF: 21
    /// - Preset: Slow
    pub fn download_h264() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Crf(21),
            preset: X264Preset::Slow,
            resolution: None,
            frame_rate: None,
            audio: Some(AudioSettings::aac(192)),
        })
    }

    // ========================================================================
    // WebM (VP9/AV1) Presets
    // ========================================================================

    /// WebM/VP9 preset for web streaming.
    ///
    /// VP9 offers excellent compression, widely supported in browsers.
    ///
    /// - Codec: VP9
    /// - CRF: 32 (good quality)
    /// - Deadline: Good
    pub fn streaming_vp9() -> VideoOutputSettings {
        VideoOutputSettings::Webm(WebmSettings {
            codec: WebmCodec::Vp9,
            crf: 32,
            deadline: VpxDeadline::Good,
            cpu_used: None,
            resolution: None,
            frame_rate: None,
            audio: Some(AudioSettings::opus(128)),
        })
    }

    /// WebM/VP9 preset for high quality streaming.
    ///
    /// - Codec: VP9
    /// - CRF: 24
    /// - Deadline: Best
    pub fn high_quality_vp9() -> VideoOutputSettings {
        VideoOutputSettings::Webm(WebmSettings {
            codec: WebmCodec::Vp9,
            crf: 24,
            deadline: VpxDeadline::Best,
            cpu_used: None,
            resolution: None,
            frame_rate: None,
            audio: Some(AudioSettings::opus(192)),
        })
    }

    /// WebM/AV1 preset for modern streaming.
    ///
    /// AV1 provides ~30% better compression than VP9 but slower encoding.
    ///
    /// - Codec: AV1
    /// - CRF: 35
    /// - Deadline: Good
    /// - CPU used: 4 (balanced)
    pub fn streaming_av1() -> VideoOutputSettings {
        VideoOutputSettings::Webm(WebmSettings {
            codec: WebmCodec::Av1,
            crf: 35,
            deadline: VpxDeadline::Good,
            cpu_used: Some(4),
            resolution: None,
            frame_rate: None,
            audio: Some(AudioSettings::opus(128)),
        })
    }

    /// WebM/AV1 preset for high quality streaming.
    ///
    /// - Codec: AV1
    /// - CRF: 28
    /// - Deadline: Best
    /// - CPU used: 2 (slower, better quality)
    pub fn high_quality_av1() -> VideoOutputSettings {
        VideoOutputSettings::Webm(WebmSettings {
            codec: WebmCodec::Av1,
            crf: 28,
            deadline: VpxDeadline::Best,
            cpu_used: Some(2),
            resolution: None,
            frame_rate: None,
            audio: Some(AudioSettings::opus(192)),
        })
    }

    /// WebM/AV1 preset for archival - maximum quality.
    ///
    /// - Codec: AV1
    /// - CRF: 20
    /// - Deadline: Best
    /// - CPU used: 0 (slowest, best quality)
    pub fn archive_av1() -> VideoOutputSettings {
        VideoOutputSettings::Webm(WebmSettings {
            codec: WebmCodec::Av1,
            crf: 20,
            deadline: VpxDeadline::Best,
            cpu_used: Some(0),
            resolution: None,
            frame_rate: None,
            audio: Some(AudioSettings::opus_high_quality()),
        })
    }

    /// WebM preset for real-time encoding.
    ///
    /// - Codec: VP9
    /// - CRF: 38
    /// - Deadline: Realtime
    pub fn realtime_vp9() -> VideoOutputSettings {
        VideoOutputSettings::Webm(WebmSettings {
            codec: WebmCodec::Vp9,
            crf: 38,
            deadline: VpxDeadline::Realtime,
            cpu_used: None,
            resolution: None,
            frame_rate: None,
            audio: Some(AudioSettings::opus(96)),
        })
    }

    // ========================================================================
    // Social Media Presets
    // ========================================================================

    /// Preset optimized for YouTube upload.
    ///
    /// YouTube re-encodes everything, so we use high quality source.
    /// Resolution is not set - upload your source resolution.
    ///
    /// - Format: MP4/H.264
    /// - CRF: 18
    /// - Preset: Slow
    pub fn youtube() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Crf(18),
            preset: X264Preset::Slow,
            resolution: None, // YouTube recommends uploading at source resolution
            frame_rate: None,
            audio: Some(AudioSettings::aac(320)), // High bitrate for YouTube
        })
    }

    /// Preset optimized for Instagram/TikTok.
    ///
    /// Optimized for mobile viewing - 1080p is recommended.
    ///
    /// - Format: MP4/H.264
    /// - CRF: 22
    /// - Preset: Medium
    /// - Resolution: 1080p
    pub fn instagram() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Crf(22),
            preset: X264Preset::Medium,
            resolution: Some(VideoResolution::Standard(StandardResolution::Fhd1080p)),
            frame_rate: None,
            audio: Some(AudioSettings::aac(192)),
        })
    }

    /// Preset optimized for Twitter/X.
    ///
    /// - Format: MP4/H.264
    /// - CRF: 23
    /// - Preset: Medium
    /// - Resolution: 720p (Twitter compresses heavily)
    pub fn twitter() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Crf(23),
            preset: X264Preset::Medium,
            resolution: Some(VideoResolution::Standard(StandardResolution::Hd720p)),
            frame_rate: None,
            audio: Some(AudioSettings::aac(128)),
        })
    }

    /// Preset optimized for Discord (8MB limit for non-Nitro).
    ///
    /// Uses bitrate control to stay under file size limits.
    /// Consider also reducing resolution for longer videos.
    ///
    /// - Format: MP4/H.264
    /// - Bitrate: 1000 kbps target, 1500 kbps max
    /// - Preset: Fast
    /// - Resolution: 480p
    pub fn discord_small() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Bitrate {
                target: 1000,
                max: 1500,
            },
            preset: X264Preset::Fast,
            resolution: Some(VideoResolution::Standard(StandardResolution::Sd480p)),
            frame_rate: None,
            audio: Some(AudioSettings::aac(64)),
        })
    }

    /// Preset for Discord Nitro (50MB limit).
    ///
    /// - Format: MP4/H.264
    /// - Bitrate: 5000 kbps target, 6000 kbps max
    /// - Preset: Medium
    /// - Resolution: 1080p
    pub fn discord_nitro() -> VideoOutputSettings {
        VideoOutputSettings::Mp4(Mp4Settings {
            codec: Mp4Codec::H264,
            rate_control: RateControl::Bitrate {
                target: 5000,
                max: 6000,
            },
            preset: X264Preset::Medium,
            resolution: Some(VideoResolution::Standard(StandardResolution::Fhd1080p)),
            frame_rate: None,
            audio: Some(AudioSettings::aac(128)),
        })
    }

    // ========================================================================
    // Use Case-Based Recommendations
    // ========================================================================

    /// Best preset for modern web streaming (balanced quality/compatibility).
    ///
    /// Uses H.264 for maximum compatibility with good quality.
    pub fn web_streaming_best() -> VideoOutputSettings {
        Self::streaming_1080p_h264()
    }

    /// Best preset for modern browsers (uses VP9).
    ///
    /// VP9 offers better compression than H.264.
    pub fn web_streaming_modern() -> VideoOutputSettings {
        Self::streaming_vp9()
    }

    /// Best preset for cutting-edge browsers (uses AV1).
    ///
    /// AV1 offers the best compression available.
    pub fn web_streaming_cutting_edge() -> VideoOutputSettings {
        Self::streaming_av1()
    }

    /// Best preset for archival storage.
    ///
    /// Uses H.265 for excellent quality with good compression.
    pub fn archive_best() -> VideoOutputSettings {
        Self::archive_h265()
    }
}

/// Video resolution tiers for streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingResolution {
    /// 3840x2160 (4K UHD)
    Resolution4K,
    /// 1920x1080 (Full HD)
    Resolution1080p,
    /// 1280x720 (HD)
    Resolution720p,
    /// 854x480 (SD)
    Resolution480p,
    /// 640x360 (Low)
    Resolution360p,
}

impl StreamingResolution {
    /// Get the width for this resolution.
    pub fn width(&self) -> u32 {
        match self {
            Self::Resolution4K => 3840,
            Self::Resolution1080p => 1920,
            Self::Resolution720p => 1280,
            Self::Resolution480p => 854,
            Self::Resolution360p => 640,
        }
    }

    /// Get the height for this resolution.
    pub fn height(&self) -> u32 {
        match self {
            Self::Resolution4K => 2160,
            Self::Resolution1080p => 1080,
            Self::Resolution720p => 720,
            Self::Resolution480p => 480,
            Self::Resolution360p => 360,
        }
    }

    /// Get recommended bitrate in kbps for this resolution.
    pub fn recommended_bitrate_kbps(&self) -> u32 {
        match self {
            Self::Resolution4K => 20000,
            Self::Resolution1080p => 6000,
            Self::Resolution720p => 3500,
            Self::Resolution480p => 1800,
            Self::Resolution360p => 800,
        }
    }

    /// Convert to VideoResolution type.
    pub fn to_video_resolution(&self) -> VideoResolution {
        match self {
            Self::Resolution4K => VideoResolution::Standard(StandardResolution::Uhd4K),
            Self::Resolution1080p => VideoResolution::Standard(StandardResolution::Fhd1080p),
            Self::Resolution720p => VideoResolution::Standard(StandardResolution::Hd720p),
            Self::Resolution480p => VideoResolution::Standard(StandardResolution::Sd480p),
            Self::Resolution360p => VideoResolution::Standard(StandardResolution::Low360p),
        }
    }
}

/// Encoding quality tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoQualityTier {
    /// Lowest quality, fastest encoding
    Draft,
    /// Low quality, fast encoding
    Preview,
    /// Balanced quality and speed
    Standard,
    /// High quality, slower encoding
    High,
    /// Maximum quality, slowest encoding
    Maximum,
    /// Lossless or near-lossless
    Lossless,
}

/// Get H.264 preset for a given quality tier.
///
/// Returns settings without resolution set (original resolution preserved).
pub fn get_h264_for_tier(tier: VideoQualityTier) -> VideoOutputSettings {
    let (crf, preset) = match tier {
        VideoQualityTier::Draft => (28, X264Preset::Ultrafast),
        VideoQualityTier::Preview => (26, X264Preset::Veryfast),
        VideoQualityTier::Standard => (23, X264Preset::Medium),
        VideoQualityTier::High => (20, X264Preset::Slow),
        VideoQualityTier::Maximum => (18, X264Preset::Veryslow),
        VideoQualityTier::Lossless => (0, X264Preset::Veryslow), // CRF 0 is lossless
    };

    VideoOutputSettings::Mp4(Mp4Settings {
        codec: Mp4Codec::H264,
        rate_control: RateControl::Crf(crf),
        preset,
        resolution: None,
        frame_rate: None,
        audio: None,
    })
}

/// Get H.265 preset for a given quality tier.
///
/// Returns settings without resolution set (original resolution preserved).
pub fn get_h265_for_tier(tier: VideoQualityTier) -> VideoOutputSettings {
    let (crf, preset) = match tier {
        VideoQualityTier::Draft => (32, X264Preset::Ultrafast),
        VideoQualityTier::Preview => (30, X264Preset::Veryfast),
        VideoQualityTier::Standard => (26, X264Preset::Medium),
        VideoQualityTier::High => (22, X264Preset::Slow),
        VideoQualityTier::Maximum => (18, X264Preset::Veryslow),
        VideoQualityTier::Lossless => (0, X264Preset::Veryslow),
    };

    VideoOutputSettings::Mp4(Mp4Settings {
        codec: Mp4Codec::H265,
        rate_control: RateControl::Crf(crf),
        preset,
        resolution: None,
        frame_rate: None,
        audio: None,
    })
}

/// Get VP9 preset for a given quality tier.
///
/// Returns settings without resolution set (original resolution preserved).
pub fn get_vp9_for_tier(tier: VideoQualityTier) -> VideoOutputSettings {
    let (crf, deadline) = match tier {
        VideoQualityTier::Draft => (45, VpxDeadline::Realtime),
        VideoQualityTier::Preview => (40, VpxDeadline::Realtime),
        VideoQualityTier::Standard => (32, VpxDeadline::Good),
        VideoQualityTier::High => (26, VpxDeadline::Good),
        VideoQualityTier::Maximum => (20, VpxDeadline::Best),
        VideoQualityTier::Lossless => (0, VpxDeadline::Best),
    };

    VideoOutputSettings::Webm(WebmSettings {
        codec: WebmCodec::Vp9,
        crf,
        deadline,
        cpu_used: None,
        resolution: None,
        frame_rate: None,
        audio: None,
    })
}

/// Get AV1 preset for a given quality tier.
///
/// Returns settings without resolution set (original resolution preserved).
pub fn get_av1_for_tier(tier: VideoQualityTier) -> VideoOutputSettings {
    let (crf, deadline, cpu_used) = match tier {
        VideoQualityTier::Draft => (50, VpxDeadline::Realtime, 8),
        VideoQualityTier::Preview => (45, VpxDeadline::Realtime, 6),
        VideoQualityTier::Standard => (35, VpxDeadline::Good, 4),
        VideoQualityTier::High => (28, VpxDeadline::Good, 2),
        VideoQualityTier::Maximum => (20, VpxDeadline::Best, 1),
        VideoQualityTier::Lossless => (0, VpxDeadline::Best, 0),
    };

    VideoOutputSettings::Webm(WebmSettings {
        codec: WebmCodec::Av1,
        crf,
        deadline,
        cpu_used: Some(cpu_used),
        resolution: None,
        frame_rate: None,
        audio: None,
    })
}

/// Get H.264 streaming preset for a given resolution.
pub fn get_h264_for_resolution(resolution: StreamingResolution) -> VideoOutputSettings {
    match resolution {
        StreamingResolution::Resolution4K => VideoPresets::streaming_4k_h264(),
        StreamingResolution::Resolution1080p => VideoPresets::streaming_1080p_h264(),
        StreamingResolution::Resolution720p => VideoPresets::streaming_720p_h264(),
        StreamingResolution::Resolution480p => VideoPresets::streaming_480p_h264(),
        StreamingResolution::Resolution360p => VideoPresets::streaming_360p_h264(),
    }
}

/// Get H.265 streaming preset for a given resolution.
pub fn get_h265_for_resolution(resolution: StreamingResolution) -> VideoOutputSettings {
    match resolution {
        StreamingResolution::Resolution4K => VideoPresets::streaming_4k_h265(),
        StreamingResolution::Resolution1080p => VideoPresets::streaming_1080p_h265(),
        StreamingResolution::Resolution720p => VideoPresets::streaming_720p_h265(),
        // Fall back to 720p settings for lower resolutions
        StreamingResolution::Resolution480p | StreamingResolution::Resolution360p => {
            VideoOutputSettings::Mp4(Mp4Settings {
                codec: Mp4Codec::H265,
                rate_control: RateControl::Crf(28),
                preset: X264Preset::Fast,
                resolution: Some(resolution.to_video_resolution()),
                frame_rate: None,
                audio: Some(AudioSettings::aac(96)),
            })
        }
    }
}

/// Generate a complete set of adaptive streaming presets.
///
/// Returns presets for all standard resolutions, useful for creating
/// multiple quality variants for adaptive bitrate streaming (HLS/DASH).
pub fn adaptive_streaming_h264() -> Vec<(StreamingResolution, VideoOutputSettings)> {
    vec![
        (
            StreamingResolution::Resolution1080p,
            VideoPresets::streaming_1080p_h264(),
        ),
        (
            StreamingResolution::Resolution720p,
            VideoPresets::streaming_720p_h264(),
        ),
        (
            StreamingResolution::Resolution480p,
            VideoPresets::streaming_480p_h264(),
        ),
        (
            StreamingResolution::Resolution360p,
            VideoPresets::streaming_360p_h264(),
        ),
    ]
}

/// Generate a complete set of adaptive streaming presets for 4K source.
pub fn adaptive_streaming_h264_4k() -> Vec<(StreamingResolution, VideoOutputSettings)> {
    vec![
        (
            StreamingResolution::Resolution4K,
            VideoPresets::streaming_4k_h264(),
        ),
        (
            StreamingResolution::Resolution1080p,
            VideoPresets::streaming_1080p_h264(),
        ),
        (
            StreamingResolution::Resolution720p,
            VideoPresets::streaming_720p_h264(),
        ),
        (
            StreamingResolution::Resolution480p,
            VideoPresets::streaming_480p_h264(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_streaming_presets() {
        let preset = VideoPresets::streaming_1080p_h264();
        if let VideoOutputSettings::Mp4(mp4) = preset {
            assert!(matches!(mp4.codec, Mp4Codec::H264));
            if let RateControl::Crf(crf) = mp4.rate_control {
                assert_eq!(crf, 23);
            } else {
                panic!("Expected CRF rate control");
            }
            // Check resolution is set
            assert!(mp4.resolution.is_some());
            // Check audio is set
            assert!(mp4.audio.is_some());
        } else {
            panic!("Expected MP4 settings");
        }
    }

    #[test]
    fn test_resolution_dimensions() {
        assert_eq!(StreamingResolution::Resolution1080p.width(), 1920);
        assert_eq!(StreamingResolution::Resolution1080p.height(), 1080);
        assert_eq!(StreamingResolution::Resolution4K.width(), 3840);
    }

    #[test]
    fn test_quality_tier_ordering() {
        // Higher tiers should have lower CRF (better quality)
        let draft = get_h264_for_tier(VideoQualityTier::Draft);
        let max = get_h264_for_tier(VideoQualityTier::Maximum);

        if let (VideoOutputSettings::Mp4(draft_mp4), VideoOutputSettings::Mp4(max_mp4)) =
            (draft, max)
        {
            if let (RateControl::Crf(draft_crf), RateControl::Crf(max_crf)) =
                (draft_mp4.rate_control, max_mp4.rate_control)
            {
                assert!(draft_crf > max_crf);
            } else {
                panic!("Expected CRF rate control");
            }
        } else {
            panic!("Expected MP4 settings");
        }
    }

    #[test]
    fn test_adaptive_streaming() {
        let presets = adaptive_streaming_h264();
        assert_eq!(presets.len(), 4);

        // Check resolutions are in descending order
        assert_eq!(presets[0].0, StreamingResolution::Resolution1080p);
        assert_eq!(presets[1].0, StreamingResolution::Resolution720p);
        assert_eq!(presets[2].0, StreamingResolution::Resolution480p);
        assert_eq!(presets[3].0, StreamingResolution::Resolution360p);
    }

    #[test]
    fn test_webm_presets() {
        let vp9 = VideoPresets::streaming_vp9();
        let av1 = VideoPresets::streaming_av1();

        if let VideoOutputSettings::Webm(vp9_settings) = vp9 {
            assert!(matches!(vp9_settings.codec, WebmCodec::Vp9));
            assert!(matches!(vp9_settings.deadline, VpxDeadline::Good));
            assert!(vp9_settings.audio.is_some());
        } else {
            panic!("Expected WebM settings");
        }

        if let VideoOutputSettings::Webm(av1_settings) = av1 {
            assert!(matches!(av1_settings.codec, WebmCodec::Av1));
            assert!(av1_settings.cpu_used.is_some());
        } else {
            panic!("Expected WebM settings");
        }
    }

    #[test]
    fn test_x264_preset_ffmpeg_str() {
        assert_eq!(X264Preset::Medium.as_ffmpeg_str(), "medium");
        assert_eq!(X264Preset::Ultrafast.as_ffmpeg_str(), "ultrafast");
        assert_eq!(X264Preset::Veryslow.as_ffmpeg_str(), "veryslow");
    }

    #[test]
    fn test_vpx_deadline_ffmpeg_str() {
        assert_eq!(VpxDeadline::Good.as_ffmpeg_str(), "good");
        assert_eq!(VpxDeadline::Best.as_ffmpeg_str(), "best");
        assert_eq!(VpxDeadline::Realtime.as_ffmpeg_str(), "realtime");
    }

    #[test]
    fn test_archival_presets_preserve_resolution() {
        let archive = VideoPresets::archive_h265();
        if let VideoOutputSettings::Mp4(mp4) = archive {
            // Archival should preserve original resolution
            assert!(mp4.resolution.is_none());
        } else {
            panic!("Expected MP4 settings");
        }
    }

    #[test]
    fn test_social_media_audio_settings() {
        let youtube = VideoPresets::youtube();
        if let VideoOutputSettings::Mp4(mp4) = youtube {
            // YouTube preset should have high audio bitrate
            if let Some(audio) = mp4.audio {
                assert_eq!(audio.bitrate_kbps, Some(320));
            } else {
                panic!("Expected audio settings");
            }
        } else {
            panic!("Expected MP4 settings");
        }
    }
}
