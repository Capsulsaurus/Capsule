//! Contract tests for [`crate::lqip`].
//!
//! The list, in the order the contract states it:
//!
//! 1. Tier — an encode is exactly 32 bytes (`DEFAULT_TIER`), never the 21-byte compact tier.
//! 2. Determinism — pinned known-answer vectors, the guard that the CLI, the FFI and
//!    `capsule-wasm` produce identical bytes for identical input.
//! 3. No pre-resize — a known-answer vector for a frame well above the retired 100 px
//!    threshold, which no longer holds if any downscale-before-hash step returns.
//! 4. Input validation — the two shapes `chromahash::encode` *panics* on are checked errors.
//! 5. Gamut — a real encode input, exhaustively mapped, and not recoverable afterwards.
//! 6. Round trip — `from_bytes`/`as_bytes`, and rejection of everything that is not a payload.
//! 7. Band-limited decode — `decode_capped` honours the box it is given.
//! 8. Dominant colour — the DC-only fill matches the source.
//! 9. Versioned fallback — unknown version, corrupt bytes, and a *real ThumbHash payload* all
//!    paint the solid fill rather than misrendering.

use super::{Gamut, LQIP_FORMAT_V1, Lqip, LqipError, RgbaImage, render};

/// A `width` x `height` frame of packed RGBA8, every pixel `rgba`.
fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
    let pixels = (width as usize) * (height as usize);
    let mut frame = Vec::with_capacity(pixels * 4);
    for _ in 0..pixels {
        frame.extend_from_slice(&rgba);
    }
    frame
}

/// A deterministic RGBA gradient — the fixture behind the pinned vectors below.
fn gradient(width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut v = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            v.extend_from_slice(&[
                (x * 255 / w) as u8,
                (y * 255 / h) as u8,
                ((x + y) * 255 / (w + h)) as u8,
                255,
            ]);
        }
    }
    v
}

/// Box-downscale by an integer factor — used only to build the "not what the encoder hashed"
/// comparison frame.
fn box_downscale(width: u32, height: u32, rgba: &[u8], factor: u32) -> (u32, u32, Vec<u8>) {
    let (w, f) = (width as usize, factor as usize);
    let (nw, nh) = ((width / factor) as usize, (height / factor) as usize);
    let mut out = Vec::with_capacity(nw * nh * 4);
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0u32; 4];
            for dy in 0..f {
                for dx in 0..f {
                    let i = ((y * f + dy) * w + (x * f + dx)) * 4;
                    for (c, slot) in acc.iter_mut().enumerate() {
                        *slot += u32::from(rgba[i + c]);
                    }
                }
            }
            for slot in acc {
                out.push((slot / (factor * factor)) as u8);
            }
        }
    }
    (nw as u32, nh as u32, out)
}

/// A **real** ThumbHash payload, produced by the retired `thumbhash::rgba_to_thumb_hash` path
/// before that crate was removed (a 100x75 gradient — the exact shape the old 100 px
/// pre-resize produced).
///
/// It is **21 bytes**, which is precisely `chromahash::COMPACT_TIER`'s length: the concrete
/// evidence for the contract's warning that byte length alone cannot discriminate a stale
/// ThumbHash payload from a valid chromahash one. Only header validation catches it.
const REAL_THUMBHASH_21B: [u8; 21] = [
    223, 7, 10, 53, 154, 128, 135, 135, 112, 135, 135, 135, 136, 120, 136, 136, 128, 128, 8, 248,
    136,
];

/// A second real ThumbHash payload, 24 bytes (a 32x32 solid) — a length no chromahash tier has.
const REAL_THUMBHASH_24B: [u8; 24] = [
    152, 89, 3, 7, 0, 150, 136, 104, 136, 135, 135, 135, 120, 136, 135, 152, 136, 143, 115, 207,
    118, 240, 136, 8,
];

// ── 1. Tier ────────────────────────────────────────────────────────────────

/// The committed tier is `DEFAULT_TIER` — exactly 32 bytes. `COMPACT_TIER` is 21 and is
/// selected by a bare `0`, so this is the guard against silently dropping a tier.
#[test]
fn encode_is_exactly_thirty_two_bytes() {
    for (w, h) in [(1, 1), (8, 4), (64, 64), (200, 133), (3, 4000)] {
        let hash = Lqip::encode(w, h, &gradient(w, h), Gamut::Srgb).expect("valid frame");
        assert_eq!(
            hash.as_bytes().len(),
            32,
            "{w}x{h} encoded to {} bytes, not the 32-byte DEFAULT_TIER",
            hash.as_bytes().len()
        );
    }
}

