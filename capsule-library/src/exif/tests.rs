use super::*;

#[test]
fn test_extract_exif_nonexistent_file_returns_io_error() {
    let result = extract_exif(Path::new("/nonexistent/path/to/file.jpg"));
    assert!(result.is_err());
}

#[test]
fn a_file_without_exif_extracts_to_nothing() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let path = dir.path().join("plain.bin");
    std::fs::write(&path, b"not an image").expect("write fixture");

    assert_eq!(
        extract_exif(&path).expect("a readable file"),
        ExifExtract::default()
    );
}
