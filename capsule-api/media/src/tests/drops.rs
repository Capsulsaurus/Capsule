//! Rejecting tests for drop-server invariants 26–32 (status + `error.*` code), the
//! seal→stage→adopt happy path, and the adoption-atomicity crash-injection smoke.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use capsule_core::crypto::hash::Hash32;
use capsule_i18n::error_codes;
use salvo::Service;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, TransactionTrait};
use serde_json::{Value, json};
use service::drop::{AdoptInput, AdoptOutcome, Mutation as DropMutation, StageInput};
use service::quota::QuotaLimits;

use super::{
    MediaTestCtx, adopt_manifest_cbor, drop_setup as setup, error_code, passphrase_verifier,
    seal_and_body, sha256_hex,
};

// ─────────────────────────────────────── HTTP helpers ────────────────────────────────────

async fn post_create(svc: &Service, opaque: &str, body: &Value) -> (StatusCode, Value) {
    let mut res = TestClient::post(format!("http://localhost/u/{opaque}/drop"))
        .json(body)
        .send(svc)
        .await;
    let status = res.status_code.unwrap_or(StatusCode::OK);
    let json = res.take_json::<Value>().await.unwrap_or(Value::Null);
    (status, json)
}

async fn patch_chunk(
    svc: &Service,
    opaque: &str,
    drop_id: &str,
    offset: u64,
    checksum: &str,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let mut res = TestClient::patch(format!("http://localhost/u/{opaque}/drop/{drop_id}"))
        .add_header("Content-Type", "application/octet-stream", true)
        .add_header("X-Capsule-Offset", offset.to_string(), true)
        .add_header("X-Capsule-Checksum", checksum, true)
        .body(body)
        .send(svc)
        .await;
    let status = res.status_code.unwrap_or(StatusCode::OK);
    let json = res.take_json::<Value>().await.unwrap_or(Value::Null);
    (status, json)
}

async fn get_inbox(ctx: &MediaTestCtx, svc: &Service) -> (StatusCode, Value) {
    let mut res = TestClient::get("http://localhost/drops")
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .send(svc)
        .await;
    let status = res.status_code.unwrap_or(StatusCode::OK);
    let json = res.take_json::<Value>().await.unwrap_or(Value::Null);
    (status, json)
}

async fn post_adopt(
    ctx: &MediaTestCtx,
    svc: &Service,
    drop_id: &str,
    body: &Value,
) -> (StatusCode, Value) {
    let mut res = TestClient::post(format!("http://localhost/drops/{drop_id}/adopt"))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .json(body)
        .send(svc)
        .await;
    let status = res.status_code.unwrap_or(StatusCode::OK);
    let json = res.take_json::<Value>().await.unwrap_or(Value::Null);
    (status, json)
}

fn adopt_body(manifest_cbor_b64: &str, metadata_blob: &[u8], album_id: &str) -> Value {
    json!({
        "manifest_cbor": manifest_cbor_b64,
        "metadata_blob": BASE64.encode(metadata_blob),
        "album_id": album_id,
    })
}

fn assert_rejected(status: StatusCode, body: &Value, expected: StatusCode, code: &str) {
    assert_eq!(status, expected, "status; body={body:?}");
    assert_eq!(error_code(body), Some(code), "error code; body={body:?}");
}

// ─────────────────────────────── Invariant 26: live link ─────────────────────────────────

