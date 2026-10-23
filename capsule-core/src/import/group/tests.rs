use super::*;
use crate::domain::{MemberRole, StackType};

fn paths(names: &[&str]) -> Vec<PathBuf> {
    names.iter().map(PathBuf::from).collect()
}

#[test]
fn test_raw_jpeg_pair() {
    let files = paths(&["/photos/img_0001.jpg", "/photos/img_0001.ARW"]);
    let candidates = group_by_stem(&files);
    assert_eq!(candidates.len(), 1);
    let c = &candidates[0];
    assert_eq!(c.stack_type, Some(StackType::RawJpeg));
    assert!(c.members.iter().any(|(_, r)| *r == MemberRole::Primary));
    assert!(c.members.iter().any(|(_, r)| *r == MemberRole::Raw));
}

#[test]
fn test_raw_only() {
    let files = paths(&["/photos/img_0002.CR2"]);
    let candidates = group_by_stem(&files);
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].stack_type.is_none());
    assert_eq!(candidates[0].detected_type, AssetType::Photo);
    assert!(
        candidates[0]
            .members
            .iter()
            .any(|(_, r)| *r == MemberRole::Primary)
    );
}

#[test]
fn test_jpeg_only() {
    let files = paths(&["/photos/img_0003.jpg"]);
    let candidates = group_by_stem(&files);
    assert_eq!(candidates.len(), 1);
    assert!(candidates[0].stack_type.is_none());
}

#[test]
fn test_xmp_with_raw() {
    let files = paths(&["/photos/img.ARW", "/photos/img.xmp"]);
    let candidates = group_by_stem(&files);
    assert_eq!(candidates.len(), 1);
    // RAW-only (xmp pairs as sidecar)
    assert!(
        candidates[0]
            .members
            .iter()
            .any(|(_, r)| *r == MemberRole::Sidecar)
    );
    assert!(
        candidates[0]
            .members
            .iter()
            .any(|(_, r)| *r == MemberRole::Primary)
    );
}

#[test]
fn test_xmp_with_raw_and_jpeg() {
    let files = paths(&["/photos/img.ARW", "/photos/img.jpg", "/photos/img.xmp"]);
    let candidates = group_by_stem(&files);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].stack_type, Some(StackType::RawJpeg));
    assert!(
        candidates[0]
            .members
            .iter()
            .any(|(_, r)| *r == MemberRole::Sidecar)
    );
}

#[test]
fn test_xmp_standalone() {
    let files = paths(&["/photos/img.xmp"]);
    let candidates = group_by_stem(&files);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].detected_type, AssetType::Sidecar);
}

#[test]
fn test_multiple_independent_files() {
    let files = paths(&["/photos/a.jpg", "/photos/b.jpg"]);
    let candidates = group_by_stem(&files);
    assert_eq!(candidates.len(), 2);
}

#[test]
fn test_different_dirs_not_grouped() {
    let files = paths(&["/a/img.jpg", "/b/img.ARW"]);
    let candidates = group_by_stem(&files);
    assert_eq!(
        candidates.len(),
        2,
        "files in different dirs must not be grouped"
    );
}

#[test]
fn test_video_standalone() {
    let files = paths(&["/photos/clip.MOV"]);
    let candidates = group_by_stem(&files);
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].detected_type, AssetType::Video);
}

/// Every input path appears in exactly one candidate, sorted for comparison.
fn grouped_paths(candidates: &[ImportCandidate]) -> Vec<PathBuf> {
    let mut all: Vec<PathBuf> = candidates
        .iter()
        .flat_map(|c| c.source_paths.iter().cloned())
        .collect();
    all.sort();
    all
}

/// Issue #695: an unpaired same-stem still and video are two originals. Neither is dropped,
/// whichever still kind (JPEG, RAW, RAW+JPEG) shares the video's stem.
#[test]
fn a_video_beside_a_same_stem_still_is_its_own_candidate() {
    for names in [
        &["/p/IMG_0001.JPG", "/p/IMG_0001.MOV"][..],
        &["/p/IMG_0002.ARW", "/p/IMG_0002.MP4"][..],
        &["/p/IMG_0003.jpg", "/p/IMG_0003.ARW", "/p/IMG_0003.mov"][..],
    ] {
        let files = paths(names);
        let candidates = group_by_stem(&files);
        let mut expected = files.clone();
        expected.sort();
        assert_eq!(grouped_paths(&candidates), expected, "for {names:?}");
        let video = candidates
            .iter()
            .find(|c| c.detected_type == AssetType::Video)
            .expect("the video is a candidate");
        assert_eq!(video.source_paths.len(), 1, "standalone, for {names:?}");
    }
}

/// Issue #695: an XMP beside a lone video has no still to ride with, so it is kept as its
/// own candidate rather than dropped.
#[test]
fn an_xmp_beside_a_lone_video_is_its_own_candidate() {
    let files = paths(&["/p/clip.MOV", "/p/clip.xmp"]);
    let candidates = group_by_stem(&files);
    assert_eq!(candidates.len(), 2);
    let mut expected = files.clone();
    expected.sort();
    assert_eq!(grouped_paths(&candidates), expected);
    assert!(
        candidates.iter().any(
            |c| c.detected_type == AssetType::Sidecar && c.primary_path().ends_with("clip.xmp")
        )
    );
}

/// Issue #788: an XMP beside several unpaired same-stem stills lands in exactly one
/// candidate, the first still in path order, whatever order the files were listed in.
#[test]
fn an_xmp_beside_several_stills_rides_with_the_first_only() {
    for (names, owner) in [
        (
            &["/p/IMG.jpg", "/p/IMG.heic", "/p/IMG.xmp"][..],
            "/p/IMG.heic",
        ),
        (
            &["/p/IMG.xmp", "/p/IMG.heic", "/p/IMG.jpg"][..],
            "/p/IMG.heic",
        ),
        (
            &["/p/IMG.NEF", "/p/IMG.CR2", "/p/IMG.xmp"][..],
            "/p/IMG.CR2",
        ),
    ] {
        let files = paths(names);
        let candidates = group_by_stem(&files);
        let mut expected = files.clone();
        expected.sort();
        assert_eq!(
            grouped_paths(&candidates),
            expected,
            "each file exactly once, for {names:?}"
        );
        let carrying: Vec<&ImportCandidate> = candidates
            .iter()
            .filter(|c| c.source_paths.iter().any(|p| p.ends_with("IMG.xmp")))
            .collect();
        assert_eq!(carrying.len(), 1, "one carrier, for {names:?}");
        assert_eq!(
            carrying[0].primary_path(),
            PathBuf::from(owner).as_path(),
            "for {names:?}"
        );
        assert!(
            carrying[0]
                .members
                .iter()
                .any(|(p, r)| p.ends_with("IMG.xmp") && *r == MemberRole::Sidecar)
        );
    }
}
