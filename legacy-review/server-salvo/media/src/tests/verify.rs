//! The storage-verification Validation bullets (slice `S-C3`). See `tests/mod.rs` for the
//! coverage map. Each of the doc's six unsigned-verdict bullets is one `#[tokio::test]`.

use std::sync::Arc;

use jiff::{SignedDuration, Timestamp};
use salvo::http::StatusCode;
use salvo::test::{ResponseExt, TestClient};
use serde_json::{Value, json};

use super::{GatedHasher, MockClock, TestCtx, setup};
use crate::service::verify::{
    AssetQuery, BlobRole, VerificationService, VerifyError, VerifyLimits,
};

/// A single-asset query helper.
fn query(asset_id: &str, hashes: &[&str]) -> Vec<AssetQuery> {
    vec![AssetQuery {
        asset_id: asset_id.to_string(),
        blob_hashes: hashes.iter().map(|h| (*h).to_string()).collect(),
    }]
}

/// **Durable verdict (smoke).** Upload an asset to completion (original + metadata blobs
/// finalized and on disk); `POST /storage/verify` with its blob hashes; assert
/// `durable = true` and every blob `stored && indexed && retrievable`. Driven end-to-end
/// through the real HTTP router.
#[tokio::test]
async fn durable_verdict() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let original = ctx
        .finalize_blob(&asset_id, "original", b"original ciphertext bytes")
        .await;
    let metadata = ctx
        .finalize_blob(&asset_id, "metadata", b"metadata ciphertext bytes")
        .await;

    let svc = ctx.http_service();
    let mut res = TestClient::post("http://localhost/verify")
        .add_header("Authorization", format!("Bearer {}", ctx.token()), true)
        .json(&json!({
            "assets": [{ "asset_id": asset_id, "blob_hashes": [original, metadata] }],
        }))
        .send(&svc)
        .await;

    assert_eq!(res.status_code, Some(StatusCode::OK));
    let body = res.take_json::<Value>().await.expect("verdict body");
    let verdict = &body["verdicts"][0];
    assert_eq!(verdict["durable"], json!(true), "asset is durable");
    assert!(!verdict["checked_at"].as_str().unwrap().is_empty());
    for blob in verdict["blobs"].as_array().unwrap() {
        assert_eq!(blob["stored"], json!(true));
        assert_eq!(blob["indexed"], json!(true));
        assert_eq!(blob["retrievable"], json!(true));
    }
}

/// **Partial / missing verdict (unit).** Verify an asset whose metadata blob never
/// finalized; assert `durable = false` with the metadata blob `indexed = false` (and
/// `stored = false`), and the original still reported accurately.
#[tokio::test]
async fn partial_missing_verdict() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let original = ctx
        .finalize_blob(&asset_id, "original", b"original bytes")
        .await;
    // The client computed the intended metadata address locally, but it never finalized.
    let metadata = TestCtx::address(b"metadata that never uploaded");

    let verdicts = ctx
        .service()
        .verify(
            &ctx.db,
            &ctx.user_id,
            &query(&asset_id, &[&original, &metadata]),
            false,
        )
        .await
        .expect("verify");
    let v = &verdicts[0];
    assert!(!v.durable, "a missing required blob is not durable");

    let orig = v.blobs.iter().find(|b| b.hash == original).unwrap();
    assert!(
        orig.stored && orig.indexed && orig.retrievable,
        "original reported accurately"
    );
    assert_eq!(orig.role, BlobRole::Original);

    let meta = v.blobs.iter().find(|b| b.hash == metadata).unwrap();
    assert!(!meta.indexed, "never-finalized metadata is not indexed");
    assert!(!meta.stored, "and its bytes are not on disk");
    assert!(!meta.retrievable);
}

/// **Mid-GC blob (unit).** Mark a referenced, stored blob `collectable_since`; assert it
/// reports `retrievable = false` and the asset is `durable = false`.
#[tokio::test]
async fn mid_gc_blob() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let original = ctx
        .finalize_blob(&asset_id, "original", b"collectable bytes")
        .await;
    ctx.mark_gc(&original, Some(Timestamp::now()), false).await;

    let verdicts = ctx
        .service()
        .verify(
            &ctx.db,
            &ctx.user_id,
            &query(&asset_id, &[&original]),
            false,
        )
        .await
        .expect("verify");
    let blob = &verdicts[0].blobs[0];
    assert!(blob.stored && blob.indexed, "still stored + indexed");
    assert!(!blob.retrievable, "but mid-GC ⇒ not retrievable");
    assert!(!verdicts[0].durable);
}