// ── 2/3. Determinism and the absence of a pre-resize ───────────────────────

/// Pinned known-answer vector for a small frame.
///
/// chromahash is bit-exact across platforms and this module is the single, unconditional
/// implementation linked by the CLI, the uniffi FFI and `capsule-wasm` alike — so pinning the
/// bytes here pins them for every surface. A change to this value is a change to a *signed*
/// sidecar field and must be a deliberate, version-bumped decision.
#[test]
fn encode_matches_the_pinned_vector() {
    let hash = Lqip::encode(8, 4, &gradient(8, 4), Gamut::Srgb).expect("valid frame");
    assert_eq!(
        hex::encode(hash.as_bytes()),
        "089fc462b0f14c0092a19d9999dddddddddddddd1dbc691ab76d332872d3b66d"
    );
}

/// Pinned known-answer vector for a frame **well above** the 100 px long edge the retired
/// ThumbHash path downsized to before hashing.
///
/// This is the guard for that removal: chromahash takes the full frame and band-limits on the
/// read side via `decode_capped`, so reintroducing any downscale-before-hash step would change
/// these bytes and fail here — which is exactly what "carrying the resize forward silently caps
/// fidelity" would otherwise look like (no error, no test failure, just less detail).
#[test]
fn encode_hashes_the_full_frame_with_no_pre_resize() {
    let full = gradient(200, 200);
    let hash = Lqip::encode(200, 200, &full, Gamut::Srgb).expect("valid frame");
    assert_eq!(
        hex::encode(hash.as_bytes()),
        "08804fe1d0e90608169a9dddddddd9dddddddddd9db18d9bb66d831b4ea3b86d"
    );

    // Supporting evidence: the reduced frame is a different placeholder, so the resolution the
    // caller hands in genuinely reaches the format.
    let (rw, rh, reduced) = box_downscale(200, 200, &full, 2);
    let reduced_hash = Lqip::encode(rw, rh, &reduced, Gamut::Srgb).expect("valid frame");
    assert_ne!(hash.as_bytes(), reduced_hash.as_bytes());
}

/// The same input encodes to the same bytes every time — no clock, no RNG, no allocation
/// address leaking into the payload.
#[test]
fn encode_is_deterministic() {
    let rgba = gradient(37, 91);
    let a = Lqip::encode(37, 91, &rgba, Gamut::DisplayP3).expect("valid frame");
    let b = Lqip::encode(37, 91, &rgba, Gamut::DisplayP3).expect("valid frame");
    assert_eq!(a, b);
    assert_eq!(a.as_bytes(), b.as_bytes());
}

// ── 4. Input validation ────────────────────────────────────────────────────

/// `chromahash::encode` panics on a zero dimension. The import pipeline must degrade to "no
/// LQIP" instead, so it is a checked error here.
#[test]
fn encode_rejects_a_zero_dimension() {
    assert_eq!(
        Lqip::encode(0, 4, &[], Gamut::Srgb),
        Err(LqipError::ZeroDimension {
            width: 0,
            height: 4
        })
    );
    assert_eq!(
        Lqip::encode(4, 0, &[], Gamut::Srgb),
        Err(LqipError::ZeroDimension {
            width: 4,
            height: 0
        })
    );
}

/// The other `chromahash::encode` panic: a buffer that is not exactly `w * h * 4`.
#[test]
fn encode_rejects_a_pixel_count_mismatch() {
    let short = solid(4, 4, [1, 2, 3, 4]);
    assert_eq!(
        Lqip::encode(4, 5, &short, Gamut::Srgb),
        Err(LqipError::PixelCountMismatch {
            width: 4,
            height: 5,
            expected: 80,
            actual: 64,
        })
    );
    // An RGB (3-byte) buffer mistaken for RGBA is the realistic form of this bug.
    assert!(matches!(
        Lqip::encode(4, 4, &[0u8; 4 * 4 * 3], Gamut::Srgb),
        Err(LqipError::PixelCountMismatch { .. })
    ));
    // Huge dimensions must not overflow into a spurious match.
    assert!(matches!(
        Lqip::encode(u32::MAX, u32::MAX, &[0u8; 4], Gamut::Srgb),
        Err(LqipError::PixelCountMismatch { .. })
    ));
}

