use super::*;

#[test]
fn listed_types_take_their_preferred_extension() {
    assert_eq!(extension_for("image/jpeg"), "jpeg");
    assert_eq!(extension_for("image/x-portable-anymap"), "pnm");
    assert_eq!(extension_for("image/dng"), "dng");
    assert_eq!(extension_for("image/x-adobe-dng"), "dng");
    assert_eq!(extension_for("video/quicktime"), "mov");
}

#[test]
fn an_unlisted_type_is_bin() {
    assert_eq!(extension_for("application/octet-stream"), "bin");
    assert_eq!(extension_for("application/x-unknown"), "bin");
    assert_eq!(extension_for(""), "bin");
}

/// Every media type a sniffed still is written with names an extension. The `match` is
/// exhaustive so a new [`StillFormat`](crate::media::StillFormat) variant fails to compile here
/// until it is listed, and so until the table is checked for it.
#[cfg(feature = "media")]
#[test]
fn every_still_format_the_import_path_writes_has_an_extension() {
    use crate::media::StillFormat;

    const fn listed(format: StillFormat) -> StillFormat {
        match format {
            StillFormat::Jpeg
            | StillFormat::Png
            | StillFormat::WebP
            | StillFormat::Jxl
            | StillFormat::Tiff
            | StillFormat::Gif
            | StillFormat::Ppm
            | StillFormat::Avif
            | StillFormat::Heic
            | StillFormat::Arw
            | StillFormat::Cr2
            | StillFormat::Cr3
            | StillFormat::Crw
            | StillFormat::Dng
            | StillFormat::Nef
            | StillFormat::Raf => format,
        }
    }
    for format in [
        StillFormat::Jpeg,
        StillFormat::Png,
        StillFormat::WebP,
        StillFormat::Jxl,
        StillFormat::Tiff,
        StillFormat::Gif,
        StillFormat::Ppm,
        StillFormat::Avif,
        StillFormat::Heic,
        StillFormat::Arw,
        StillFormat::Cr2,
        StillFormat::Cr3,
        StillFormat::Crw,
        StillFormat::Dng,
        StillFormat::Nef,
        StillFormat::Raf,
    ] {
        let mime = listed(format).mime();
        assert_ne!(extension_for(mime), "bin", "{format:?} ({mime})");
    }
}
