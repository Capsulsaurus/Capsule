//! Capsule-owned downscale for the derivative tiers.
//!
//! `rawshift-image` has no resize — it offers crop, flips, rotations, blur and lens correction,
//! and nothing that changes the sample grid — so tier sizing is Capsule's, not the codec's.
//!
//! # Why integer area-averaging, specifically
//!
//! A derivative's bytes are content-addressed by a **signed** `DerivativeManifest`, so two runs
//! over the same source must produce the same bytes. That rules out floating-point accumulation
//! whose order or width could differ between builds and targets, and it rules out any resampler
//! with platform-tuned SIMD paths that are not required to be bit-identical. What is left is a
//! box filter accumulated in `u32` and divided by an exact sample count: deterministic on every
//! target, and the right filter for a large downscale anyway (a box average over the full source
//! rect is alias-free, where a bilinear tap would ignore most of the source pixels).
//!
//! Upscaling is not a thing this performs: a tier only ever caps a long edge, and a source
//! already inside the cap takes the `format = "original"` sentinel path instead
//! ([`DerivativeTier`](super::DerivativeTier)).

use crate::lqip::RgbaImage;

/// The dimensions a `width` x `height` frame takes when its long edge is capped at
/// `max_long_edge`, preserving aspect ratio and never returning a zero dimension.
///
/// Returns the input unchanged when it already fits, so a caller can compare and skip.
pub fn capped_dimensions(width: u32, height: u32, max_long_edge: u32) -> (u32, u32) {
    let cap = max_long_edge.max(1);
    let long_edge = width.max(height);
    if long_edge <= cap {
        return (width, height);
    }
    // Rounded rather than truncated so a 3:2 frame keeps its ratio as closely as an integer
    // grid allows; `.max(1)` because a very lopsided frame (e.g. 8000x3) would otherwise round
    // its short edge to zero and produce an empty buffer.
    let scale = |edge: u32| -> u32 {
        let numerator = u64::from(edge) * u64::from(cap);
        let denominator = u64::from(long_edge);
        (((numerator + denominator / 2) / denominator) as u32).max(1)
    };
    (scale(width), scale(height))
}

/// Downscale packed RGBA8 so its long edge is at most `max_long_edge`.
///
/// A frame already within the cap is returned unchanged (cloned), which is what makes this safe
/// to call unconditionally. Deterministic: identical input yields byte-identical output on every
/// target.
pub fn downscale_rgba8(source: &RgbaImage, max_long_edge: u32) -> RgbaImage {
    let (dst_w, dst_h) = capped_dimensions(source.width, source.height, max_long_edge);
    if (dst_w, dst_h) == (source.width, source.height) {
        return source.clone();
    }

    let (src_w, src_h) = (source.width as usize, source.height as usize);
    if source.rgba.len() != src_w * src_h * 4 {
        // Defensive: this is a `pub` entry point and the very next thing it does is index the
        // buffer by those dimensions. Every in-tree caller passes a `DecodedImage`, whose
        // invariant this is, so a mismatch is a bug in a *new* caller — reported and returned
        // unchanged rather than turned into a panic inside an import.
        tracing::error!(
            width = source.width,
            height = source.height,
            len = source.rgba.len(),
            "media: downscale refused a buffer that does not match its dimensions"
        );
        return source.clone();
    }
    let (dw, dh) = (dst_w as usize, dst_h as usize);
    let mut out = Vec::with_capacity(dw * dh * 4);

    for y in 0..dh {
        // The source rows this destination row averages. Floor boundaries, so the destination
        // grid is an exact partition of the source grid — every source pixel contributes to
        // exactly one output pixel. Widened to at least one row because a lopsided cap can put
        // two destination rows inside one source row, and an empty rect would divide by zero.
        let y0 = y * src_h / dh;
        let y1 = ((y + 1) * src_h / dh).max(y0 + 1).min(src_h);
        for x in 0..dw {
            let x0 = x * src_w / dw;
            let x1 = ((x + 1) * src_w / dw).max(x0 + 1).min(src_w);

            let mut acc = [0u32; 4];
            let count = ((y1 - y0) * (x1 - x0)) as u32;
            for sy in y0..y1 {
                let row = sy * src_w * 4;
                for sx in x0..x1 {
                    let i = row + sx * 4;
                    acc[0] += u32::from(source.rgba[i]);
                    acc[1] += u32::from(source.rgba[i + 1]);
                    acc[2] += u32::from(source.rgba[i + 2]);
                    acc[3] += u32::from(source.rgba[i + 3]);
                }
            }
            // Round-half-up on the mean, so a uniform region reproduces its own value exactly
            // rather than drifting down by up to one level per reduction.
            for channel in acc {
                out.push(((channel + count / 2) / count) as u8);
            }
        }
    }

    RgbaImage {
        width: dst_w,
        height: dst_h,
        rgba: out,
    }
}