/// **Quarantined blob (unit).** The retrievable check also fails on an integrity-quarantined
/// blob.
#[tokio::test]
async fn quarantined_blob() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let original = ctx
        .finalize_blob(&asset_id, "original", b"quarantined bytes")
        .await;
    ctx.mark_gc(&original, None, true).await;

    let verdicts = ctx
        .service()
        .verify(
            &ctx.db,
            &ctx.user_id,
            &query(&asset_id, &[&original]),
            false,
        )
        .await
        .expect("verify");
    let blob = &verdicts[0].blobs[0];
    assert!(!blob.retrievable, "quarantined ⇒ not retrievable");
    assert!(!verdicts[0].durable);
}

/// **Wrong-hash declaration (unit).** Declare a blob hash the server does not hold for the
/// asset; assert `stored = false, indexed = false` (role `unknown`) rather than a silent
/// omission — the declared hash is surfaced.
#[tokio::test]
async fn wrong_hash_declaration() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let original = ctx
        .finalize_blob(&asset_id, "original", b"real bytes")
        .await;
    let bogus = TestCtx::address(b"a hash the server never took custody of");

    let verdicts = ctx
        .service()
        .verify(
            &ctx.db,
            &ctx.user_id,
            &query(&asset_id, &[&original, &bogus]),
            false,
        )
        .await
        .expect("verify");
    let v = &verdicts[0];
    assert_eq!(
        v.blobs.len(),
        2,
        "both declared hashes are surfaced, none omitted"
    );

    let bad = v.blobs.iter().find(|b| b.hash == bogus).unwrap();
    assert!(!bad.stored, "server does not hold it");
    assert!(!bad.indexed, "no committed row references it");
    assert_eq!(bad.role, BlobRole::Unknown);
    assert!(!v.durable);
}

/// **Verify-before-destroy gate (smoke), server half.** The endpoint is the durability
/// signal the client-side release gate consumes: while a required blob is unfinalized the
/// asset is non-`durable` (the client refuses to evict); once it finalizes the verdict flips
/// to `durable` (the release may proceed). Proven against the real server + blob tree.
#[tokio::test]
async fn verify_before_destroy_signal() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let original = ctx
        .finalize_blob(&asset_id, "original", b"device-owned original")
        .await;
    let metadata_bytes = b"metadata blob bytes";
    let metadata = TestCtx::address(metadata_bytes);

    // Before the metadata finalizes: non-durable — the release gate holds the local copy.
    let before = ctx
        .service()
        .verify(
            &ctx.db,
            &ctx.user_id,
            &query(&asset_id, &[&original, &metadata]),
            false,
        )
        .await
        .expect("verify");
    assert!(
        !before[0].durable,
        "release must NOT proceed while unconfirmed"
    );

    // The blob finalizes (bytes land + indexed).
    ctx.finalize_blob(&asset_id, "metadata", metadata_bytes)
        .await;

    // Now durable — the release may proceed.
    let after = ctx
        .service()
        .verify(
            &ctx.db,
            &ctx.user_id,
            &query(&asset_id, &[&original, &metadata]),
            false,
        )
        .await
        .expect("verify");
    assert!(after[0].durable, "release proceeds once confirmed durable");
}

/// **Deep scan (unit).** Corrupt a stored blob's bytes on disk; assert the structural check
/// still reports `stored = true` but `deep = true` reports the hash mismatch as
/// `stored = false`.
#[tokio::test]
async fn deep_scan_detects_bitrot() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let clean = b"the ciphertext the client relies on";
    let hash = TestCtx::address(clean);
    // Index the intended hash, but plant divergent bytes at its content address.
    ctx.index_blob(&asset_id, "original", &hash, clean.len() as u64)
        .await;
    ctx.write_blob_bytes(&hash, b"silently rotted bytes");

    // Structural: the file exists at the address ⇒ stored = true.
    let structural = ctx
        .service()
        .verify(&ctx.db, &ctx.user_id, &query(&asset_id, &[&hash]), false)
        .await
        .expect("verify");
    assert!(
        structural[0].blobs[0].stored,
        "structural stat trusts presence"
    );

    // Deep: re-hash catches the bit-rot ⇒ stored = false.
    let deep = ctx
        .service()
        .verify(&ctx.db, &ctx.user_id, &query(&asset_id, &[&hash]), true)
        .await
        .expect("verify");
    assert!(
        !deep[0].blobs[0].stored,
        "deep re-hash reports the mismatch"
    );
    assert!(!deep[0].durable);
}

