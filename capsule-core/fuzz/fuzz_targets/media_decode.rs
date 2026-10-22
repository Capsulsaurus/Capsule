//! A still image from an import source: sniffing, probing and decoding never panic. The
//! decoder is called directly rather than through `decode_guarded`, so a panic the import path
//! would only have caught and reported surfaces here as a finding. The first byte picks the
//! extension the importer would have passed.
#![no_main]

use capsule_core::media::{Decoder, RawshiftDecoder, StillFormat};
use libfuzzer_sys::fuzz_target;

const EXTENSIONS: [&str; 8] = ["jpg", "png", "jxl", "tif", "gif", "arw", "dng", "avif"];

fuzz_target!(|data: &[u8]| {
    let Some((&pick, bytes)) = data.split_first() else {
        return;
    };
    let ext = EXTENSIONS[usize::from(pick) % EXTENSIONS.len()];
    let _ = StillFormat::from_bytes(bytes);
    if RawshiftDecoder.probe(bytes, ext).is_ok() {
        let _ = RawshiftDecoder.decode(bytes, ext);
    }
});
