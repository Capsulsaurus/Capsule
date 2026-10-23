use std::fs;

use tempfile::TempDir;

use super::*;

fn create_file(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    fs::write(&p, b"placeholder").unwrap();
    p
}

#[test]
fn test_single_jpeg_standalone() {
    let tmp = TempDir::new().unwrap();
    create_file(tmp.path(), "photo.jpg");
    let result = scan(&[tmp.path().to_path_buf()]).unwrap();
    assert_eq!(result.candidates.len(), 1);
    let c = &result.candidates[0];
    assert!(c.stack_type.is_none());
    assert_eq!(c.detected_type, AssetType::Photo);
}

#[test]
fn test_raw_jpeg_pair() {
    let tmp = TempDir::new().unwrap();
    create_file(tmp.path(), "img_0001.jpg");
    create_file(tmp.path(), "img_0001.ARW");
    let result = scan(&[tmp.path().to_path_buf()]).unwrap();
    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].stack_type, Some(StackType::RawJpeg));
}

#[test]
fn test_raw_only() {
    let tmp = TempDir::new().unwrap();
    create_file(tmp.path(), "img_0002.CR2");
    let result = scan(&[tmp.path().to_path_buf()]).unwrap();
    assert_eq!(result.candidates.len(), 1);
    assert!(result.candidates[0].stack_type.is_none());
}

#[test]
fn test_git_dir_skipped() {
    let tmp = TempDir::new().unwrap();
    let git = tmp.path().join(".git");
    fs::create_dir_all(&git).unwrap();
    create_file(&git, "config");
    create_file(tmp.path(), "photo.jpg");
    let result = scan(&[tmp.path().to_path_buf()]).unwrap();
    assert_eq!(
        result.candidates.len(),
        1,
        "files inside .git should be skipped"
    );
}

#[test]
fn test_xmp_pairs_with_raw() {
    let tmp = TempDir::new().unwrap();
    create_file(tmp.path(), "img.NEF");
    create_file(tmp.path(), "img.xmp");
    let result = scan(&[tmp.path().to_path_buf()]).unwrap();
    assert_eq!(result.candidates.len(), 1);
    let c = &result.candidates[0];
    assert!(c.members.iter().any(|(_, r)| *r == MemberRole::Sidecar));
}

#[test]
fn test_empty_directory() {
    let tmp = TempDir::new().unwrap();
    let result = scan(&[tmp.path().to_path_buf()]).unwrap();
    assert!(result.candidates.is_empty());
}

/// **Every file is kept** (issue #695): an extension Capsule does not recognise, no
/// extension at all, and a zero-byte file are each a standalone candidate, exactly like a
/// recognised video or HEIC. If someone reintroduces an extension gate, this test fails.
#[test]
fn unrecognised_extensionless_and_empty_files_are_candidates() {
    let tmp = TempDir::new().unwrap();
    create_file(tmp.path(), "document.pdf");
    create_file(tmp.path(), "notes.xyz");
    create_file(tmp.path(), "README");
    create_file(tmp.path(), "clip.mkv");
    create_file(tmp.path(), "iphone.heic");
    fs::write(tmp.path().join("empty.dat"), b"").unwrap();

    let result = scan(&[tmp.path().to_path_buf()]).unwrap();

    let mut names: Vec<String> = result
        .candidates
        .iter()
        .flat_map(|c| c.source_paths.iter())
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(
        names,
        [
            "README",
            "clip.mkv",
            "document.pdf",
            "empty.dat",
            "iphone.heic",
            "notes.xyz"
        ]
    );
    assert_eq!(result.candidates.len(), 6, "each is its own candidate");
}

#[test]
fn os_folder_metadata_is_left_out() {
    let tmp = TempDir::new().unwrap();
    create_file(tmp.path(), ".DS_Store");
    create_file(tmp.path(), "._photo.jpg");
    create_file(tmp.path(), "Thumbs.db");
    create_file(tmp.path(), "desktop.ini");
    create_file(tmp.path(), "photo.jpg");

    let result = scan(&[tmp.path().to_path_buf()]).unwrap();

    assert_eq!(result.total_files(), 1);
    assert!(result.candidates[0].primary_path().ends_with("photo.jpg"));
}

/// A source the walk cannot read is reported, not skipped: it reaches the plan as a
/// candidate, where hashing it fails and the run counts it as an error.
#[test]
fn an_unreadable_source_is_a_candidate_not_a_silent_skip() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("not-there");

    let result = scan(std::slice::from_ref(&missing)).unwrap();

    assert_eq!(result.candidates.len(), 1);
    assert_eq!(result.candidates[0].primary_path(), &missing);
}
