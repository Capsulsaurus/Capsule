//! Example of generating a JpegXL derivative from an image file
//!
//! This example demonstrates how to use the `capsule_media` crate to read an image file,
//! detect its format, and generate a JpegXL derivative.
//!
//! Usage:
//! ```
//! cargo run --example generate_derivative <input_image> [output_image]
//! ```

use std::path::PathBuf;

use capsule_media::{
    fs::MediaFile,
    image::{ConvertImage, ImageEncode, formats::jxl::JxlImage},
};

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
        input_path.with_extension("jxl")
    };

    let media = capsule_media::fs::read(&input_path)
        .await
        .expect("Failed to create reader");
    let MediaFile::Image(file) = media else {
        eprintln!("File is not an image: {media:?}");
        return;
    };
    let image = file.image;
    let format = image.get_format();
    println!("Loaded image as format: {format:?}");

    let jxl = JxlImage::convert_from_boxed(image).expect("Failed to create JXL image");
    println!("Created JXL image");

    jxl.save(&output_path)
        .await
        .unwrap_or_else(|_| panic!("Failed to save JXL to: {output_path:?}"));
    println!("Saved JXL derivative to: {output_path:?}");
}
