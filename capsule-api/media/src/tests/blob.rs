//! Key-free ranged blob serving (slice `S-C10`), against the real `GET /blob/{hash}` router
//! + a real Postgres testcontainer + an on-disk content-addressed blob tree.
//!
//! Coverage map:
//! - `full_get_serves_ciphertext_and_decrypts` — a whole-blob GET returns the exact ciphertext
//!   and `Accept-Ranges: bytes`; it decrypts back to the plaintext with core's STREAM.
//! - `ranged_mid_file_chunk_decrypts_without_full_file` — **the encryption doc's ranged-read
//!   acceptance**: a `Range` at the ciphertext stride fetches one *interior* chunk (`206`,
//!   `Content-Range`) and core's `decrypt_chunk` decrypts it in isolation, proving a mid-file
//!   range decrypts without the whole file.
//! - `ranged_reads_stitch_to_sequential_decrypt` — every stride-aligned range, fetched
//!   separately and stitched, byte-matches a sequential decrypt of the whole ciphertext.
//! - Status taxonomy: `unknown_hash_is_404`, `malformed_hash_is_404`, `quarantined_is_410`,
//!   `mid_gc_is_410`, `dangling_reference_is_410`, `awaiting_original_is_pending_upload_not_410`,
//!   `missing_token_is_401`.

use capsule_core::crypto::encryption::stream::{
    CIPHERTEXT_CHUNK, PLAINTEXT_CHUNK, decrypt_asset_vec, decrypt_chunk, encrypt_asset_vec_full,
};
use capsule_i18n::error_codes;
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use serde_json::Value;

use super::{TestCtx, setup};

/// A fixed file key for the STREAM fixtures (the server holds no key — this lives only in the
/// test so it can decrypt what the server served).
const FILE_KEY: [u8; 32] = [0x2b; 32];

/// A multi-chunk plaintext with a partial final chunk (3 full chunks + 500 bytes) so ranged
/// reads exercise interior chunks *and* the last-chunk flag.
fn fixture_plaintext() -> Vec<u8> {
    (0..(PLAINTEXT_CHUNK * 3 + 500))
        .map(|i| (i % 251) as u8)
        .collect()
}

/// A bearer GET against the blob router.
fn get(url: &str, token: &str) -> salvo::test::RequestBuilder {
    TestClient::get(url).add_header("Authorization", format!("Bearer {token}"), true)
}

/// **Full GET (smoke).** Encrypt a real asset, finalize its ciphertext blob, then
/// `GET /blob/{hash}` with no `Range`: assert `200`, `Accept-Ranges: bytes`, the body is the
/// exact ciphertext, and it decrypts back to the plaintext.
#[tokio::test]
async fn full_get_serves_ciphertext_and_decrypts() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let plaintext = fixture_plaintext();
    let (meta, ciphertext) = encrypt_asset_vec_full(&FILE_KEY, &plaintext);

    let hash = ctx.finalize_blob(&asset_id, "original", &ciphertext).await;
    assert_eq!(
        hash,
        meta.ciphertext_hash.to_hex(),
        "content address matches"
    );

    let svc = ctx.blob_service();
    let mut res = get(&format!("http://localhost/{hash}"), &ctx.token())
        .send(&svc)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::OK));
    assert_eq!(
        res.headers()
            .get("accept-ranges")
            .and_then(|v| v.to_str().ok()),
        Some("bytes"),
        "ranged serving is advertised"
    );
    let body = res.take_bytes(None).await.expect("body bytes");
    assert_eq!(
        body.as_ref(),
        ciphertext.as_slice(),
        "served the exact ciphertext"
    );

    let recovered = decrypt_asset_vec(&FILE_KEY, &meta.nonce_prefix, &body).expect("decrypt");
    assert_eq!(recovered, plaintext, "ciphertext decrypts to the plaintext");
}

