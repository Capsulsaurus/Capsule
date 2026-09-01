//! Rejecting tests for server-side invariants 1–15 and every row of the upload-protocol
//! Strictness Table, plus the idempotency (replay + create-dedup) contract. Each rejection
//! asserts BOTH the HTTP status and the `error.*` code.

use capsule_i18n::error_codes;
use salvo::Service;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use serde_json::Value;

use super::{PROTOCOL, TestCtx, error_code, setup, sha256_hex, valid_create_body};

/// POST /upload with an explicit protocol header and JSON body.
async fn post_create(
    ctx: &TestCtx,
    svc: &Service,
    protocol: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let mut res = TestClient::post("http://localhost/upload")
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", protocol, true)
        .json(body)
        .send(svc)
        .await;
    let status = res.status_code.unwrap_or(StatusCode::OK);
    let json = res.take_json::<Value>().await.unwrap_or(Value::Null);
    (status, json)
}

/// Create a valid session and return its upload id.
async fn create_session(ctx: &TestCtx, svc: &Service, hash: &str, size: u64) -> String {
    let body = valid_create_body(&ctx.album_id, hash, size);
    let mut res = TestClient::post("http://localhost/upload")
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .json(&body)
        .send(svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::CREATED), "create failed");
    let json = res.take_json::<Value>().await.expect("create body");
    json["id"].as_str().expect("id").to_string()
}

/// PATCH a chunk. `checksum` and `content_type` are `Some` to include the header, `None` to
/// omit it (for the missing-header strictness rows).
async fn patch_chunk(
    ctx: &TestCtx,
    svc: &Service,
    id: &str,
    offset: u64,
    checksum: Option<&str>,
    content_type: Option<&str>,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let mut req = TestClient::patch(format!("http://localhost/upload/{id}"))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .add_header("X-Capsule-Offset", offset.to_string(), true);
    if let Some(ct) = content_type {
        req = req.add_header("Content-Type", ct, true);
    }
    if let Some(cs) = checksum {
        req = req.add_header("X-Capsule-Checksum", cs, true);
    }
    let mut res = req.body(body).send(svc).await;
    let status = res.status_code.unwrap_or(StatusCode::OK);
    let json = res.take_json::<Value>().await.unwrap_or(Value::Null);
    (status, json)
}

fn assert_rejected(status: StatusCode, body: &Value, expected: StatusCode, code: &str) {
    assert_eq!(status, expected, "status; body={body:?}");
    assert_eq!(error_code(body), Some(code), "error code; body={body:?}");
}

// ─────────────────────────────── Invariants 1–15 ────────────────────────────────

#[tokio::test]
async fn invariant_1_protocol_out_of_window() {
    let ctx = setup().await;
    let svc = ctx.service();
    let body = valid_create_body(&ctx.album_id, &sha256_hex(b"x"), 1);
    let (status, json) = post_create(&ctx, &svc, "2020-01-01", &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::UPGRADE_REQUIRED,
        error_codes::PROTOCOL_VERSION_UNSUPPORTED,
    );
}

#[tokio::test]
async fn invariant_2_unknown_crypto_suite() {
    let ctx = setup().await;
    let svc = ctx.service();
    let mut body = valid_create_body(&ctx.album_id, &sha256_hex(b"x"), 10);
    body["crypto_suite_id"] = 0x9999.into();
    body["manifest_envelope"]["crypto_suite_id"] = 0x9999.into();
    let (status, json) = post_create(&ctx, &svc, PROTOCOL, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_UNKNOWN_CRYPTO_SUITE,
    );
}

#[tokio::test]
async fn invariant_3_invalid_hash_length() {
    let ctx = setup().await;
    let svc = ctx.service();
    let bad = "abc123"; // not 64 hex chars
    let mut body = valid_create_body(&ctx.album_id, bad, 10);
    body["manifest_envelope"]["ciphertext_hash"] = bad.into();
    let (status, json) = post_create(&ctx, &svc, PROTOCOL, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_INVALID_HASH,
    );
}

#[tokio::test]
async fn invariant_4_zero_size() {
    let ctx = setup().await;
    let svc = ctx.service();
    let body = valid_create_body(&ctx.album_id, &sha256_hex(b"x"), 0);
    let (status, json) = post_create(&ctx, &svc, PROTOCOL, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_INVALID_SIZE,
    );
}

#[tokio::test]
async fn invariant_5_unsupported_content_type() {
    let ctx = setup().await;
    let svc = ctx.service();
    let mut body = valid_create_body(&ctx.album_id, &sha256_hex(b"x"), 10);
    body["content_type"] = "application/x-evil".into();
    let (status, json) = post_create(&ctx, &svc, PROTOCOL, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_UNSUPPORTED_CONTENT_TYPE,
    );
}

