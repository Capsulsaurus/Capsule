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
