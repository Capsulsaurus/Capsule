use std::ffi::OsStr;

use uuid::Uuid;

use super::*;

fn test_uuid() -> Uuid {
    // Use a fixed UUID for deterministic tests
    Uuid::parse_str("01956ef3-1234-7abc-9def-123456789abc").unwrap()
}

#[test]
fn test_uuid_shard() {
    let uuid = test_uuid();
    let (s1, s2) = uuid_shard(&uuid);
    // simple() gives 32 hex chars; first 2 and next 2
    let hex = uuid.simple().to_string();
    assert_eq!(s1, &hex[0..2]);
    assert_eq!(s2, &hex[2..4]);
    assert_eq!(s1.len(), 2);
    assert_eq!(s2.len(), 2);
}

#[test]
fn test_media_dir() {
    let root = Path::new("/lib");
    let path = media_dir(root, 2024, 7);
    assert_eq!(path, PathBuf::from("/lib/media/2024/2024-07"));
}

#[test]
fn test_media_path_with_capture_utc() {
    let root = Path::new("/lib");
    let uuid = test_uuid();
    // 2024-07-15 -> ts = 1721001600 (approx)
    let ts = 1_721_001_600_i64;
    let path = media_path(root, &uuid, "jpg", Some(ts));
    let uuid_str = uuid.simple().to_string();
    assert!(path.to_str().unwrap().contains("media/2024/2024-07"));
    assert!(path.to_str().unwrap().contains(&format!("{uuid_str}.jpg")));
}

#[test]
fn test_media_path_no_utc() {
    let root = Path::new("/lib");
    let uuid = test_uuid();
    let path = media_path(root, &uuid, "arw", None);
    assert!(path.to_str().unwrap().contains("media/1970/1970-01"));
}

#[test]
fn test_sidecar_path() {
    let root = Path::new("/lib");
    let uuid = test_uuid();
    let path = sidecar_path(root, &uuid, "jpg", Some(1_721_001_600));
    assert_eq!(path.extension(), Some(OsStr::new("cbor")));
    assert!(path.to_str().unwrap().contains("media/2024/2024-07"));
}

#[test]
fn test_attestations_path_sits_beside_the_receipts() {
    let root = Path::new("/lib");
    let uuid = test_uuid();
    let ts = Some(1_721_001_600);
    let attestations = attestations_path(root, &uuid, ts);
    assert_eq!(
        attestations.parent(),
        receipts_path(root, &uuid, ts).parent()
    );
    assert!(
        attestations
            .to_str()
            .unwrap()
            .ends_with(&format!("{}.attestations.cbor", uuid.simple()))
    );
}

#[test]
fn only_the_bare_cbor_file_is_a_sidecar() {
    let id = test_uuid().simple().to_string();
    assert!(is_sidecar_file_name(&format!("{id}.cbor")));
    for sibling in ["provenance", "receipts", "attestations", "derivatives"] {
        assert!(
            !is_sidecar_file_name(&format!("{id}.{sibling}.cbor")),
            "{sibling} is a sibling file, not a sidecar"
        );
    }
    assert!(!is_sidecar_file_name(&format!(
        "{id}.attestations.cbor.tmp"
    )));
    assert!(!is_sidecar_file_name(&format!("{id}.jpg")));
}

#[test]
fn test_meta_cache_path() {
    let root = Path::new("/lib");
    let uuid = test_uuid();
    let path = meta_cache_path(root, &uuid);
    assert!(path.to_str().unwrap().contains("index/meta"));
    assert!(path.to_str().unwrap().ends_with(".meta.cbor"));
}

#[test]
fn test_transcode_h264_path() {
    let root = Path::new("/lib");
    let uuid = test_uuid();
    let path = transcode_h264_path(root, &uuid);
    assert!(path.to_str().unwrap().contains("transcodes/h264"));
    assert_eq!(path.extension(), Some(OsStr::new("mp4")));
}

#[test]
fn test_transcode_live_path() {
    let root = Path::new("/lib");
    let uuid = test_uuid();
    let path = transcode_live_path(root, &uuid);
    assert!(path.to_str().unwrap().contains("transcodes/live"));
    assert_eq!(path.extension(), Some(OsStr::new("mov")));
}

#[test]
fn test_trash_path() {
    let root = Path::new("/lib");
    let uuid = test_uuid();
    let path = trash_path(root, &uuid, "jpg");
    assert!(path.to_str().unwrap().contains(".library/trash"));
    assert_eq!(path.extension(), Some(OsStr::new("jpg")));
}

#[test]
fn test_tmp_path() {
    let p = PathBuf::from("/lib/media/2024/2024-07/abc.jpg");
    let tmp = tmp_path(&p);
    assert_eq!(tmp, PathBuf::from("/lib/media/2024/2024-07/abc.jpg.tmp"));
}

#[test]
fn test_thumbnail_size_str() {
    assert_eq!(ThumbnailSize::Xs.as_str(), "xs");
    assert_eq!(ThumbnailSize::O.as_str(), "o");
}