#[tokio::test]
async fn invariant_6_album_access_denied() {
    let ctx = setup().await;
    let svc = ctx.service();
    let bogus = nanoid::nanoid!();
    let body = valid_create_body(&bogus, &sha256_hex(b"x"), 10);
    let (status, json) = post_create(&ctx, &svc, PROTOCOL, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::FORBIDDEN,
        error_codes::UPLOAD_ALBUM_ACCESS_DENIED,
    );
}

#[tokio::test]
async fn invariant_7_device_added_after_timestamp() {
    let ctx = setup().await;
    let svc = ctx.service();
    let mut body = valid_create_body(&ctx.album_id, &sha256_hex(b"x"), 10);
    // Timestamp predates the account/device authorization floor (seeded at now-24h).
    let ts = (jiff::Timestamp::now() - jiff::SignedDuration::from_hours(48)).to_string();
    body["manifest_envelope"]["timestamp"] = ts.into();
    let (status, json) = post_create(&ctx, &svc, PROTOCOL, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::FORBIDDEN,
        error_codes::UPLOAD_DEVICE_NOT_AUTHORIZED,
    );
}

#[tokio::test]
async fn invariant_8_timestamp_out_of_drift() {
    let ctx = setup().await;
    let svc = ctx.service();
    let mut body = valid_create_body(&ctx.album_id, &sha256_hex(b"x"), 10);
    // Far in the future: past the account floor (passes inv7) but beyond the drift bound.
    let ts = (jiff::Timestamp::now() + jiff::SignedDuration::from_hours(24 * 60)).to_string();
    body["manifest_envelope"]["timestamp"] = ts.into();
    let (status, json) = post_create(&ctx, &svc, PROTOCOL, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_TIMESTAMP_OUT_OF_RANGE,
    );
}

#[tokio::test]
async fn invariant_15_family_top_level_contradicts_envelope() {
    let ctx = setup().await;
    let svc = ctx.service();
    let mut body = valid_create_body(&ctx.album_id, &sha256_hex(b"a"), 10);
    // Top-level hash disagrees with the envelope's ciphertext_hash.
    body["manifest_envelope"]["ciphertext_hash"] = sha256_hex(b"different").into();
    let (status, json) = post_create(&ctx, &svc, PROTOCOL, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_ENVELOPE_MISMATCH,
    );
}

#[tokio::test]
async fn invariant_9_offset_mismatch() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![7u8; 4096]), 4096).await;
    let chunk = vec![7u8; 4096];
    // Offset ahead of EOF.
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        4096,
        Some(&sha256_hex(&chunk)),
        Some("application/octet-stream"),
        chunk,
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::CONFLICT,
        error_codes::UPLOAD_OFFSET_MISMATCH,
    );
}

#[tokio::test]
async fn invariant_10_unsupported_media_type() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![1u8; 4096]), 4096).await;
    let chunk = vec![1u8; 4096];
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&sha256_hex(&chunk)),
        Some("text/plain"),
        chunk,
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        error_codes::UPLOAD_UNSUPPORTED_MEDIA_TYPE,
    );
}

#[tokio::test]
async fn invariant_11_size_exceeded() {
    let ctx = setup().await;
    let svc = ctx.service();
    // Declare 4096; send an aligned 8192 chunk (aligned so the alignment check passes and
    // the cumulative-bound check fires).
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![2u8; 4096]), 4096).await;
    let chunk = vec![2u8; 8192];
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&sha256_hex(&chunk)),
        Some("application/octet-stream"),
        chunk,
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_SIZE_EXCEEDED,
    );
}

#[tokio::test]
async fn invariant_12_missing_checksum() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![3u8; 4096]), 4096).await;
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        None,
        Some("application/octet-stream"),
        vec![3u8; 4096],
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_MISSING_CHECKSUM,
    );
}

#[tokio::test]
async fn invariant_13_incomplete_finalization_rejected() {
    // Total received must equal the declared size before finalization runs. Driven against
    // the service directly (an incomplete finalize is unreachable over HTTP by construction).
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![4u8; 8192]), 8192).await;
    let chunk = vec![4u8; 4096];
    let (status, _) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&sha256_hex(&chunk)),
        Some("application/octet-stream"),
        chunk,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let err = ctx
        .upload_service
        .finalize_upload(&id)
        .await
        .expect_err("incomplete finalize must error");
    assert_eq!(err.code(), Some(error_codes::UPLOAD_MALFORMED_REQUEST));
}

#[tokio::test]
async fn invariant_14_content_hash_mismatch() {
    let ctx = setup().await;
    let svc = ctx.service();
    let bytes = vec![5u8; 4096];
    // Declare a hash that does NOT match the bytes we will upload.
    let wrong_hash = sha256_hex(b"not the bytes");
    let id = create_session(&ctx, &svc, &wrong_hash, bytes.len() as u64).await;
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&sha256_hex(&bytes)),
        Some("application/octet-stream"),
        bytes,
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_CONTENT_HASH_MISMATCH,
    );
}

