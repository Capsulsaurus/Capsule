//! Session-lifecycle smoke, discard survival-floor, startup scrub, and crash-injection
//! recovery tests. The HTTP-observable behaviors run against the real server; the internal
//! recovery machinery (scrub, CAS, eviction) runs against the crate-internal services.

use bytes::Bytes;
use jiff::{SignedDuration, Timestamp};
use salvo::Service;
use salvo::http::StatusCode;
use salvo::test::TestClient;
use sea_orm::EntityTrait;

use super::{PROTOCOL, TestCtx, setup, sha256_hex, valid_create_body};
use crate::models::session::{BlobRole, UploadSession, UploadSessionStatus};

async fn create(ctx: &TestCtx, svc: &Service, hash: &str, size: u64) -> String {
    use salvo::test::ResponseExt;
    let body = valid_create_body(&ctx.album_id, hash, size);
    let mut res = TestClient::post("http://localhost/upload")
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .json(&body)
        .send(svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CREATED));
    res.take_json::<serde_json::Value>().await.unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn patch(ctx: &TestCtx, svc: &Service, id: &str, offset: u64, bytes: Vec<u8>) -> StatusCode {
    let checksum = sha256_hex(&bytes);
    TestClient::patch(format!("http://localhost/upload/{id}"))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .add_header("Content-Type", "application/octet-stream", true)
        .add_header("X-Capsule-Offset", offset.to_string(), true)
        .add_header("X-Capsule-Checksum", checksum, true)
        .body(bytes)
        .send(svc)
        .await
        .status_code
        .unwrap()
}

#[tokio::test]
async fn session_lifecycle_smoke() {
    let ctx = setup().await;
    let svc = ctx.service();
    let bytes = vec![42u8; 4096];
    let hash = sha256_hex(&bytes);
    let id = create(&ctx, &svc, &hash, bytes.len() as u64).await;

    // HEAD reflects Pending / offset 0.
    let head = TestClient::head(format!("http://localhost/upload/{id}"))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .send(&svc)
        .await;
    assert_eq!(head.status_code, Some(StatusCode::OK));
    assert_eq!(
        head.headers().get("X-Capsule-Offset").unwrap(),
        "0",
        "fresh session offset is zero"
    );

    // Upload the single final chunk; completion runs finalization automatically.
    assert_eq!(
        patch(&ctx, &svc, &id, 0, bytes).await,
        StatusCode::NO_CONTENT
    );

    // The session is Completed and the pending asset row is now uploaded.
    let session = ctx.session_manager.get(&id).await.unwrap().unwrap();
    assert_eq!(session.status, UploadSessionStatus::Completed);
    let asset = entity::asset::Entity::find_by_id(&session.asset_id)
        .one(&ctx.db)
        .await
        .unwrap()
        .unwrap();
    assert!(asset.uploaded, "asset marked uploaded on finalization");

    // The blob was committed into the content-addressed store.
    let blob_path = ctx.upload_dir.join("blobs").join(format!("{hash}.bin"));
    assert!(
        blob_path.exists(),
        "blob committed to content-addressed store"
    );
}

/// Build a stalled session (last progress in the past) directly in the store, seeding the
/// progress index with an old score.
async fn seed_stalled(ctx: &TestCtx, ago: SignedDuration) -> String {
    let id = nanoid::nanoid!();
    let when = Timestamp::now() - ago;
    let session = UploadSession {
        id: id.clone(),
        asset_id: nanoid::nanoid!(),
        owner_id: ctx.user_id.clone(),
        upload_user_id: ctx.user_id.clone(),
        album_id: Some(ctx.album_id.clone()),
        content_type: Some("image/jpeg".to_string()),
        expected_hash: sha256_hex(b"x"),
        crypto_suite_id: 1,
        protocol_version: PROTOCOL.to_string(),
        blob_role: BlobRole::Original,
        intent_id: None,
        manifest_envelope: "{}".to_string(),
        received_bytes: 0,
        total_size: 100,
        status: UploadSessionStatus::Uploading,
        created_at: when,
        last_progress_at: when,
        expires_at: Timestamp::now() + SignedDuration::from_hours(22),
    };
    ctx.session_manager.create(&session).await.unwrap();
    id
}

#[tokio::test]
async fn discard_floor_protects_recent_progress() {
    let ctx = setup().await;
    // One session stalled beyond the survival floor; one that just made progress.
    let stalled = seed_stalled(&ctx, SignedDuration::from_hours(2)).await;
    let fresh = seed_stalled(&ctx, SignedDuration::from_secs(30)).await;

    // Inject heavy pressure.
    ctx.discard()
        .evict_for_pressure(1_000_000_000)
        .await
        .unwrap();

    // The within-floor session survives; the stalled one is evicted.
    assert!(
        ctx.session_manager.get(&fresh).await.unwrap().is_some(),
        "session that progressed within the floor is never evicted"
    );
    assert!(
        ctx.session_manager.get(&stalled).await.unwrap().is_none(),
        "stalled session past the floor is evicted under pressure"
    );
}

