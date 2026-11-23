//! Real camera RAW files through the import path's decode (issue #1324's acceptance: "a real DNG
//! and ARW still thumbnail").
//!
//! The pre-open bounds in `capsule_core::media` refuse a RAW whose directories, values, strips and
//! tiles exceed twice its length, or which follows more than 256 SubIFD, EXIF and GPS pointer
//! elements. The unit fixtures prove each refusal on synthetic 16x8 files; only a file a camera
//! wrote proves the limits leave room for a real one. Camera files are tens of MB and carry their
//! makers' licences, so none is committed: this case reads one DNG and one ARW from the paths in
//! `CAPSULE_TEST_DNG` and `CAPSULE_TEST_ARW` and is ignored unless asked for:
//!
//! ```sh
//! CAPSULE_TEST_DNG=/path/to/camera.dng CAPSULE_TEST_ARW=/path/to/camera.arw \
//!   cargo test -p capsule-core --test raw_camera_files -- --ignored
//! ```
//!
//! For each file it runs what the import path runs before a thumbnail is encoded: probe, the
//! guarded full decode, the downscale to the thumbnail tier's long edge, and the embedded-preview
//! read and decode. A file whose raw image uses a variant the reader has no codec for (an ARW in
//! Sony's lossy compression, 32767) defers its full decode as `UnsupportedFormat`, and then its
//! embedded preview must thumbnail instead; a pre-open refusal (`MediaError::Decode`) fails.
//! Run with `--nocapture` to see which path each file took.

use std::path::PathBuf;

use capsule_core::media::{
    DecodedImage, Decoder as _, DerivativeTier, MediaError, RawshiftDecoder, StillFormat,
    decode_embedded_preview, decode_guarded, downscale_rgba8, extract_embedded_preview,
};

/// The file `var` names. Panics rather than skipping, so `--ignored` without the files fails
/// instead of passing on nothing.
fn camera_file(var: &str) -> (PathBuf, Vec<u8>) {
    let path = std::env::var_os(var)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("set {var} to a camera-written file"));
    let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    (path, bytes)
}

fn assert_thumbnails(var: &str, ext: &str, format: StillFormat) {
    let (path, bytes) = camera_file(var);
    let at = path.display();

    let probed = RawshiftDecoder
        .probe(&bytes, ext)
        .unwrap_or_else(|e| panic!("{at}: probe: {e}"));
    assert_eq!(probed.format, format, "{at}");

    let cap = DerivativeTier::Thumbnail
        .max_long_edge()
        .expect("the thumbnail tier caps its long edge");
    let thumbnail = |frame: &DecodedImage, source: &str| {
        let small = downscale_rgba8(&frame.image, cap);
        assert_eq!(small.width.max(small.height), cap, "{at}: {source}");
        eprintln!(
            "{at}: {source} {}x{} -> thumbnail {}x{}",
            frame.width(),
            frame.height(),
            small.width,
            small.height
        );
    };

    // A pre-open refusal is `MediaError::Decode`; a variant the reader has no codec for is
    // `UnsupportedFormat` and is deferred, never refused. Only the second is a pass.
    let decoded = decode_guarded(&RawshiftDecoder, &bytes, ext);
    match &decoded {
        Ok(frame) => {
            assert_eq!(
                (frame.width(), frame.height()),
                probed.upright_dimensions(),
                "{at}: the decoded frame is the size the probe read"
            );
            thumbnail(frame, "decoded");
        }
        Err(MediaError::UnsupportedFormat { .. }) => {
            eprintln!("{at}: no codec for its raw image here; deferred");
        }
        Err(e) => panic!("{at}: decode: {e}"),
    }

    // The preview read opens the file behind the same bounds. Where the full decode deferred,
    // the embedded preview is the file's thumbnail, so it must be there.
    let preview =
        extract_embedded_preview(&bytes, ext).unwrap_or_else(|e| panic!("{at}: preview read: {e}"));
    match preview {
        Some(preview) => {
            let frame =
                decode_embedded_preview(&preview).unwrap_or_else(|e| panic!("{at}: preview: {e}"));
            thumbnail(&frame, "embedded preview");
        }
        None => assert!(
            decoded.is_ok(),
            "{at}: neither decodes nor carries a preview"
        ),
    }
}

#[test]
#[ignore = "needs camera files: CAPSULE_TEST_DNG=… CAPSULE_TEST_ARW=… cargo test -p capsule-core \
            --test raw_camera_files -- --ignored"]
fn a_camera_dng_and_arw_reach_a_thumbnail() {
    assert_thumbnails("CAPSULE_TEST_DNG", "dng", StillFormat::Dng);
    assert_thumbnails("CAPSULE_TEST_ARW", "arw", StillFormat::Arw);
}
