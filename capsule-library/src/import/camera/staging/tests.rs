use super::*;

fn object(handle: u32, parent: u32, filename: &str) -> CameraObject {
    CameraObject {
        storage: 0x0001_0001,
        handle,
        parent,
        filename: filename.into(),
        size: Some(10),
        captured_at: None,
        is_folder: false,
    }
}

#[test]
fn objects_keep_their_camera_name_under_storage_and_folder() {
    let root = Path::new("/stage");
    assert_eq!(
        object_path(root, &object(7, 0x20, "DSC00001.ARW")),
        Path::new("/stage/objects/00010001/00000020/DSC00001.ARW")
    );
}

#[test]
fn raw_and_jpeg_of_one_shot_share_a_directory() {
    let root = Path::new("/stage");
    let raw = object_path(root, &object(7, 0x20, "DSC00001.ARW"));
    let jpeg = object_path(root, &object(8, 0x20, "DSC00001.JPG"));
    assert_eq!(raw.parent(), jpeg.parent());
}

#[test]
fn same_name_in_two_folders_does_not_collide() {
    let root = Path::new("/stage");
    assert_ne!(
        object_path(root, &object(7, 0x20, "DSC00001.JPG")),
        object_path(root, &object(9, 0x21, "DSC00001.JPG"))
    );
}

#[test]
fn hostile_names_fall_back_to_the_handle() {
    let root = Path::new("/stage");
    for name in [
        "",
        "  ",
        ".",
        "..",
        "../../etc/passwd",
        "a\\b",
        "a\u{0}b",
        "line\nbreak",
    ] {
        assert_eq!(
            object_path(root, &object(0xAB, 1, name)),
            Path::new("/stage/objects/00010001/00000001/object-000000ab"),
            "{name:?}"
        );
    }
    assert!(safe_file_name(&"x".repeat(256)).is_none());
}

#[test]
fn partial_files_live_outside_the_scanned_tree() {
    let root = Path::new("/stage");
    let partial = partial_path(root, &object(7, 0x20, "DSC00001.ARW"));
    assert!(partial.starts_with(partial_dir(root)));
    assert!(!partial.starts_with(objects_dir(root)));
}

#[test]
fn partial_key_changes_when_the_handle_names_another_object() {
    let root = Path::new("/stage");
    let original = object(7, 0x20, "DSC00001.ARW");
    let mut reassigned = original.clone();
    reassigned.filename = "DSC00002.ARW".into();
    assert_ne!(
        partial_path(root, &original),
        partial_path(root, &reassigned)
    );
    let mut resized = original.clone();
    resized.size = Some(11);
    assert_ne!(partial_path(root, &original), partial_path(root, &resized));
    assert_eq!(
        partial_path(root, &original),
        partial_path(root, &original.clone())
    );
}