#[tokio::test]
async fn startup_scrub_deletes_orphan_files() {
    let ctx = setup().await;
    // An upload file with no backing session.
    let orphan = ctx.upload_dir.join(format!("{}.bin", nanoid::nanoid!()));
    std::fs::write(&orphan, b"orphan bytes").unwrap();
    assert!(orphan.exists());

    let report = ctx.discard().scrub().await.unwrap();
    assert_eq!(report.orphan_files_deleted, 1);
    assert!(!orphan.exists(), "orphan upload file deleted by scrub");
}

#[tokio::test]
async fn startup_scrub_fails_length_diverged_session() {
    let ctx = setup().await;
    let svc = ctx.service();
    // Accept one aligned chunk of a larger declared upload (stays Uploading).
    let id = create(&ctx, &svc, &sha256_hex(&vec![1u8; 16384]), 16384).await;
    assert_eq!(
        patch(&ctx, &svc, &id, 0, vec![1u8; 4096]).await,
        StatusCode::NO_CONTENT
    );

    // Truncate the on-disk file below the recorded offset (an ACK the disk can't back).
    let path = ctx.upload_dir.join(format!("{id}.bin"));
    let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.set_len(100).unwrap();
    drop(f);

    let report = ctx.discard().scrub().await.unwrap();
    assert_eq!(report.length_diverged_failed, 1);
    let session = ctx.session_manager.get(&id).await.unwrap().unwrap();
    assert_eq!(session.status, UploadSessionStatus::FailedProcessing);
    assert!(!path.exists(), "diverged upload file removed");
}

#[tokio::test]
async fn crash_between_append_and_counter_recovers_from_disk() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create(&ctx, &svc, &sha256_hex(&vec![1u8; 16384]), 16384).await;
    assert_eq!(
        patch(&ctx, &svc, &id, 0, vec![1u8; 4096]).await,
        StatusCode::NO_CONTENT
    );

    // Simulate a durable append whose counter increment was lost to a crash: bytes are on
    // disk (file longer) but received_bytes is still 4096.
    let path = ctx.upload_dir.join(format!("{id}.bin"));
    let mut existing = std::fs::read(&path).unwrap();
    existing.extend_from_slice(&[1u8; 4096]);
    std::fs::write(&path, &existing).unwrap();

    // On restart the scrub reconciles the counter forward to the on-disk truth.
    let report = ctx.discard().scrub().await.unwrap();
    assert_eq!(report.reconciled_forward, 1);

    // HEAD now reports the on-disk offset.
    let head = TestClient::head(format!("http://localhost/upload/{id}"))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .send(&svc)
        .await;
    assert_eq!(head.headers().get("X-Capsule-Offset").unwrap(), "8192");
}

#[tokio::test]
async fn crash_between_rename_and_commit_leaves_no_half_state() {
    let ctx = setup().await;
    let svc = ctx.service();
    let bytes = vec![3u8; 4096];
    let hash = sha256_hex(&bytes);
    let id = create(&ctx, &svc, &hash, bytes.len() as u64).await;

    // Fill the transfer via the service (no auto-finalize), so we control finalization.
    ctx.upload_service
        .append_chunk(&id, Bytes::from(bytes.clone()), 0, &hash)
        .await
        .unwrap();

    // Force the finalization commit to fail after the blob rename by removing the pending
    // asset row out from under it (the crash window between rename and commit).
    let session = ctx.session_manager.get(&id).await.unwrap().unwrap();
    entity::asset::Entity::delete_by_id(&session.asset_id)
        .exec(&ctx.db)
        .await
        .unwrap();

    let err = ctx.upload_service.finalize_upload(&id).await;
    assert!(
        err.is_err(),
        "finalization must fail when the commit cannot land"
    );

    // Recovery per the asset-bundle atomicity invariant: the session is terminal and the
    // partial blob is GC'd — no half-committed state.
    let session = ctx.session_manager.get(&id).await.unwrap().unwrap();
    assert_eq!(session.status, UploadSessionStatus::FailedProcessing);
    let blob_path = ctx.upload_dir.join("blobs").join(format!("{hash}.bin"));
    assert!(!blob_path.exists(), "partial blob rolled back / GC'd");
}

#[tokio::test]
async fn finalization_cas_admits_one_winner() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create(&ctx, &svc, &sha256_hex(&vec![1u8; 4096]), 4096).await;

    // First CAS wins; the racing second observes the transition and loses.
    assert!(ctx.session_manager.begin_finalize_cas(&id).await.unwrap());
    assert!(!ctx.session_manager.begin_finalize_cas(&id).await.unwrap());

    // A finalize against the now-WaitingForProcessing session is finalize_in_progress.
    let err = ctx.upload_service.finalize_upload(&id).await.unwrap_err();
    assert_eq!(
        err.code(),
        Some(capsule_i18n::error_codes::UPLOAD_FINALIZE_IN_PROGRESS)
    );
}