#[tokio::test]
async fn invariant_26_unknown_link_is_indistinguishable_404() {
    let ctx = setup().await;
    let svc = ctx.service();
    let (_sealed, body) = seal_and_body("image/jpeg", b"hello drop");
    let (status, _) = post_create(&svc, "00112233445566778899aabbccddeeff", &body).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invariant_26_revoked_link_is_404() {
    let ctx = setup().await;
    let svc = ctx.service();
    let (link_id, opaque) = ctx.create_link(None, None, None, None, false, None).await;
    ctx.revoke_link(&link_id).await;
    let (_sealed, body) = seal_and_body("image/jpeg", b"hi");
    let (status, _) = post_create(&svc, &opaque, &body).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invariant_26_expired_link_is_404() {
    let ctx = setup().await;
    let svc = ctx.service();
    let past = jiff::Timestamp::now() - jiff::SignedDuration::from_hours(1);
    let (_id, opaque) = ctx
        .create_link(Some(past), None, None, None, false, None)
        .await;
    let (_sealed, body) = seal_and_body("image/jpeg", b"hi");
    let (status, _) = post_create(&svc, &opaque, &body).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invariant_26_cap_exhausted_is_409() {
    let ctx = setup().await;
    let svc = ctx.service();
    // A file-count cap of 1: the first drop-session reserves it; the second is refused.
    let (_id, opaque) = ctx
        .create_link(None, None, Some(1), None, false, None)
        .await;
    let (_s1, body1) = seal_and_body("image/jpeg", b"first");
    let (status1, _) = post_create(&svc, &opaque, &body1).await;
    assert_eq!(status1, StatusCode::CREATED);
    let (_s2, body2) = seal_and_body("image/jpeg", b"second");
    let (status2, json2) = post_create(&svc, &opaque, &body2).await;
    assert_rejected(
        status2,
        &json2,
        StatusCode::CONFLICT,
        error_codes::DROP_CAP_EXCEEDED,
    );
}

// ───────────────────────── Invariant 27: content-type enum ───────────────────────────────

#[tokio::test]
async fn invariant_27_unsupported_content_type_is_400() {
    let ctx = setup().await;
    let svc = ctx.service();
    let (_id, opaque) = ctx.create_link(None, None, None, None, false, None).await;
    let (_sealed, body) = seal_and_body("application/zip", b"payload");
    let (status, json) = post_create(&svc, &opaque, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_UNSUPPORTED_CONTENT_TYPE,
    );
}

// ─────────────────────────────── Invariant 28: size bounds ───────────────────────────────

#[tokio::test]
async fn invariant_28_zero_size_is_400() {
    let ctx = setup().await;
    let svc = ctx.service();
    let (_id, opaque) = ctx.create_link(None, None, None, None, false, None).await;
    let (_sealed, mut body) = seal_and_body("image/jpeg", b"payload");
    body["size"] = json!(0);
    let (status, json) = post_create(&svc, &opaque, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_INVALID_SIZE,
    );
}

#[tokio::test]
async fn invariant_28_oversize_is_413() {
    let ctx = setup().await;
    let svc = ctx.service();
    let (_id, opaque) = ctx.create_link(None, None, None, None, false, None).await;
    let (_sealed, mut body) = seal_and_body("image/jpeg", b"payload");
    body["size"] = json!(ctx.config.max_file_size + 1);
    let (status, json) = post_create(&svc, &opaque, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::PAYLOAD_TOO_LARGE,
        error_codes::UPLOAD_FILE_TOO_LARGE,
    );
}

// ───────────────────────────────── Invariant 29: owner quota ─────────────────────────────

#[tokio::test]
async fn invariant_29_owner_quota_exceeded_is_403() {
    let mut ctx = setup().await;
    ctx.set_quota_limits(u64::MAX, 100).await;
    // Pre-load the owner to 95 of a 100-byte hard limit.
    ctx.seed_asset(&sha256_hex(b"seed-original"), 95).await;
    let svc = ctx.service();
    let (_id, opaque) = ctx.create_link(None, None, None, None, false, None).await;
    let (_sealed, mut body) = seal_and_body("image/jpeg", b"payload");
    body["size"] = json!(50u64); // 95 + 50 > 100
    let (status, json) = post_create(&svc, &opaque, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::FORBIDDEN,
        error_codes::QUOTA_EXCEEDED,
    );
}

// ───────────────────────── Invariant 30: descriptor well-formedness ──────────────────────

#[tokio::test]
async fn invariant_30_bad_kem_ct_length_is_400() {
    let ctx = setup().await;
    let svc = ctx.service();
    let (_id, opaque) = ctx.create_link(None, None, None, None, false, None).await;
    let (_sealed, mut body) = seal_and_body("image/jpeg", b"payload");
    // A kem_ct that decodes but is the wrong length for the suite.
    body["descriptor"]["kem_ct"] = json!(BASE64.encode([0u8; 10]));
    let (status, json) = post_create(&svc, &opaque, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::DROP_MALFORMED_DESCRIPTOR,
    );
}

#[tokio::test]
async fn invariant_30_descriptor_naming_an_album_is_400() {
    let ctx = setup().await;
    let svc = ctx.service();
    let (_id, opaque) = ctx.create_link(None, None, None, None, false, None).await;
    let (_sealed, mut body) = seal_and_body("image/jpeg", b"payload");
    // A drop that supplies an album/manifest field is refused (deny_unknown_fields → 400).
    body["descriptor"]["album_id"] = json!("some-album");
    let (status, json) = post_create(&svc, &opaque, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::DROP_MALFORMED_DESCRIPTOR,
    );
}

// ─────────────────────────────── Invariant 31: rate limiting ─────────────────────────────

#[tokio::test]
async fn invariant_31_rate_limited_is_429() {
    let mut ctx = setup().await;
    ctx.set_rate_limit(2).await;
    let svc = ctx.service();
    let opaque = "0123456789abcdef0123456789abcdef";
    let (_sealed, body) = seal_and_body("image/jpeg", b"payload");
    // The first two are within budget (they 404 on the unknown link); the third is rate-limited.
    let (s1, _) = post_create(&svc, opaque, &body).await;
    assert_eq!(s1, StatusCode::NOT_FOUND);
    let (s2, _) = post_create(&svc, opaque, &body).await;
    assert_eq!(s2, StatusCode::NOT_FOUND);
    let (s3, j3) = post_create(&svc, opaque, &body).await;
    assert_rejected(
        s3,
        &j3,
        StatusCode::TOO_MANY_REQUESTS,
        error_codes::DROP_RATE_LIMITED,
    );
}

// ─────────────────────────────── Invariant 32: adoption ──────────────────────────────────

#[tokio::test]
async fn invariant_32_adopt_hash_not_in_inbox_is_409() {
    let ctx = setup().await;
    let svc = ctx.service();
    // A structurally valid create manifest whose ciphertext_hash references nothing in the inbox.
    let phantom = Hash32([0x11; 32]);
    let metadata = b"adopter sidecar";
    let manifest = adopt_manifest_cbor(&ctx, &phantom, metadata);
    let body = adopt_body(&manifest, metadata, &ctx.album_id);
    let (status, json) = post_adopt(&ctx, &svc, "drop-x", &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::CONFLICT,
        error_codes::DROP_NOT_IN_INBOX,
    );
}

#[tokio::test]
async fn invariant_32_25_metadata_hash_mismatch_is_400() {
    let ctx = setup().await;
    let svc = ctx.service();
    // Manifest binds metadata A; the request submits metadata B (invariant 25 within adoption).
    let cipher = Hash32([0x22; 32]);
    let manifest = adopt_manifest_cbor(&ctx, &cipher, b"metadata-A");
    let body = adopt_body(&manifest, b"metadata-B", &ctx.album_id);
    let (status, json) = post_adopt(&ctx, &svc, "drop-x", &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::BAD_REQUEST,
        error_codes::UPLOAD_ENVELOPE_MISMATCH,
    );
}

// ──────────────────────────── Happy path: seal → stage → adopt ───────────────────────────

#[tokio::test]
async fn drop_seal_stage_adopt_end_to_end() {
    let ctx = setup().await;
    let svc = ctx.service();
    let (_id, opaque) = ctx.create_link(None, None, None, None, false, None).await;

    // 1. Seal + open the session.
    let (sealed, body) = seal_and_body("image/jpeg", b"a guest photo, sealed in the browser");
    let (status, json) = post_create(&svc, &opaque, &body).await;
    assert_eq!(status, StatusCode::CREATED, "create: {json:?}");
    let drop_id = json["drop_id"].as_str().unwrap().to_string();

    // 2. Upload the ciphertext in one final chunk → finalize into the inbox.
    let checksum = sha256_hex(&sealed.ciphertext);
    let (status, json) = patch_chunk(
        &svc,
        &opaque,
        &drop_id,
        0,
        &checksum,
        sealed.ciphertext.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "patch: {json:?}");

    // 3. The drop is in the owner's inbox.
    let (status, inbox) = get_inbox(&ctx, &svc).await;
    assert_eq!(status, StatusCode::OK);
    let drops = inbox["drops"].as_array().unwrap();
    assert_eq!(drops.len(), 1, "inbox: {inbox:?}");
    assert_eq!(
        drops[0]["ciphertext_hash"].as_str().unwrap(),
        sealed.descriptor.ciphertext_hash.to_hex()
    );

    // 4. Adopt into the album (signed create manifest referencing the inbox blob).
    let metadata = b"the adopter's freshly authored sidecar";
    let manifest = adopt_manifest_cbor(&ctx, &sealed.descriptor.ciphertext_hash, metadata);
    let body = adopt_body(&manifest, metadata, &ctx.album_id);
    let (status, json) = post_adopt(&ctx, &svc, &drop_id, &body).await;
    assert_eq!(status, StatusCode::OK, "adopt: {json:?}");
    let asset_id = json["asset_id"].as_str().unwrap().to_string();

    // 5. The blob is now an album asset; a sync feed entry exists; the inbox row is gone.
    let asset = entity::asset::Entity::find_by_id(&asset_id)
        .one(&ctx.db)
        .await
        .unwrap()
        .expect("promoted asset row");
    assert_eq!(asset.file_hash, sealed.descriptor.ciphertext_hash.to_hex());
    assert!(asset.uploaded);
    assert_eq!(asset.album_id.as_deref(), Some(ctx.album_id.as_str()));

    let feed = entity::sync_entry::Entity::find()
        .filter(entity::sync_entry::Column::AlbumId.eq(&ctx.album_id))
        .all(&ctx.db)
        .await
        .unwrap();
    assert_eq!(feed.len(), 1, "one feed entry minted for the adoption");
    assert_eq!(feed[0].asset_id, asset_id);

    let (_s, inbox) = get_inbox(&ctx, &svc).await;
    assert!(
        inbox["drops"].as_array().unwrap().is_empty(),
        "inbox drained"
    );

    // 6. Re-adopting the same (now-absent) drop is idempotent: returns the promoted asset.
    let (status, json) = post_adopt(&ctx, &svc, &drop_id, &body).await;
    assert_eq!(status, StatusCode::OK, "idempotent re-adopt: {json:?}");
    assert_eq!(json["asset_id"].as_str().unwrap(), asset_id);
}

// ─────────────────── Adoption atomicity: crash between promotion steps ────────────────────

#[tokio::test]
async fn adoption_atomicity_rollback_leaves_no_half_state() {
    let ctx = setup().await;
    let cipher = sha256_hex(b"a staged drop ciphertext");

    // Stage a pending inbox drop directly.
    let drop_id = uuid::Uuid::now_v7().to_string();
    DropMutation::stage_drop(
        &ctx.db,
        StageInput {
            drop_id: drop_id.clone(),
            owner_id: ctx.owner_id.clone(),
            link_id: "link-x".to_string(),
            ciphertext_hash: cipher.clone(),
            size: 128,
            content_type: "image/jpeg".to_string(),
            suggested_filename: None,
            descriptor: json!({}),
            single_use: false,
        },
    )
    .await
    .unwrap();

    let metadata = b"sidecar";
    let input = AdoptInput {
        album_id: ctx.album_id.clone(),
        ciphertext_hash: cipher.clone(),
        metadata_hash: sha256_hex(metadata),
        metadata_blob: metadata.to_vec(),
        manifest_cbor: b"opaque-manifest".to_vec(),
        protocol_version: super::PROTOCOL.to_string(),
    };

    // Drive the promotion inside a transaction, then simulate a crash between the promotion
    // steps and the commit by rolling back.
    let txn = ctx.db.begin().await.unwrap();
    let outcome =
        DropMutation::adopt_in_txn(&txn, &ctx.owner_id, &input, &QuotaLimits::unlimited())
            .await
            .unwrap();
    assert!(matches!(outcome, AdoptOutcome::Promoted { .. }));
    txn.rollback().await.unwrap();

    // No half-state: the inbox row survives; no asset row; no sync feed entry.
    let inbox_rows = entity::drop_inbox::Entity::find()
        .filter(entity::drop_inbox::Column::OwnerId.eq(&ctx.owner_id))
        .all(&ctx.db)
        .await
        .unwrap();
    assert_eq!(inbox_rows.len(), 1, "inbox row must survive the rollback");
    assert_eq!(inbox_rows[0].drop_id, drop_id);

    let assets = entity::asset::Entity::find()
        .filter(entity::asset::Column::FileHash.eq(&cipher))
        .all(&ctx.db)
        .await
        .unwrap();
    assert!(assets.is_empty(), "no half-promoted asset row");

    let feed = entity::sync_entry::Entity::find()
        .filter(entity::sync_entry::Column::AlbumId.eq(&ctx.album_id))
        .all(&ctx.db)
        .await
        .unwrap();
    assert!(feed.is_empty(), "no orphaned sync feed entry");
}

// ─────────────────────────────── Passphrase abuse gate ──────────────────────────────────

#[tokio::test]
async fn passphrase_gate_refuses_without_proof() {
    let ctx = setup().await;
    let svc = ctx.service();
    let (verifier, _proof) = passphrase_verifier("open sesame");
    let (_id, opaque) = ctx
        .create_link(None, None, None, None, false, Some(verifier))
        .await;
    let (_sealed, body) = seal_and_body("image/jpeg", b"x");
    let (status, json) = post_create(&svc, &opaque, &body).await;
    assert_rejected(
        status,
        &json,
        StatusCode::FORBIDDEN,
        error_codes::DROP_PASSPHRASE_REQUIRED,
    );
}

#[tokio::test]
async fn passphrase_gate_accepts_valid_proof() {
    let ctx = setup().await;
    let svc = ctx.service();
    let (verifier, proof) = passphrase_verifier("open sesame");
    let (_id, opaque) = ctx
        .create_link(None, None, None, None, false, Some(verifier))
        .await;
    let (_sealed, mut body) = seal_and_body("image/jpeg", b"x");
    body["passphrase_proof"] = json!(proof);
    let (status, _json) = post_create(&svc, &opaque, &body).await;
    assert_eq!(status, StatusCode::CREATED);
}

// ─────────────────────────────── Discard frees the inbox ─────────────────────────────────

#[tokio::test]
async fn discard_removes_the_inbox_row() {
    let ctx = setup().await;
    let svc = ctx.service();
    let (_id, opaque) = ctx.create_link(None, None, None, None, false, None).await;
    let (sealed, body) = seal_and_body("image/jpeg", b"discard me");
    let (_s, json) = post_create(&svc, &opaque, &body).await;
    let drop_id = json["drop_id"].as_str().unwrap().to_string();
    let checksum = sha256_hex(&sealed.ciphertext);
    let (s, _) = patch_chunk(
        &svc,
        &opaque,
        &drop_id,
        0,
        &checksum,
        sealed.ciphertext.clone(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    let mut res = TestClient::delete(format!("http://localhost/drops/{drop_id}"))
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::NO_CONTENT));
    let _ = res.take_string().await;

    let (_s, inbox) = get_inbox(&ctx, &svc).await;
    assert!(inbox["drops"].as_array().unwrap().is_empty());
}
