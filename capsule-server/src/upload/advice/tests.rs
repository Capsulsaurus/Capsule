use super::*;
use crate::store::upload::LIFETIME_CAP;
use crate::store::{AssetId, BlobRole, OwnerId, UploadId, UploadSessionStatus, UserId};

/// A session created at `created` that last progressed at `progressed`, in `status`.
fn session(created: i64, progressed: i64, status: UploadSessionStatus) -> UploadSessionRecord {
    UploadSessionRecord {
        upload_id: UploadId::new("advice-up".to_owned()),
        asset_id: AssetId::new("advice-asset".to_owned()),
        owner_id: OwnerId::new("advice-owner".to_owned()),
        upload_user_id: UserId::new("advice-uploader".to_owned()),
        album_id: None,
        content_type: Some("application/octet-stream".to_owned()),
        expected_hash: "a".repeat(64),
        crypto_suite_id: 1,
        protocol_version: "2026-08-29".to_owned(),
        blob_role: BlobRole::Original,
        intent_id: None,
        manifest_envelope: "{}".to_owned(),
        received_bytes: 0,
        total_size: 8192,
        status,
        failure_code: None,
        created_at: at(created),
        last_progress_at: at(progressed),
    }
}

fn at(second: i64) -> Timestamp {
    Timestamp::from_second(1_800_000_000 + second).expect("in range")
}

#[test]
fn advice_renders_as_a_structured_field_dictionary_of_integers() {
    assert_eq!(
        Advice::for_size(1).to_string(),
        "chunk=262144, sessions=4, ranges=1, ttl=300"
    );
}

#[test]
fn the_chunk_member_is_the_suggested_chunk_size_for_the_declared_size() {
    for size in [1, 10 * 1024 * 1024, 100 * 1024 * 1024] {
        assert_eq!(Advice::for_size(size).chunk, suggested_chunk_size(size));
    }
}

#[test]
fn a_fresh_session_has_the_whole_cap_and_the_whole_floor() {
    let expiry = SessionExpiry::of(
        &session(0, 0, UploadSessionStatus::Pending),
        LIFETIME_CAP,
        at(0),
    );
    assert_eq!(expiry.expires_in, 24 * 3600);
    assert_eq!(expiry.idle_expires_in, 3600);
}

#[test]
fn progress_moves_the_floor_and_never_the_cap() {
    let expiry = SessionExpiry::of(
        &session(0, 7200, UploadSessionStatus::Uploading),
        LIFETIME_CAP,
        at(7300),
    );
    assert_eq!(expiry.expires_in, 24 * 3600 - 7300);
    assert_eq!(expiry.idle_expires_in, 3600 - 100);
}

#[test]
fn the_floor_never_outlasts_the_cap() {
    let created = 0;
    let progressed = 24 * 3600 - 60;
    let expiry = SessionExpiry::of(
        &session(created, progressed, UploadSessionStatus::Uploading),
        LIFETIME_CAP,
        at(progressed),
    );
    assert_eq!(expiry.expires_in, 60);
    assert_eq!(
        expiry.idle_expires_in, 60,
        "the cap ends a session whatever its progress"
    );
}

#[test]
fn a_session_past_uploading_lives_to_the_cap() {
    for status in [
        UploadSessionStatus::WaitingForProcessing,
        UploadSessionStatus::Completed,
        UploadSessionStatus::FailedProcessing,
    ] {
        let expiry = SessionExpiry::of(&session(0, 0, status), LIFETIME_CAP, at(5000));
        assert_eq!(expiry.idle_expires_in, expiry.expires_in, "{status:?}");
        assert_eq!(expiry.expires_in, 24 * 3600 - 5000, "{status:?}");
    }
}

#[test]
fn an_overdue_session_is_due_now_rather_than_negative() {
    let expiry = SessionExpiry::of(
        &session(0, 0, UploadSessionStatus::Uploading),
        LIFETIME_CAP,
        at(30 * 3600),
    );
    assert_eq!(
        expiry,
        SessionExpiry {
            expires_in: 0,
            idle_expires_in: 0
        }
    );
}
