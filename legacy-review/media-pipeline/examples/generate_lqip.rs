//! Print the chromahash LQIP of a JPEG: the payload, its dominant-colour fallback, and the
//! band-limited placeholder the sidecar reader would paint.

use std::path::PathBuf;

use capsule_core::media::image::formats::jpeg::JpegImage;
use capsule_core::media::image::lqip::{LQIP, gamut_for};
use capsule_core::media::image::{Image, ImageReader};

#[tokio::main]
pub async fn main() {
    let image_path = PathBuf::from("./data/test.jpg");
    println!("Image path: {}", image_path.display());
    let image: Box<dyn Image> = Box::new(
        JpegImage::from_path(&image_path)
            .await
            .expect("Failed to load image"),
    );
    let buffer = image.get_buffer().to_rgba8().expect("RGBA8 conversion");
    let lqip = LQIP::from_rgba_buffer(&buffer).expect("Failed to generate LQIP");

    println!("Source gamut: {:?}", gamut_for(buffer.color_space));
    println!(
        "LQIP ({} bytes): {:02x?}",
        lqip.as_bytes().len(),
        lqip.as_bytes()
    );
    println!("Average RGBA: {:?}", lqip.hash().average_rgba());
    println!("Dominant color: {:?}", lqip.hash().dominant_color());

    // What a grid cell actually paints: a band-limited decode of the box being drawn.
    let placeholder = lqip.hash().decode_capped(32, 32);
    println!(
        "Placeholder: {}x{} ({} bytes RGBA)",
        placeholder.width,
        placeholder.height,
        placeholder.rgba.len()
    );
}
