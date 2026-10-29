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
        object_path(root, &object(7, 0x20, "DSC00001.ARW"), 0),
        Path::new("/stage/objects/00010001/00000020/DSC00001.ARW")
    );
}

#[test]
fn later_generations_get_their_own_folder_directory() {
    let root = Path::new("/stage");
    let shot = object(7, 0x20, "DSC00001.ARW");
    assert_eq!(
        object_path(root, &shot, 2),
        Path::new("/stage/objects/00010001/00000020~2/DSC00001.ARW")
    );
    assert_eq!(
        identity_path(root, &shot, 2),
        Path::new("/stage/identity/00010001/00000020~2/DSC00001.ARW.id")
    );
    assert!(!identity_path(root, &shot, 0).starts_with(objects_dir(root)));
}

#[test]
fn identity_ignores_the_handle_but_not_the_capture_date() {
    let original = object(7, 0x20, "DSC00001.ARW");
    let renumbered = object(9, 0x20, "DSC00001.ARW");
    assert_eq!(identity_key(&original), identity_key(&renumbered));
    let mut reshot = original.clone();
    reshot.captured_at = Some(jiff::civil::date(2026, 1, 2).at(3, 4, 5, 0));
    assert_ne!(identity_key(&original), identity_key(&reshot));
}

#[test]
fn raw_and_jpeg_of_one_shot_share_a_directory() {
    let root = Path::new("/stage");
    let raw = object_path(root, &object(7, 0x20, "DSC00001.ARW"), 0);
    let jpeg = object_path(root, &object(8, 0x20, "DSC00001.JPG"), 0);
    assert_eq!(raw.parent(), jpeg.parent());
}

#[test]
fn same_name_in_two_folders_does_not_collide() {
    let root = Path::new("/stage");
    assert_ne!(
        object_path(root, &object(7, 0x20, "DSC00001.JPG"), 0),
        object_path(root, &object(9, 0x21, "DSC00001.JPG"), 0)
    );
}

#[test]
fn names_invalid_on_windows_are_repaired_and_keep_their_extension() {
    for (name, repaired) in [
        ("a:b.JPG", "a_b.JPG"),
        ("what?*.ARW", "what__.ARW"),
        ("\"<x>|\".MP4", "__x___.MP4"),
        ("trailing.", "trailing_"),
        ("spaced. .", "spaced___"),
        ("CON", "_CON"),
        ("con.jpg", "_con.jpg"),
        ("Aux.tar.gz", "_Aux.tar.gz"),
        ("COM1.ARW", "_COM1.ARW"),
        ("lpt9", "_lpt9"),
        ("COM\u{b9}.JPG", "_COM\u{b9}.JPG"),
    ] {
        assert_eq!(safe_file_name(name).as_deref(), Some(repaired), "{name:?}");
    }
    for unchanged in [
        "DSC00001.JPG",
        "CONSOLE.JPG",
        "COM10.JPG",
        "LPT0",
        ".hidden",
    ] {
        assert!(
            matches!(safe_file_name(unchanged), Some(Cow::Borrowed(n)) if n == unchanged),
            "{unchanged:?}"
        );
    }
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
            object_path(root, &object(0xAB, 1, name), 0),
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
    let checkpoint = checkpoint_path(&partial);
    assert!(checkpoint.starts_with(partial_dir(root)));
    assert_ne!(checkpoint, partial);
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
