use super::*;

#[test]
fn tmp_path_appends_the_suffix_without_touching_the_extension() {
    // Not `set_extension`: a sidecar is `{uuid}.cbor`, and its temp file must be
    // `{uuid}.cbor.tmp`, never `{uuid}.tmp` — otherwise two different sidecars for the
    // same asset would collide on one temp path.
    assert_eq!(
        tmp_path(Path::new("/a/b/c.cbor")),
        PathBuf::from("/a/b/c.cbor.tmp")
    );
}

#[test]
fn tmp_path_handles_a_path_with_no_extension() {
    assert_eq!(tmp_path(Path::new("/a/b/c")), PathBuf::from("/a/b/c.tmp"));
}
