//! Video-as-sparse-photos keyframe selection (SSoT: [AI/ML — Video-as-Sparse-Photos]).
//!
//! Processing every frame through heavy models is prohibitive, so a video is treated as a sparse
//! set of keyframes: content-aware cut detection chunks it into scenes (provided here as input),
//! each scene is sampled at the 10% / 50% / 90% timestamps, and frames too blurry (low Laplacian
//! variance) are rejected before they enter the image queue. Pure functions over timing + blur
//! metrics — the heavy decode/cut-detect lives in a runner.
//!
//! [AI/ML — Video-as-Sparse-Photos]: https://docs/design/ai/#video-as-sparse-photos

/// A content-aware scene as a `[start_ms, end_ms)` range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Scene {
    /// Scene start (ms from the video start).
    pub start_ms: u64,
    /// Scene end (exclusive).
    pub end_ms: u64,
}

/// A sampled keyframe — its timestamp in the parent video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keyframe {
    /// Timestamp (ms) of the sampled frame in the parent video.
    pub timestamp_ms: u64,
}

/// The temporal sample points within each scene (percent of the scene duration).
pub const SAMPLE_FRACTIONS_PERCENT: [u64; 3] = [10, 50, 90];

/// Sample keyframes at the 10% / 50% / 90% timestamps of each scene.
pub fn sample_keyframes(scenes: &[Scene]) -> Vec<Keyframe> {
    let mut out = Vec::with_capacity(scenes.len() * SAMPLE_FRACTIONS_PERCENT.len());
    for s in scenes {
        let dur = s.end_ms.saturating_sub(s.start_ms);
        for frac in SAMPLE_FRACTIONS_PERCENT {
            out.push(Keyframe {
                timestamp_ms: s.start_ms + dur * frac / 100,
            });
        }
    }
    out
}

/// Whether a frame is sharp enough to keep: the variance of its Laplacian is at or above
/// `threshold`. Below it, the frame is discarded as too blurry.
pub fn is_sharp(laplacian_variance: f64, threshold: f64) -> bool {
    laplacian_variance >= threshold
}

/// Keep only the keyframes whose Laplacian variance clears `threshold`.
pub fn reject_blurry(frames: Vec<(Keyframe, f64)>, threshold: f64) -> Vec<Keyframe> {
    frames
        .into_iter()
        .filter(|(_, var)| is_sharp(*var, threshold))
        .map(|(kf, _)| kf)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_at_ten_fifty_ninety_percent_of_each_scene() {
        let scenes = [
            Scene {
                start_ms: 0,
                end_ms: 1000,
            },
            Scene {
                start_ms: 2000,
                end_ms: 2500,
            },
        ];
        let kf = sample_keyframes(&scenes);
        let ts: Vec<u64> = kf.iter().map(|k| k.timestamp_ms).collect();
        assert_eq!(ts, vec![100, 500, 900, 2050, 2250, 2450]);
    }

    #[test]
    fn empty_input_yields_no_keyframes() {
        assert!(sample_keyframes(&[]).is_empty());
    }

    #[test]
    fn blur_rejection_drops_frames_below_threshold() {
        let frames = vec![
            (Keyframe { timestamp_ms: 100 }, 5.0),   // blurry
            (Keyframe { timestamp_ms: 500 }, 150.0), // sharp
            (Keyframe { timestamp_ms: 900 }, 99.9),  // just below
        ];
        let kept = reject_blurry(frames, 100.0);
        assert_eq!(
            kept.iter().map(|k| k.timestamp_ms).collect::<Vec<_>>(),
            vec![500]
        );
    }
}