#[tokio::test]
async fn invariant_15_envelope_revalidation_at_finalization() {
    let ctx = setup().await;
    let svc = ctx.service();
    let bytes = vec![6u8; 4096];
    let hash = sha256_hex(&bytes);
    let id = create_session(&ctx, &svc, &hash, bytes.len() as u64).await;

    // Revoke album write-capability after creation but before the completing chunk.
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
    entity::owner_member::Entity::delete_many()
        .filter(entity::owner_member::Column::UserId.eq(&ctx.user_id))
        .exec(&ctx.db)
        .await
        .expect("revoke membership");

    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&hash),
        Some("application/octet-stream"),
        bytes,
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_ENVELOPE_REJECTED,
    );
}

// ─────────────────────────── Strictness Table rows ─────────────────────────────

#[tokio::test]
async fn strictness_unknown_json_field() {
    let ctx = setup().await;
    let svc = ctx.service();
    let mut body = valid_create_body(&ctx.album_id, &sha256_hex(b"x"), 10);
    body["totally_unknown_field"] = "surprise".into();
    let (status, json) = post_create(&ctx, &svc, PROTOCOL, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_MALFORMED_REQUEST,
    );
}

#[tokio::test]
async fn strictness_empty_chunk() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![1u8; 4096]), 4096).await;
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&sha256_hex(b"")),
        Some("application/octet-stream"),
        vec![],
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_EMPTY_CHUNK,
    );
}

#[tokio::test]
async fn strictness_missing_content_type() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![1u8; 4096]), 4096).await;
    let chunk = vec![1u8; 4096];
    let (status, json) =
        patch_chunk(&ctx, &svc, &id, 0, Some(&sha256_hex(&chunk)), None, chunk).await;
    assert_rejected(
        status,
        &json,
        StatusCode::UNSUPPORTED_MEDIA_TYPE,
        error_codes::UPLOAD_UNSUPPORTED_MEDIA_TYPE,
    );
}

#[tokio::test]
async fn strictness_checksum_mismatch() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![1u8; 4096]), 4096).await;
    let chunk = vec![1u8; 4096];
    // A well-formed but wrong checksum (hash of different bytes).
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&sha256_hex(b"wrong")),
        Some("application/octet-stream"),
        chunk,
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_CHECKSUM_MISMATCH,
    );
}

#[tokio::test]
async fn strictness_unaligned_non_final_chunk() {
    let ctx = setup().await;
    let svc = ctx.service();
    // Declare a large total so a 100-byte chunk is a non-final chunk.
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![1u8; 8192]), 8192).await;
    let chunk = vec![1u8; 100];
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&sha256_hex(&chunk)),
        Some("application/octet-stream"),
        chunk,
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_CHUNK_NOT_ALIGNED,
    );
}

#[tokio::test]
async fn strictness_chunk_too_large() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create_session(&ctx, &svc, &sha256_hex(b"x"), 4096).await;
    // 16 MiB + 1 byte exceeds the protocol maximum (checked before checksum verification).
    let chunk = vec![0u8; 16 * 1024 * 1024 + 1];
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&sha256_hex(b"dummy")),
        Some("application/octet-stream"),
        chunk,
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::PAYLOAD_TOO_LARGE,
        error_codes::UPLOAD_CHUNK_TOO_LARGE,
    );
}

#[tokio::test]
async fn strictness_missing_offset() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![1u8; 4096]), 4096).await;
    let chunk = vec![1u8; 4096];
    let mut res = TestClient::patch(format!("http://localhost/upload/{id}"))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .add_header("Content-Type", "application/octet-stream", true)
        .add_header("X-Capsule-Checksum", sha256_hex(&chunk), true)
        .body(chunk)
        .send(&svc)
        .await;
    let status = res.status_code.unwrap_or(StatusCode::OK);
    let json = res.take_json::<Value>().await.unwrap_or(Value::Null);
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_MISSING_OFFSET,
    );
}

#[tokio::test]
async fn strictness_same_offset_different_checksum_conflicts() {
    let ctx = setup().await;
    let svc = ctx.service();
    // Declare a large total so the first aligned chunk is accepted (non-final).
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![9u8; 16384]), 16384).await;
    let first = vec![9u8; 4096];
    let (s1, _) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&sha256_hex(&first)),
        Some("application/octet-stream"),
        first,
    )
    .await;
    assert_eq!(s1, StatusCode::NO_CONTENT);
    // Re-send offset 0 with DIFFERENT bytes → conflict.
    let other = vec![8u8; 4096];
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&sha256_hex(&other)),
        Some("application/octet-stream"),
        other,
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::CONFLICT,
        error_codes::UPLOAD_CHUNK_CONFLICT,
    );
}

