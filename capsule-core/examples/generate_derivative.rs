//! Example of generating a derivative image from an image file
//!
//! This example demonstrates how to use the `capsule_core::media` module (enable the `media`
//! feature) to read an image file, detect its format, and generate a derivative.
//!
//! It also demonstrates the codec-coverage contract from slice `S-B13`: only the formats in
//! `SUPPORTED_IMAGE_FORMATS` have a real codec, and asking for any other one returns a typed
//! `ImageError::UnsupportedFormat` that callers can propagate — it never panics the process.
//! JPEG XL is the deferred format shown here; PNG is the one that actually gets written.
//!
//! Usage:
//! ```
//! cargo run --example generate_derivative <input_image> [output_image]
//! ```

use std::path::PathBuf;

use capsule_core::media::fs::MediaFile;
use capsule_core::media::image::formats::jxl::JxlImage;
use capsule_core::media::image::formats::png::PngImage;
use capsule_core::media::image::types::SUPPORTED_IMAGE_FORMATS;
use capsule_core::media::image::{ConvertImage, Image, ImageEncode};

#[tokio::main]
pub async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <input_image> [output_image]", args[0]);
        return;
    }

    let input_path = PathBuf::from(&args[1]);
    let output_path = if args.len() > 2 {
        PathBuf::from(&args[2])
    } else {
        input_path.with_extension("png")
    };

    let media = capsule_core::media::fs::read(&input_path)
        .await
        .expect("Failed to create reader");
    let MediaFile::Image(file) = media else {
        eprintln!("File is not an image: {media:?}");
        return;
    };
    let image = file.image;
    let format = image.get_format();
    println!("Loaded image as format: {format:?}");
    println!("Formats with a codec in this build: {SUPPORTED_IMAGE_FORMATS:?}");

    // Deferred-codec demo: JPEG XL has no encoder in this build, so building one from an
    // already-decoded buffer + metadata is a *value*, not a panic. Nothing is unwrapped here.
    match JxlImage::from_raw_parts(image.get_buffer(), image.get_metadata()) {
        Ok(_) => println!("JXL encoder is available in this build"),
        Err(e) => println!("JXL derivative deferred: {e}"),
    }

    // The supported path: PNG has a real encoder, so this one is actually written out.
    let png = PngImage::convert_from_boxed(image).expect("Failed to create PNG image");
    println!("Created PNG image");

    png.save(&output_path)
        .await
        .unwrap_or_else(|e| panic!("Failed to save PNG to {}: {e}", output_path.display()));
    println!("Saved PNG derivative to: {}", output_path.display());
}