/// **Deep-scan coalescing (S-C3 pricing).** Two concurrent `deep` requests for the same blob
/// share **one** re-hash — proven with an injected gated hasher whose invocation count must
/// be exactly 1. No sleeps: the first request parks inside the hasher (holding the per-hash
/// gate); the second necessarily coalesces onto its result.
#[tokio::test]
async fn deep_scan_coalesces_concurrent_rehashes() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let bytes = b"coalesced blob";
    let hash = TestCtx::address(bytes);
    ctx.write_blob_bytes(&hash, bytes);
    ctx.index_blob(&asset_id, "original", &hash, bytes.len() as u64)
        .await;

    let hasher = GatedHasher::new();
    let clock = MockClock::new("2026-07-10T00:00:00Z".parse().unwrap());
    let svc = VerificationService::with_seams(
        ctx.upload_dir.clone(),
        VerifyLimits {
            deep_max_per_window: 100,
            ..VerifyLimits::default()
        },
        Arc::new(clock),
        Arc::new(hasher.clone()),
    );

    let db = ctx.db.clone();
    let user = ctx.user_id.clone();
    let q = query(&asset_id, &[&hash]);

    let (svc1, db1, user1, q1) = (svc.clone(), db.clone(), user.clone(), q.clone());
    let task1 = tokio::spawn(async move { svc1.verify(&db1, &user1, &q1, true).await });

    // Wait until request 1 is parked inside the hasher, holding the per-hash gate.
    hasher.wait_entered().await;

    let (svc2, db2, user2, q2) = (svc.clone(), db.clone(), user.clone(), q.clone());
    let task2 = tokio::spawn(async move { svc2.verify(&db2, &user2, &q2, true).await });

    // Release the parked re-hash; both requests complete.
    hasher.release();
    let v1 = task1.await.unwrap().expect("verify 1");
    let v2 = task2.await.unwrap().expect("verify 2");

    assert_eq!(
        hasher.invocations(),
        1,
        "concurrent deep scans share ONE re-hash"
    );
    assert!(
        v1[0].blobs[0].stored && v2[0].blobs[0].stored,
        "both see the shared verdict"
    );
}

/// **Deep-scan rate limit (S-C3 pricing).** Deep re-hashes are budgeted per user per window.
/// With a budget of 1, a second distinct-blob deep request in the same window is refused;
/// advancing the injected clock past the window restores the budget. No sleeps.
#[tokio::test]
async fn deep_scan_is_rate_limited_per_user() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let a = ctx.finalize_blob(&asset_id, "original", b"blob A").await;
    let b = ctx.finalize_blob(&asset_id, "derivative", b"blob B").await;

    let clock = MockClock::new("2026-07-10T00:00:00Z".parse().unwrap());
    let window = SignedDuration::from_secs(60);
    let svc = VerificationService::with_seams(
        ctx.upload_dir.clone(),
        VerifyLimits {
            deep_max_per_window: 1,
            deep_window: window,
            coalesce_window: window,
        },
        Arc::new(clock.clone()),
        Arc::new(crate::service::verify::FsBlobHasher),
    );

    // First deep re-hash spends the single budgeted token.
    svc.verify(&ctx.db, &ctx.user_id, &query(&asset_id, &[&a]), true)
        .await
        .expect("first deep scan within budget");

    // A second distinct blob in the same window exceeds the budget.
    let err = svc
        .verify(&ctx.db, &ctx.user_id, &query(&asset_id, &[&b]), true)
        .await
        .expect_err("second deep scan is rate limited");
    assert!(matches!(err, VerifyError::DeepRateLimited));

    // Advancing past the window resets the budget.
    clock.advance(window + SignedDuration::from_secs(1));
    svc.verify(&ctx.db, &ctx.user_id, &query(&asset_id, &[&b]), true)
        .await
        .expect("budget restored in the next window");
}

/// **GC-grace seam (S-C11 consumes this).** A blob that answers `durable` has no
/// `collectable_since`, so the earliest the GC worker could byte-delete it — even if it were
/// marked collectable the instant after the verdict — is a full grace window out, safely past
/// the client's bounded verify→release window. This is the contract `S-C11` gates its
/// deletion sweep on (`service::gc::earliest_byte_deletion`).
#[tokio::test]
async fn durable_blob_survives_release_window_via_gc_grace() {
    let ctx = setup().await;
    let asset_id = nanoid::nanoid!();
    let hash = ctx
        .finalize_blob(&asset_id, "original", b"just-verified blob")
        .await;

    let verdicts = ctx
        .service()
        .verify(&ctx.db, &ctx.user_id, &query(&asset_id, &[&hash]), false)
        .await
        .expect("verify");
    assert!(verdicts[0].durable);
    let checked_at = verdicts[0].checked_at;

    // A durable verdict implies no GC row / no collectable_since at checked_at.
    let state = service::gc::Query::blob_states(&ctx.db, std::slice::from_ref(&hash))
        .await
        .unwrap();
    assert!(
        state
            .get(&hash)
            .copied()
            .unwrap_or_default()
            .is_retrievable()
    );

    // Worst case: the GC worker marks it collectable one instant after the verdict. Its bytes
    // still survive a full grace window — past the client's 60 s release window.
    let marked = checked_at
        .checked_add(SignedDuration::from_nanos(1))
        .unwrap();
    let release_deadline = checked_at
        .checked_add(SignedDuration::from_secs(60))
        .unwrap();
    assert!(service::gc::earliest_byte_deletion(marked) > release_deadline);
}