// ── 5. Gamut ───────────────────────────────────────────────────────────────

/// Every Capsule gamut maps onto exactly one chromahash gamut. Exhaustive, so adding a variant
/// on either side fails to compile or fails here rather than silently defaulting.
#[test]
fn gamut_maps_onto_chromahash_exhaustively() {
    let pairs = [
        (Gamut::Srgb, chromahash::Gamut::Srgb),
        (Gamut::DisplayP3, chromahash::Gamut::DisplayP3),
        (Gamut::AdobeRgb, chromahash::Gamut::AdobeRgb),
        (Gamut::Bt2020, chromahash::Gamut::Bt2020),
        (Gamut::ProPhotoRgb, chromahash::Gamut::ProPhotoRgb),
    ];
    for (ours, theirs) in pairs {
        assert_eq!(chromahash::Gamut::from(ours), theirs, "{ours:?}");
    }
    assert_eq!(Gamut::default(), Gamut::Srgb);
}

/// The gamut is a real input, not decoration: the same pixels in a different source space
/// encode to different bytes.
///
/// It is also **not recoverable** afterwards — the sidecar stores the payload, the format
/// version and the fallback colour, and chromahash does not carry the source gamut in the
/// payload. Choosing it wrongly at import is therefore permanent for that sidecar, which is why
/// the mapping is defined once, at the one place a colour space is known.
#[test]
fn gamut_changes_the_encoded_bytes_and_is_not_stored() {
    let rgba = gradient(16, 16);
    let srgb = Lqip::encode(16, 16, &rgba, Gamut::Srgb).expect("valid frame");
    let p3 = Lqip::encode(16, 16, &rgba, Gamut::DisplayP3).expect("valid frame");
    assert_ne!(srgb.as_bytes(), p3.as_bytes());

    // Both are valid payloads of identical width; nothing in either records which was which.
    assert_eq!(srgb.as_bytes().len(), p3.as_bytes().len());
    assert!(Lqip::from_bytes(srgb.as_bytes()).is_ok());
    assert!(Lqip::from_bytes(p3.as_bytes()).is_ok());
}

// ── 6. Round trip and rejection ────────────────────────────────────────────

#[test]
fn from_bytes_round_trips_as_bytes() {
    let hash = Lqip::encode(64, 48, &gradient(64, 48), Gamut::Srgb).expect("valid frame");
    let parsed = Lqip::from_bytes(hash.as_bytes()).expect("own output is valid");
    assert_eq!(parsed, hash);
    assert_eq!(parsed.as_bytes(), hash.as_bytes());
}

/// A genuine ThumbHash payload is not a chromahash payload — including the 21-byte one, whose
/// length matches `COMPACT_TIER` exactly. Header validation is what catches it; length does not.
#[test]
fn from_bytes_rejects_real_thumbhash_payloads() {
    assert_eq!(REAL_THUMBHASH_21B.len(), 21, "the compact-tier byte length");
    assert!(matches!(
        Lqip::from_bytes(&REAL_THUMBHASH_21B),
        Err(LqipError::InvalidHash(_))
    ));
    assert!(matches!(
        Lqip::from_bytes(&REAL_THUMBHASH_24B),
        Err(LqipError::InvalidHash(_))
    ));
}

#[test]
fn from_bytes_rejects_malformed_payloads() {
    for bytes in [
        vec![],
        vec![0u8; 3],
        vec![0xFF; 32],
        vec![0u8; 32],
        vec![0x08; 31],
    ] {
        assert!(
            matches!(Lqip::from_bytes(&bytes), Err(LqipError::InvalidHash(_))),
            "{} bytes were accepted",
            bytes.len()
        );
    }
}

// ── 7. Band-limited decode ─────────────────────────────────────────────────

/// `decode_capped` renders no larger than the box it is given, and the buffer it returns is
/// well-formed RGBA8 for the size it reports.
#[test]
fn decode_capped_honours_the_bound() {
    let hash = Lqip::encode(200, 100, &gradient(200, 100), Gamut::Srgb).expect("valid frame");
    for (max_w, max_h) in [(1, 1), (4, 4), (16, 16), (u32::MAX, u32::MAX)] {
        let image = hash.decode_capped(max_w, max_h);
        assert!(
            image.width <= max_w && image.height <= max_h,
            "{max_w}x{max_h}"
        );
        assert!(image.width >= 1 && image.height >= 1);
        assert_eq!(
            image.rgba.len(),
            (image.width as usize) * (image.height as usize) * 4,
            "buffer is not packed RGBA8 for {}x{}",
            image.width,
            image.height
        );
    }
}

