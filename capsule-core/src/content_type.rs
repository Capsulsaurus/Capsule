//! The file extension an original is named by for its sidecar `content_type`.
//!
//! One table for every writer that names an original from its media type rather than from the
//! file it was imported from: the library adopting a synced original into `media/`, and a
//! restore writing one out. Both name an original alike, so a library and a restore of it agree
//! on a file's suffix. The name is cosmetic — the bytes are the signed original whatever it
//! says — and an unlisted type is `bin`.
//!
//! The table covers every media type the import path writes into a sidecar:
//! [`StillFormat::mime`](crate::media::StillFormat::mime) for a still whose header named its
//! format, and the video suffix fallback for the rest. Each extension is the one the Apple
//! lane's file-type table prefers for the type (`UTType.preferredFilenameExtension`), so an
//! original an app wrote before the workspace adopted synced assets carries the same name.

/// The extension an original of media type `content_type` is named by, without the dot.
/// `bin` for a type the table does not list.
#[must_use]
pub fn extension_for(content_type: &str) -> &'static str {
    match content_type {
        "image/jpeg" => "jpeg",
        "image/png" => "png",
        "image/gif" => "gif",
        "image/tiff" => "tiff",
        "image/webp" => "webp",
        "image/heic" => "heic",
        "image/heif" => "heif",
        "image/avif" => "avif",
        "image/jxl" => "jxl",
        "image/x-portable-anymap" => "pnm",
        "image/x-adobe-dng" | "image/dng" => "dng",
        "image/x-sony-arw" => "arw",
        "image/x-canon-cr2" => "cr2",
        "image/x-canon-cr3" => "cr3",
        "image/x-canon-crw" => "crw",
        "image/x-nikon-nef" => "nef",
        "image/x-fuji-raf" => "raf",
        "image/x-olympus-orf" => "orf",
        "image/x-panasonic-rw2" => "rw2",
        "video/mp4" => "mp4",
        "video/quicktime" => "mov",
        "video/x-matroska" => "mkv",
        "video/webm" => "webm",
        "video/x-msvideo" => "avi",
        _ => "bin",
    }
}

#[cfg(test)]
mod tests;
