use capsule_core::crypto::hash::hash_bytes;

use super::*;

#[test]
fn hash_file_matches_one_shot() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("blob.bin");
    // Larger than the 64 KiB read block, so the streaming path takes more than one turn.
    let data = vec![0x5Au8; 64 * 1024 + 7];
    std::fs::write(&path, &data).unwrap();

    assert_eq!(hash_file(&path).unwrap(), hash_bytes(&data));
}

#[test]
fn hash_file_reports_a_missing_path() {
    let dir = tempfile::tempdir().unwrap();
    let err = hash_file(&dir.path().join("absent.bin")).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
}