/// A zero bound is raised to 1 rather than returning an empty buffer — every caller of this is
/// about to paint something.
#[test]
fn decode_capped_never_returns_an_empty_image() {
    let hash = Lqip::encode(8, 8, &gradient(8, 8), Gamut::Srgb).expect("valid frame");
    let image = hash.decode_capped(0, 0);
    assert_eq!((image.width, image.height), (1, 1));
    assert_eq!(image.rgba.len(), 4);
}

/// A solid source decodes back to (approximately) that colour.
#[test]
fn decode_capped_reproduces_a_solid_source() {
    let hash =
        Lqip::encode(64, 64, &solid(64, 64, [200, 30, 60, 255]), Gamut::Srgb).expect("valid frame");
    let image = hash.decode_capped(8, 8);
    for px in image.rgba.chunks_exact(4) {
        assert!(px[0].abs_diff(200) < 8, "red {}", px[0]);
        assert!(px[1].abs_diff(30) < 8, "green {}", px[1]);
        assert!(px[2].abs_diff(60) < 8, "blue {}", px[2]);
        assert_eq!(px[3], 255);
    }
}

// ── 8. Dominant colour ─────────────────────────────────────────────────────

#[test]
fn dominant_color_matches_a_solid_source() {
    let hash =
        Lqip::encode(32, 32, &solid(32, 32, [220, 20, 30, 255]), Gamut::Srgb).expect("valid frame");
    let [r, g, b] = hash.dominant_color();
    assert!(r > g && r > b, "dominant colour {r},{g},{b} should be red");
    assert!(r.abs_diff(220) < 8 && g.abs_diff(20) < 8 && b.abs_diff(30) < 8);
    assert_eq!(hash.average_rgba(), [r, g, b, 255]);
}

#[test]
fn average_rgba_carries_alpha() {
    let hash =
        Lqip::encode(32, 32, &solid(32, 32, [0, 255, 0, 128]), Gamut::Srgb).expect("valid frame");
    let alpha = hash.average_rgba()[3];
    assert!(alpha.abs_diff(128) < 16, "alpha {alpha} not near 128");
}

// ── 9. Versioned fallback ──────────────────────────────────────────────────

#[test]
fn render_decodes_a_recognized_version() {
    let hash = Lqip::encode(80, 40, &gradient(80, 40), Gamut::Srgb).expect("valid frame");
    let image = render(LQIP_FORMAT_V1, hash.as_bytes(), [12, 34, 56], 16, 16);
    assert!(
        image.width > 1 && image.height > 1,
        "should be a real decode"
    );
    assert_eq!(
        image.rgba.len(),
        (image.width as usize) * (image.height as usize) * 4
    );
    assert_eq!(image, hash.decode_capped(16, 16));
}

/// An unknown future format version must not be handed to this build's decoder.
#[test]
fn render_falls_back_on_an_unrecognized_version() {
    let hash = Lqip::encode(80, 40, &gradient(80, 40), Gamut::Srgb).expect("valid frame");
    let image = render(LQIP_FORMAT_V1 + 1, hash.as_bytes(), [12, 34, 56], 16, 16);
    assert_eq!(image, RgbaImage::solid([12, 34, 56]));
}

/// Corrupt bytes under a recognized version fall back too — `from_bytes` is the gate.
#[test]
fn render_falls_back_on_an_undecodable_payload() {
    let image = render(LQIP_FORMAT_V1, &[0xDE, 0xAD, 0xBE, 0xEF], [7, 8, 9], 16, 16);
    assert_eq!(image, RgbaImage::solid([7, 8, 9]));
}

/// The migration guard: a stale ThumbHash payload sitting under `format_version = 1` paints the
/// solid `dominant_color` fill, never noise. This is what makes the "no `sidecar_schema` bump"
/// decision safe to hold even if the totality assumption were ever violated in the field.
#[test]
fn render_falls_back_on_a_stale_thumbhash_payload() {
    for stale in [REAL_THUMBHASH_21B.as_slice(), REAL_THUMBHASH_24B.as_slice()] {
        let image = render(LQIP_FORMAT_V1, stale, [90, 91, 92], 32, 32);
        assert_eq!(
            image,
            RgbaImage::solid([90, 91, 92]),
            "a stale ThumbHash payload rendered instead of falling back"
        );
    }
}
