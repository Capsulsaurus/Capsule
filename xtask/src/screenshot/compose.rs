//! `xtask screenshot-compose`: turn a raw simulator capture into the README hero image — the
//! phone screen with device-shaped corners and a soft drop shadow, on a transparent canvas so it
//! sits cleanly on both GitHub themes.
//!
//! The screen shape comes from the capture itself: the mise task asks `simctl io screenshot` for
//! `--mask=alpha`, which emits Apple's exact (continuous-curvature) display mask. An opaque
//! capture — taken without the mask — falls back to a circular-arc rounded rectangle of
//! [`Style::fallback_corner_radius`].
//!
//! Output is deterministic: the same capture always yields byte-identical PNG bytes.

use std::fs;
use std::path::Path;

use eyre::{Context, Result, eyre};
use image::imageops::fast_blur;
use image::{
    DynamicImage, GrayImage, ImageDecoder, ImageEncoder, ImageReader, Luma, Rgba, RgbaImage,
};
use tracing::info;

/// Every tunable of the composition, in capture pixels (3x for the iPhone 17 Pro capture).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Style {
    /// Corner radius applied when the capture carries no alpha mask of its own.
    /// 62 pt is the iPhone 16/17 Pro display radius.
    pub(crate) fallback_corner_radius: f32,
    /// How far the shadow drops below the screen.
    pub(crate) shadow_offset_y: u32,
    /// Gaussian spread of the shadow.
    pub(crate) shadow_sigma: f32,
    /// Peak shadow opacity (0–1).
    pub(crate) shadow_opacity: f32,
}

/// The README hero style.
pub(crate) const HERO: Style = Style {
    fallback_corner_radius: 62.0 * 3.0,
    shadow_offset_y: 30,
    shadow_sigma: 45.0,
    shadow_opacity: 0.28,
};

impl Style {
    /// Transparent margin on every side: room for the full blur plus the drop, so nothing clips.
    pub(crate) fn padding(&self) -> u32 {
        (self.shadow_sigma * 3.0).ceil() as u32 + self.shadow_offset_y
    }
}

/// Read `input`, compose with `style`, write a PNG to `output`. The capture's colour profile (the
/// simulator may tag it Display P3) is carried into the output so its colours are not reinterpreted.
pub(crate) fn run(input: &Path, output: &Path, style: &Style) -> Result<()> {
    let mut decoder = ImageReader::open(input)
        .with_context(|| format!("reading capture {}", input.display()))?
        .with_guessed_format()?
        .into_decoder()?;
    let icc = decoder.icc_profile()?;
    let capture = DynamicImage::from_decoder(decoder)?.into_rgba8();
    let composed = compose(&capture, style);
    let png = encode_png(&composed, icc)?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(output, &png).with_context(|| format!("writing {}", output.display()))?;
    info!(
        input = %input.display(),
        output = %output.display(),
        width = composed.width(),
        height = composed.height(),
        bytes = png.len(),
        "composed hero image"
    );
    Ok(())
}

/// Compose `capture` onto a padded transparent canvas with a drop shadow.
pub(crate) fn compose(capture: &RgbaImage, style: &Style) -> RgbaImage {
    let screen = with_screen_mask(capture, style.fallback_corner_radius);
    let pad = style.padding();
    let (w, h) = (screen.width() + 2 * pad, screen.height() + 2 * pad);

    // Shadow: the screen's own alpha, dropped and blurred.
    let mut silhouette = GrayImage::new(w, h);
    for (x, y, px) in screen.enumerate_pixels() {
        silhouette.put_pixel(x + pad, y + pad + style.shadow_offset_y, Luma([px[3]]));
    }
    let shadow = fast_blur(&silhouette, style.shadow_sigma);

    let mut canvas = RgbaImage::from_fn(w, h, |x, y| {
        let a = (f32::from(shadow.get_pixel(x, y)[0]) * style.shadow_opacity).round() as u8;
        Rgba([0, 0, 0, a])
    });
    for (x, y, px) in screen.enumerate_pixels() {
        let dst = canvas.get_pixel_mut(x + pad, y + pad);
        *dst = over(*px, *dst);
    }
    canvas
}

/// The capture with its screen shape as alpha: its own mask when it has one, otherwise a rounded
/// rectangle of `radius`.
fn with_screen_mask(capture: &RgbaImage, radius: f32) -> RgbaImage {
    if capture.pixels().any(|p| p[3] < u8::MAX) {
        return capture.clone();
    }
    let (w, h) = (capture.width() as f32, capture.height() as f32);
    let mut out = capture.clone();
    for (x, y, px) in out.enumerate_pixels_mut() {
        let coverage = rounded_rect_coverage(x as f32 + 0.5, y as f32 + 0.5, w, h, radius);
        px[3] = (coverage * 255.0).round() as u8;
    }
    out
}

/// Anti-aliased coverage (0–1) of the pixel centred at `(px, py)` by a `w`×`h` rounded rectangle,
/// from the signed distance to its edge.
fn rounded_rect_coverage(px: f32, py: f32, w: f32, h: f32, r: f32) -> f32 {
    let r = r.min(w / 2.0).min(h / 2.0);
    let dx = (px - w / 2.0).abs() - (w / 2.0 - r);
    let dy = (py - h / 2.0).abs() - (h / 2.0 - r);
    let outside = dx.max(0.0).hypot(dy.max(0.0));
    let inside = dx.max(dy).min(0.0);
    let distance = outside + inside - r;
    (0.5 - distance).clamp(0.0, 1.0)
}

