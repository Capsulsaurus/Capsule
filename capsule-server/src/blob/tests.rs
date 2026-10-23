use super::*;

#[test]
fn an_upload_id_must_be_a_name_every_adapter_can_carry() {
    assert!(check_upload_id(&UploadId::new("019286c1-3f7a-7c11-9d2e-6f5a1b2c3d4e")).is_ok());
    assert!(check_upload_id(&UploadId::new("a_b-C9")).is_ok());
    assert!(check_upload_id(&UploadId::new("")).is_err());
    assert!(check_upload_id(&UploadId::new("..")).is_err());
    assert!(check_upload_id(&UploadId::new("../../etc/passwd")).is_err());
    assert!(check_upload_id(&UploadId::new("a/b")).is_err());
    assert!(check_upload_id(&UploadId::new("a.bin")).is_err());
    assert!(check_upload_id(&UploadId::new("a".repeat(MAX_UPLOAD_ID_LEN))).is_ok());
    assert!(check_upload_id(&UploadId::new("a".repeat(MAX_UPLOAD_ID_LEN + 1))).is_err());
}

#[test]
fn a_read_window_is_clamped_to_the_blob_it_reads() {
    assert_eq!(window(10, 0, 4), (0, 4));
    assert_eq!(
        window(10, 8, 4),
        (8, 2),
        "a window past the end stops there"
    );
    assert_eq!(
        window(10, 10, 4),
        (0, 0),
        "a window starting at the end is empty"
    );
    assert_eq!(window(10, 99, 4), (0, 0));
    assert_eq!(window(10, 0, 0), (0, 0));
}