/// **Ranged mid-file chunk (the encryption doc's ranged-read acceptance).** Fetch interior
/// chunk `1` with `Range: bytes={1×65536}-{2×65536−1}`: assert `206` + `Content-Range`, then
/// decrypt that chunk *in isolation* with core's `decrypt_chunk` and assert it equals the
/// matching plaintext slice — proving a mid-file range decrypts without the full file.
#[tokio::test]
async fn ranged_mid_file_chunk_decrypts_without_full_file() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let plaintext = fixture_plaintext();
    let (meta, ciphertext) = encrypt_asset_vec_full(&FILE_KEY, &plaintext);
    let hash = ctx.finalize_blob(&asset_id, "original", &ciphertext).await;

    // Chunk 1 is a full interior chunk (not the first, not the last).
    let chunk_index: u32 = 1;
    let start = u64::from(chunk_index) * CIPHERTEXT_CHUNK as u64;
    let end = start + CIPHERTEXT_CHUNK as u64 - 1; // inclusive per RFC 7233

    let svc = ctx.blob_service();
    let mut res = get(&format!("http://localhost/{hash}"), &ctx.token())
        .add_header("Range", format!("bytes={start}-{end}"), true)
        .send(&svc)
        .await;

    assert_eq!(
        res.status_code,
        Some(StatusCode::PARTIAL_CONTENT),
        "a range yields 206"
    );
    let content_range = res
        .headers()
        .get("content-range")
        .and_then(|v| v.to_str().ok())
        .expect("content-range header")
        .to_string();
    assert_eq!(
        content_range,
        format!("bytes {start}-{end}/{}", ciphertext.len()),
        "content-range names the served interval and total size"
    );

    let chunk = res.take_bytes(None).await.expect("chunk bytes");
    assert_eq!(chunk.len(), CIPHERTEXT_CHUNK, "one full ciphertext stride");

    // Decrypt this interior chunk alone — no other chunk, not the whole file.
    let pt = decrypt_chunk(&FILE_KEY, &meta.nonce_prefix, chunk_index, false, &chunk)
        .expect("mid-file chunk decrypts in isolation");
    let p_start = chunk_index as usize * PLAINTEXT_CHUNK;
    let p_end = p_start + PLAINTEXT_CHUNK;
    assert_eq!(
        pt,
        &plaintext[p_start..p_end],
        "the interior chunk decrypts to its plaintext slice"
    );
}

/// **Stitched ranged reads (unit).** Every stride-aligned range, fetched independently and
/// concatenated, byte-matches a sequential decrypt of the whole ciphertext — including the
/// final partial chunk (the last-block flag). This is the resumable-download guarantee.
#[tokio::test]
async fn ranged_reads_stitch_to_sequential_decrypt() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let plaintext = fixture_plaintext();
    let (meta, ciphertext) = encrypt_asset_vec_full(&FILE_KEY, &plaintext);
    let hash = ctx.finalize_blob(&asset_id, "original", &ciphertext).await;
    let svc = ctx.blob_service();

    let n_chunks = ciphertext.len().div_ceil(CIPHERTEXT_CHUNK);
    let mut stitched = Vec::with_capacity(plaintext.len());
    for i in 0..n_chunks {
        let start = i * CIPHERTEXT_CHUNK;
        let end = ((i + 1) * CIPHERTEXT_CHUNK).min(ciphertext.len()) - 1;
        let is_last = i == n_chunks - 1;

        let mut res = get(&format!("http://localhost/{hash}"), &ctx.token())
            .add_header("Range", format!("bytes={start}-{end}"), true)
            .send(&svc)
            .await;
        assert_eq!(
            res.status_code,
            Some(StatusCode::PARTIAL_CONTENT),
            "chunk {i}"
        );
        let chunk = res.take_bytes(None).await.expect("chunk bytes");
        let pt = decrypt_chunk(&FILE_KEY, &meta.nonce_prefix, i as u32, is_last, &chunk)
            .unwrap_or_else(|_| panic!("chunk {i} decrypts"));
        stitched.extend_from_slice(&pt);
    }

    let sequential = decrypt_asset_vec(&FILE_KEY, &meta.nonce_prefix, &ciphertext).expect("seq");
    assert_eq!(
        stitched, sequential,
        "stitched ranges == sequential decrypt"
    );
    assert_eq!(stitched, plaintext, "and == the original plaintext");
}

