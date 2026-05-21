use std::path::PathBuf;

use capsule_media::image::{Image, ImageReader, formats::jpeg::JpegImage, lqip::LQIP};

#[tokio::main]
pub async fn main() {
    let image_path = PathBuf::from("./data/test.jpg");
    println!("Image path: {}", image_path.display());
    let image: Box<dyn Image> = Box::new(
        JpegImage::from_path(&image_path)
            .await
            .expect("Failed to load image"),
    );
    let buffer = image.get_buffer();
    let lqip = LQIP::from_image_buffer(&buffer)
        .await
        .expect("Failed to generate LQIP");
    let lqip_as_hex: String = lqip
        .as_bytes()
        .iter()
        .map(|b| format!("{:02x}", b))
        .collect::<Vec<String>>()
        .join(" ");
    println!("LQIP: {}", lqip_as_hex);
    println!("Average color: {:?}", lqip.average_rgba().unwrap());
    println!(
        "Approximate aspect ratio: {:?}",
        lqip.approx_aspect_ratio().unwrap()
    );
}