#[tokio::test]
async fn strictness_stale_offset() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![9u8; 16384]), 16384).await;
    let first = vec![9u8; 4096];
    let (s1, _) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&sha256_hex(&first)),
        Some("application/octet-stream"),
        first,
    )
    .await;
    assert_eq!(s1, StatusCode::NO_CONTENT);
    // A gapped offset ahead of EOF is a stale offset.
    let ahead = vec![9u8; 4096];
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        8192,
        Some(&sha256_hex(&ahead)),
        Some("application/octet-stream"),
        ahead,
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::CONFLICT,
        error_codes::UPLOAD_OFFSET_MISMATCH,
    );
}

#[tokio::test]
async fn strictness_patch_on_terminal_session() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![1u8; 4096]), 4096).await;
    // Drive the session terminal directly, then PATCH.
    ctx.session_manager
        .update_status(
            &id,
            crate::models::session::UploadSessionStatus::WaitingForProcessing,
        )
        .await
        .unwrap();
    let chunk = vec![1u8; 4096];
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&sha256_hex(&chunk)),
        Some("application/octet-stream"),
        chunk,
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::CONFLICT,
        error_codes::UPLOAD_SESSION_NOT_ACTIVE,
    );
}

// ─────────────────────────────── Idempotency ───────────────────────────────────

#[tokio::test]
async fn idempotent_chunk_replay_is_noop() {
    let ctx = setup().await;
    let svc = ctx.service();
    let id = create_session(&ctx, &svc, &sha256_hex(&vec![9u8; 16384]), 16384).await;
    let chunk = vec![9u8; 4096];
    let checksum = sha256_hex(&chunk);
    let (s1, _) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&checksum),
        Some("application/octet-stream"),
        chunk.clone(),
    )
    .await;
    assert_eq!(s1, StatusCode::NO_CONTENT);
    // Replay the identical tuple: same 204, offset unchanged, no double-write.
    let res = TestClient::patch(format!("http://localhost/upload/{id}"))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .add_header("Content-Type", "application/octet-stream", true)
        .add_header("X-Capsule-Offset", "0", true)
        .add_header("X-Capsule-Checksum", &checksum, true)
        .body(chunk)
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::NO_CONTENT));
    let offset = res
        .headers()
        .get("X-Capsule-Offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    assert_eq!(offset, Some(4096), "replay returns the same next offset");
    // On-disk length is still one chunk — no double write.
    assert_eq!(ctx.storage.file_len(&id).await.unwrap(), Some(4096));
}

#[tokio::test]
async fn duplicate_create_finalized_hash_is_conflict() {
    let ctx = setup().await;
    let svc = ctx.service();
    let bytes = vec![7u8; 4096];
    let hash = sha256_hex(&bytes);
    let id = create_session(&ctx, &svc, &hash, bytes.len() as u64).await;
    let (s, _) = patch_chunk(
        &ctx,
        &svc,
        &id,
        0,
        Some(&hash),
        Some("application/octet-stream"),
        bytes,
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);

    // Re-create the same (owner, hash, album) tuple → duplicate_blob (the merge trigger).
    let body = valid_create_body(&ctx.album_id, &hash, 4096);
    let (status, json) = post_create(&ctx, &svc, PROTOCOL, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::CONFLICT,
        error_codes::UPLOAD_DUPLICATE_BLOB,
    );
}

#[tokio::test]
async fn duplicate_create_active_session_returned() {
    let ctx = setup().await;
    let svc = ctx.service();
    let hash = sha256_hex(&vec![1u8; 8192]);
    let id = create_session(&ctx, &svc, &hash, 8192).await;
    // Re-create the same tuple while the session is active → 200 with the same id.
    let body = valid_create_body(&ctx.album_id, &hash, 8192);
    let mut res = TestClient::post("http://localhost/upload")
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .add_header("X-Capsule-Protocol", PROTOCOL, true)
        .json(&body)
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::OK));
    let json = res.take_json::<Value>().await.unwrap();
    assert_eq!(json["id"].as_str(), Some(id.as_str()));
}

#[tokio::test]
async fn patch_unknown_session_is_not_found() {
    let ctx = setup().await;
    let svc = ctx.service();
    let chunk = vec![1u8; 4096];
    let (status, json) = patch_chunk(
        &ctx,
        &svc,
        "does-not-exist",
        0,
        Some(&sha256_hex(&chunk)),
        Some("application/octet-stream"),
        chunk,
    )
    .await;
    assert_rejected(
        status,
        &json,
        StatusCode::NOT_FOUND,
        error_codes::UPLOAD_SESSION_NOT_FOUND,
    );
}