/// **Unknown content address → 404.** A well-formed hash the server never took custody of is
/// unknown — not an oracle, not a 410.
#[tokio::test]
async fn unknown_hash_is_404() {
    let ctx = setup().await;
    let bogus = TestCtx::address(b"a blob the server never held");
    let svc = ctx.blob_service();
    let res = get(&format!("http://localhost/{bogus}"), &ctx.token())
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
}

/// **Malformed content address → 404.** A non-hex / wrong-length path segment is answered as
/// unknown before any lookup — no blob-existence oracle, no 500.
#[tokio::test]
async fn malformed_hash_is_404() {
    let ctx = setup().await;
    let svc = ctx.blob_service();
    let res = get("http://localhost/not-a-valid-hash", &ctx.token())
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::NOT_FOUND));
}

/// **Quarantined blob → 410 Gone.** An integrity-quarantined blob is taken down per policy,
/// even though its bytes are still on disk.
#[tokio::test]
async fn quarantined_is_410() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let hash = ctx
        .finalize_blob(&asset_id, "original", b"quarantined ciphertext")
        .await;
    ctx.mark_gc(&hash, None, true).await;

    let svc = ctx.blob_service();
    let res = get(&format!("http://localhost/{hash}"), &ctx.token())
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::GONE));
}

/// **Mid-GC blob → 410 Gone.** A blob marked `collectable_since` (refcount hit zero) is not
/// retrievable and is never served, even while its bytes survive the GC grace window.
#[tokio::test]
async fn mid_gc_is_410() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let hash = ctx
        .finalize_blob(&asset_id, "original", b"collectable ciphertext")
        .await;
    ctx.mark_gc(&hash, Some(jiff::Timestamp::now()), false)
        .await;

    let svc = ctx.blob_service();
    let res = get(&format!("http://localhost/{hash}"), &ctx.token())
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::GONE));
}

/// **Dangling reference → 410 Gone.** An indexed original whose `original_held = true` but
/// whose bytes are absent from disk is a dangling reference — gone, **not** the transient
/// awaiting-original state.
#[tokio::test]
async fn dangling_reference_is_410() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    // The client says the original is held, but the bytes are not on disk.
    let hash = TestCtx::address(b"an original that vanished");
    ctx.index_original(&asset_id, &hash, 64, true).await;

    let svc = ctx.blob_service();
    let res = get(&format!("http://localhost/{hash}"), &ctx.token())
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::GONE));
}

/// **Awaiting-original → pending_upload, never 410.** A staged asset whose original has not
/// yet landed (`original_held = false`, bytes absent) returns the transient
/// `error.blob.pending_upload` (a `409` carrying the code), explicitly **distinct from 410**.
#[tokio::test]
async fn awaiting_original_is_pending_upload_not_410() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let hash = TestCtx::address(b"an original still on the phone");
    ctx.index_original(&asset_id, &hash, 64, false).await;

    let svc = ctx.blob_service();
    let mut res = get(&format!("http://localhost/{hash}"), &ctx.token())
        .send(&svc)
        .await;

    assert_ne!(
        res.status_code,
        Some(StatusCode::GONE),
        "awaiting-original is explicitly NOT 410"
    );
    assert_eq!(res.status_code, Some(StatusCode::CONFLICT));
    let body = res.take_json::<Value>().await.expect("error body");
    assert_eq!(
        body["code"].as_str(),
        Some(error_codes::BLOB_PENDING_UPLOAD),
        "carries the transient pending-upload code the client badges on"
    );
}

/// **Auth per route → 401.** No bearer token, no ciphertext.
#[tokio::test]
async fn missing_token_is_401() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let hash = ctx
        .finalize_blob(&asset_id, "original", b"protected ciphertext")
        .await;

    let svc = ctx.blob_service();
    let res = TestClient::get(format!("http://localhost/{hash}"))
        .send(&svc)
        .await;
    assert_eq!(res.status_code, Some(StatusCode::UNAUTHORIZED));
}
