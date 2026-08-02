//! Extract metadata from an image
//!
//! This example demonstrates how to use the `capsule_media` crate to read an image file
//! and extract its metadata, including EXIF, XMP, IPTC, and technical details.
//!
//! Usage:
//! ```
//! cargo run --example extract_metadata <input_image>
//! ```

use std::path::PathBuf;

use capsule_media::fs::MediaFile;

#[tokio::main]
pub async fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: {} <input_image>", args[0]);
        return;
    }

    let input_path = PathBuf::from(&args[1]);

    // Read the media file. This automatically detects the format and decodes it.
    let media = capsule_media::fs::read(&input_path)
        .await
        .expect("Failed to read media file");

    let MediaFile::Image(file) = media else {
        eprintln!("File is not an image: {media:?}");
        return;
    };

    let image = file.image;

    // Extract metadata using the ImageMetadataProvider trait
    let metadata = image.get_metadata();

    println!("{:#?}", metadata);
}
