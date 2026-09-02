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
//! box filter accumulated in integers and divided by an exact sample count: deterministic on
//! every target, and the right filter for a large downscale anyway (a box average over the full
//! source rect is alias-free, where a bilinear tap would ignore most of the source pixels).
//!
//! Every product here is computed in `u64`, never in `usize`, because two of the CI-gated
//! targets (`armv7-linux-androideabi`, `i686-linux-android`) are 32-bit and both the
//! destination-to-source boundary and the channel accumulator reach past `u32` for shapes this
//! function accepts.
//!
//! Determinism here is **necessary, not sufficient**, and the distinction matters: the bytes a
//! manifest actually signs come out of libwebp, and a libwebp version bump can change them for
//! the same input. That is fine — each generation signs the bytes it produced and manifests of a
//! role chain in order — but it means "the resample is deterministic" buys reproducibility of
//! *this* step, not a stable content address across toolchains.
//!
//! Upscaling is not a thing this performs: a tier only ever caps a long edge, and a source
//! already inside the cap takes the `format = "original"` sentinel path instead
//! ([`DerivativeTier`](super::DerivativeTier)).

use crate::lqip::RgbaImage;

/// The dimensions a `width` x `height` frame takes when its long edge is capped at
/// `max_long_edge`, preserving aspect ratio.
///
/// Returns the input unchanged when it already fits, so a caller can compare and skip — and
/// unchanged for an empty frame, which is the one case where "never zero" cannot hold: there is
/// no non-degenerate size for a frame with no pixels, and inventing one would have
/// [`downscale_rgba8`] read a buffer that has nothing in it.
pub fn capped_dimensions(width: u32, height: u32, max_long_edge: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (width, height);
    }
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
/// A frame already within the cap — or an empty one — is returned unchanged (cloned), which is
/// what makes this safe to call unconditionally. Deterministic: identical input yields
/// byte-identical output on every target.
pub fn downscale_rgba8(source: &RgbaImage, max_long_edge: u32) -> RgbaImage {
    let (dst_w, dst_h) = capped_dimensions(source.width, source.height, max_long_edge);
    if (dst_w, dst_h) == (source.width, source.height) {
        return source.clone();
    }

    let (src_w, src_h) = (source.width as usize, source.height as usize);
    // `u64`, not `usize`: on a 32-bit target `w * h * 4` overflows above ~1 Gpx, and the whole
    // point of this branch is to be reached rather than to panic on its own arithmetic.
    if source.rgba.len() as u64 != u64::from(source.width) * u64::from(source.height) * 4 {
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

    // Boundary products and the channel accumulator are `u64`, not `usize`/`u32`, and both
    // widths are load-bearing rather than defensive habit:
    //
    // - `(y + 1) * src_h` reaches `dst_edge * src_edge`. For a 1 x 256M frame reduced to a
    //   256 px long edge that is 6.5e10, which overflows a 32-bit `usize` — and two of the
    //   CI-gated targets (`armv7-linux-androideabi`, `i686-linux-android`) are 32-bit.
    // - the per-channel sum reaches `count * 255`, and `count` is the whole frame when this is
    //   called with a cap of 1 (a `pub` entry point, so that is reachable), i.e. 6.5e10 again —
    //   past `u32::MAX`.
    //
    // Neither is hypothetical-only: a debug build panics on the overflow and a release build
    // wraps into wrong pixels or an out-of-bounds index.
    for y in 0..dh {
        // The source rows this destination row averages. Floor boundaries, so the destination
        // grid is an exact partition of the source grid — every source pixel contributes to
        // exactly one output pixel. Widened to at least one row because a lopsided cap can put
        // two destination rows inside one source row, and an empty rect would divide by zero.
        let y0 = (y as u64 * src_h as u64 / dh as u64) as usize;
        let y1 = (((y as u64 + 1) * src_h as u64 / dh as u64) as usize)
            .max(y0 + 1)
            .min(src_h);
        for x in 0..dw {
            let x0 = (x as u64 * src_w as u64 / dw as u64) as usize;
            let x1 = (((x as u64 + 1) * src_w as u64 / dw as u64) as usize)
                .max(x0 + 1)
                .min(src_w);

            let mut acc = [0u64; 4];
            let count = ((y1 - y0) * (x1 - x0)) as u64;
            for sy in y0..y1 {
                let row = sy * src_w * 4;
                for sx in x0..x1 {
                    let i = row + sx * 4;
                    acc[0] += u64::from(source.rgba[i]);
                    acc[1] += u64::from(source.rgba[i + 1]);
                    acc[2] += u64::from(source.rgba[i + 2]);
                    acc[3] += u64::from(source.rgba[i + 3]);
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