/// Porter–Duff source-over for straight (non-premultiplied) alpha.
fn over(src: Rgba<u8>, dst: Rgba<u8>) -> Rgba<u8> {
    let sa = f32::from(src[3]) / 255.0;
    let da = f32::from(dst[3]) / 255.0;
    let oa = sa + da * (1.0 - sa);
    if oa <= 0.0 {
        return Rgba([0, 0, 0, 0]);
    }
    let channel = |i: usize| {
        let c = (f32::from(src[i]) * sa + f32::from(dst[i]) * da * (1.0 - sa)) / oa;
        c.round() as u8
    };
    Rgba([
        channel(0),
        channel(1),
        channel(2),
        (oa * 255.0).round() as u8,
    ])
}

/// PNG with no ancillary chunks beyond the colour profile (no timestamps), so equal pixels and
/// profile mean equal bytes.
fn encode_png(image: &RgbaImage, icc: Option<Vec<u8>>) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut encoder = image::codecs::png::PngEncoder::new_with_quality(
        &mut out,
        image::codecs::png::CompressionType::Best,
        image::codecs::png::FilterType::Adaptive,
    );
    if let Some(icc) = icc {
        encoder
            .set_icc_profile(icc)
            .map_err(|e| eyre!("embedding ICC profile: {e}"))?;
    }
    encoder.write_image(
        image.as_raw(),
        image.width(),
        image.height(),
        image::ExtendedColorType::Rgba8,
    )?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const STYLE: Style = Style {
        fallback_corner_radius: 20.0,
        shadow_offset_y: 6,
        shadow_sigma: 5.0,
        shadow_opacity: 0.3,
    };

    fn opaque(w: u32, h: u32) -> RgbaImage {
        RgbaImage::from_fn(w, h, |x, y| {
            Rgba([(x % 200) as u8 + 20, (y % 200) as u8 + 20, 90, 255])
        })
    }

    #[test]
    fn canvas_is_the_capture_plus_padding_on_every_side() {
        let out = compose(&opaque(120, 260), &STYLE);
        let pad = STYLE.padding();
        assert_eq!((out.width(), out.height()), (120 + 2 * pad, 260 + 2 * pad));
    }

    #[test]
    fn screen_interior_is_preserved_exactly() {
        let capture = opaque(120, 260);
        let out = compose(&capture, &STYLE);
        let pad = STYLE.padding();
        assert_eq!(
            out.get_pixel(60 + pad, 130 + pad),
            capture.get_pixel(60, 130)
        );
    }

    #[test]
    fn opaque_capture_gets_rounded_transparent_corners() {
        let out = compose(&opaque(120, 260), &STYLE);
        let pad = STYLE.padding();
        let corner = out.get_pixel(pad, pad);
        assert!(
            corner[3] < 128,
            "screen corner must be cut away, got {corner:?}"
        );
        let edge_mid = out.get_pixel(pad, pad + 130);
        assert_eq!(edge_mid[3], 255, "straight edges stay opaque");
    }

    #[test]
    fn capture_with_its_own_mask_is_not_re_masked() {
        let mut capture = opaque(120, 260);
        capture.put_pixel(0, 0, Rgba([0, 0, 0, 0]));
        // A pixel the fallback radius would have cut, but the capture's own mask keeps.
        capture.put_pixel(2, 2, Rgba([10, 20, 30, 255]));
        let out = compose(&capture, &STYLE);
        let pad = STYLE.padding();
        assert_eq!(*out.get_pixel(2 + pad, 2 + pad), Rgba([10, 20, 30, 255]));
    }

    #[test]
    fn shadow_falls_below_the_screen_and_fades_out() {
        let out = compose(&opaque(120, 260), &STYLE);
        let pad = STYLE.padding();
        let below = out.get_pixel(pad + 60, pad + 260 + STYLE.shadow_offset_y / 2);
        let above = out.get_pixel(pad + 60, pad - STYLE.shadow_offset_y / 2);
        assert!(
            below[3] > above[3],
            "drop shadow is heavier below: {below:?} vs {above:?}"
        );
        assert!(below[3] as f32 <= STYLE.shadow_opacity * 255.0 + 1.0);
        assert_eq!(
            out.get_pixel(0, 0)[3],
            0,
            "canvas corners fully transparent"
        );
        assert_eq!(out.get_pixel(out.width() - 1, out.height() - 1)[3], 0);
    }

    #[test]
    fn output_bytes_are_deterministic() {
        let capture = opaque(90, 180);
        let a = encode_png(&compose(&capture, &STYLE), None).unwrap();
        let b = encode_png(&compose(&capture, &STYLE), None).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn run_carries_the_capture_colour_profile_through() {
        let tmp = tempfile::tempdir().unwrap();
        let (input, output) = (tmp.path().join("raw.png"), tmp.path().join("out/hero.png"));
        let profile = b"stand-in ICC profile bytes".to_vec();
        fs::write(
            &input,
            encode_png(&opaque(40, 80), Some(profile.clone())).unwrap(),
        )
        .unwrap();

        run(&input, &output, &STYLE).unwrap();
        let mut decoder = ImageReader::open(&output)
            .unwrap()
            .with_guessed_format()
            .unwrap()
            .into_decoder()
            .unwrap();
        assert_eq!(decoder.icc_profile().unwrap(), Some(profile));
    }

    #[test]
    fn coverage_is_full_inside_empty_outside_and_partial_on_the_edge() {
        let inside = rounded_rect_coverage(50.0, 50.0, 100.0, 100.0, 20.0);
        assert!((inside - 1.0).abs() < f32::EPSILON, "{inside}");
        let cut_corner = rounded_rect_coverage(0.5, 0.5, 100.0, 100.0, 20.0);
        assert!(cut_corner.abs() < f32::EPSILON, "{cut_corner}");
        let edge = rounded_rect_coverage(0.0, 50.0, 100.0, 100.0, 20.0);
        assert!((edge - 0.5).abs() < 1e-6, "{edge}");
    }
}
