pub const IGNORE_RULES: &[&str] = &[
    // Ignore hidden files and directories
    ".*",
    // Ignore system files
    "*.DS_Store",
];

pub const SIDECAR_EXTENSIONS: &[&str] = &[
    // XMP
    "xmp",  // Custom formats
    "json", // Commonly used to append metadata to media files
    "xml",
];
