//! Mocked tests for the transfer download doc's client fetch Validation bullets
//! (slice `S-D2`): tiered fetch correctness, cross-asset dedup, resume-after-
//! interrupt (zero duplicate bytes), above-tier permanent unavailability with the
//! degrade ladder and automatic re-fetch, the `403` authorization-change path, the
//! `awaiting-original` pending state, and content-hash self-verification. All
//! deterministic — driven through the [`BlobSource`] seam, never a live socket.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};

use super::*;
use crate::net::{MonotonicClock, Sleeper, StallConfig};

fn sha(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn bytes(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// A cache holding a fixed set of content addresses.
struct SetCache(HashSet<String>);
impl BlobCache for SetCache {
    fn contains(&self, hash: &str) -> bool {
        self.0.contains(hash)
    }
}

/// A source that serves `data` in fixed-size windows, dropping the stream (a
/// [`RangeOutcome::Partial`]) after every window but the last, and recording every
/// requested start offset so the test can prove zero duplicate bytes.
struct ChunkedSource {
    data: Vec<u8>,
    window: usize,
    requested_starts: Mutex<Vec<u64>>,
}

impl BlobSource for ChunkedSource {
    async fn get_range(&self, _hash: &str, start: u64, max_len: Option<u64>) -> RangeOutcome {
        self.requested_starts.lock().unwrap().push(start);
        let start = start as usize;
        if start >= self.data.len() {
            return RangeOutcome::Complete { bytes: Vec::new() };
        }
        // Honor a bounded window when the client asks for one, but never exceed the
        // source's own window.
        let cap = max_len.map_or(self.window, |m| (m as usize).min(self.window));
        let end = (start + cap).min(self.data.len());
        let chunk = self.data[start..end].to_vec();
        if end == self.data.len() {
            RangeOutcome::Complete { bytes: chunk }
        } else {
            RangeOutcome::Partial { bytes: chunk }
        }
    }
}

/// A source that always answers a fixed status (for `410`/`403`/pending).
struct AlwaysStatus {
    status: u16,
    code: Option<String>,
}

impl BlobSource for AlwaysStatus {
    async fn get_range(&self, _hash: &str, _start: u64, _max_len: Option<u64>) -> RangeOutcome {
        RangeOutcome::Status {
            status: self.status,
            code: self.code.clone(),
        }
    }
}

/// A source that answers `410 Gone` until `restored`, then serves `data` whole.
struct GoneThenServes {
    data: Vec<u8>,
    restored: Mutex<bool>,
}

impl BlobSource for GoneThenServes {
    async fn get_range(&self, _hash: &str, start: u64, _max_len: Option<u64>) -> RangeOutcome {
        if *self.restored.lock().unwrap() {
            let start = start as usize;
            RangeOutcome::Complete {
                bytes: self.data[start.min(self.data.len())..].to_vec(),
            }
        } else {
            RangeOutcome::Status {
                status: 410,
                code: None,
            }
        }
    }
}

/// A source that answers `403` for the first `deny` calls, then serves `data`.
struct ForbiddenThenServes {
    data: Vec<u8>,
    deny: Mutex<u32>,
}

impl BlobSource for ForbiddenThenServes {
    async fn get_range(&self, _hash: &str, start: u64, _max_len: Option<u64>) -> RangeOutcome {
        let mut deny = self.deny.lock().unwrap();
        if *deny > 0 {
            *deny -= 1;
            RangeOutcome::Status {
                status: 403,
                code: None,
            }
        } else {
            let start = start as usize;
            RangeOutcome::Complete {
                bytes: self.data[start.min(self.data.len())..].to_vec(),
            }
        }
    }
}

/// A source that returns bytes whose hash will not match the requested address.
struct CorruptingSource {
    len: usize,
}

impl BlobSource for CorruptingSource {
    async fn get_range(&self, _hash: &str, _start: u64, _max_len: Option<u64>) -> RangeOutcome {
        RangeOutcome::Complete {
            bytes: vec![0xFF; self.len],
        }
    }
}

// ─── Tiered fetch + dedup ────────────────────────────────────────────────────

/// **Tiered fetch correctness.** Scope = metadata + thumbnails; an entry with
/// original + thumbnail + LQIP yields exactly the thumbnail fetch — never the
/// original.
#[test]
fn tiered_fetch_plans_only_the_scoped_representations() {
    let asset = AssetBlobs {
        thumbnail: Some(RepBlob {
            hash: "thumb-hash".to_string(),
            size: 1024,
        }),
        preview: Some(RepBlob {
            hash: "preview-hash".to_string(),
            size: 4096,
        }),
        original: Some(RepBlob {
            hash: "original-hash".to_string(),
            size: 1_000_000,
        }),
        ..AssetBlobs::default()
    };

    let planned = plan_eager_fetches(FetchScope::MetadataThumbnails, &asset, &EmptyCache);
    assert_eq!(planned, vec!["thumb-hash".to_string()]);

    // Metadata-only fetches nothing eagerly (LQIP is embedded).
    assert!(plan_eager_fetches(FetchScope::MetadataOnly, &asset, &EmptyCache).is_empty());

    // The full scope adds the original but still never eagerly fetches the preview.
    let full = plan_eager_fetches(FetchScope::MetadataThumbnailsOriginal, &asset, &EmptyCache);
    assert_eq!(
        full,
        vec!["thumb-hash".to_string(), "original-hash".to_string()]
    );
    assert!(!full.contains(&"preview-hash".to_string()));
}

/// **Cross-asset dedup hit.** Two assets share a thumbnail content address; once
/// the first is cached, the second plans no fetch for it.
#[test]
fn shared_thumbnail_is_not_refetched() {
    let shared = RepBlob {
        hash: "shared-thumb".to_string(),
        size: 2048,
    };
    let asset_a = AssetBlobs {
        thumbnail: Some(shared.clone()),
        ..AssetBlobs::default()
    };
    let asset_b = AssetBlobs {
        thumbnail: Some(shared.clone()),
        ..AssetBlobs::default()
    };

    // First asset: the thumbnail is planned.
    let empty = SetCache(HashSet::new());
    assert_eq!(
        plan_eager_fetches(FetchScope::MetadataThumbnails, &asset_a, &empty),
        vec!["shared-thumb".to_string()]
    );

    // After caching it, the second asset's identical thumbnail is skipped.
    let cached = SetCache(HashSet::from(["shared-thumb".to_string()]));
    assert!(plan_eager_fetches(FetchScope::MetadataThumbnails, &asset_b, &cached).is_empty());
}

// ─── Resumable ranged fetch ──────────────────────────────────────────────────

/// **Resume after interrupt.** A large fetch is interrupted repeatedly mid-`Range`;
/// it resumes from the last persisted byte, reassembles a byte-identical result,
/// and re-fetches zero bytes it already holds.
#[tokio::test]
async fn ranged_fetch_resumes_with_zero_duplicate_bytes() {
    let data = bytes(10_000);
    let hash = sha(&data);
    let source = ChunkedSource {
        data: data.clone(),
        window: 3000,
        requested_starts: Mutex::new(Vec::new()),
    };

    let fetched = fetch_blob(&source, &hash, data.len() as u64)
        .await
        .expect("resumable fetch completes");
    assert_eq!(fetched, data, "byte-identical result");

    // Every request resumed from strictly-increasing, contiguous offsets — so no
    // byte was requested twice.
    let starts = source.requested_starts.lock().unwrap().clone();
    assert_eq!(starts, vec![0, 3000, 6000, 9000]);
    assert!(
        starts.windows(2).all(|w| w[0] < w[1]),
        "starts strictly increase"
    );
}

/// The client verifies the content address itself: bytes that do not hash to the
/// requested address are discarded.
#[tokio::test]
async fn ciphertext_hash_mismatch_is_discarded() {
    let source = CorruptingSource { len: 512 };
    let err = fetch_blob(&source, &sha(&bytes(512)), 512)
        .await
        .expect_err("integrity failure");
    assert!(matches!(err, FetchError::IntegrityFailed { .. }));
}

// ─── Degrade ladder ──────────────────────────────────────────────────────────

/// The degrade target steps down preview → thumbnail → LQIP.
#[test]
fn best_available_degrades_down_the_ladder() {
    let all = BTreeSet::from([
        Representation::Lqip,
        Representation::Thumbnail,
        Representation::Preview,
    ]);
    assert_eq!(
        best_available(Representation::Original, &all),
        Some(Representation::Preview)
    );

    let thumb_only = BTreeSet::from([Representation::Lqip, Representation::Thumbnail]);
    assert_eq!(
        best_available(Representation::Original, &thumb_only),
        Some(Representation::Thumbnail)
    );

    let lqip_only = BTreeSet::from([Representation::Lqip]);
    assert_eq!(
        best_available(Representation::Preview, &lqip_only),
        Some(Representation::Lqip)
    );
}

/// **Above-tier permanent unavailability.** With the original on-demand, `410`
/// degrades to the best locally-held representation, surfaces the unavailable
/// state, and leaves the asset intact; restoring availability re-fetches.
#[tokio::test]
async fn permanent_unavailability_degrades_then_refetches() {
    let original = bytes(4096);
    let asset = AssetBlobs {
        thumbnail: Some(RepBlob {
            hash: "t".to_string(),
            size: 256,
        }),
        preview: None,
        original: Some(RepBlob {
            hash: sha(&original),
            size: original.len() as u64,
        }),
        ..AssetBlobs::default()
    };
    let held = BTreeSet::from([Representation::Lqip, Representation::Thumbnail]);

    let source = GoneThenServes {
        data: original.clone(),
        restored: Mutex::new(false),
    };

    // While gone, opening the original degrades to the thumbnail — the asset stays
    // listed (its blob manifest is untouched: `asset` is still fully populated).
    let resolution = open_representation(
        &source,
        &asset,
        Representation::Original,
        &held,
        &no_keys(),
        async || {},
    )
    .await;
    assert_eq!(
        resolution,
        FetchResolution::Degraded {
            shown: Representation::Thumbnail,
            reason: DegradeReason::PermanentlyGone,
        }
    );
    assert!(asset.original.is_some(), "metadata/index entry left intact");

    // Availability restored → the original re-fetches automatically.
    *source.restored.lock().unwrap() = true;
    let resolution = open_representation(
        &source,
        &asset,
        Representation::Original,
        &held,
        &no_keys(),
        async || {},
    )
    .await;
    match resolution {
        FetchResolution::Fetched {
            representation,
            bytes,
        } => {
            assert_eq!(representation, Representation::Original);
            assert_eq!(bytes, original);
        }
        other => panic!("expected Fetched, got {other:?}"),
    }
}

/// A `403` triggers a membership re-sync then a retry; a still-forbidden asset
/// degrades and surfaces the authorization change (not a missing file).
#[tokio::test]
async fn forbidden_resyncs_membership_then_degrades_when_still_denied() {
    let asset = AssetBlobs {
        thumbnail: Some(RepBlob {
            hash: "t".to_string(),
            size: 64,
        }),
        preview: None,
        original: Some(RepBlob {
            hash: "o".to_string(),
            size: 4096,
        }),
        ..AssetBlobs::default()
    };
    let held = BTreeSet::from([Representation::Lqip, Representation::Thumbnail]);

    // Denied on both the initial attempt and the post-resync retry (asset unshared).
    let source = AlwaysStatus {
        status: 403,
        code: None,
    };
    let resyncs = std::cell::Cell::new(0u32);
    let resolution = open_representation(
        &source,
        &asset,
        Representation::Original,
        &held,
        &no_keys(),
        async || {
            resyncs.set(resyncs.get() + 1);
        },
    )
    .await;

    assert_eq!(
        resyncs.get(),
        1,
        "membership was re-synced before degrading"
    );
    assert_eq!(
        resolution,
        FetchResolution::Degraded {
            shown: Representation::Thumbnail,
            reason: DegradeReason::AuthorizationChanged,
        }
    );
}

/// A `403` that clears after the membership re-sync fetches on the retry.
#[tokio::test]
async fn forbidden_then_authorized_fetches_after_resync() {
    let original = bytes(2048);
    let asset = AssetBlobs {
        thumbnail: None,
        preview: None,
        original: Some(RepBlob {
            hash: sha(&original),
            size: original.len() as u64,
        }),
        ..AssetBlobs::default()
    };
    let held = BTreeSet::from([Representation::Lqip]);

    let source = ForbiddenThenServes {
        data: original.clone(),
        deny: Mutex::new(1),
    };
    let resyncs = std::cell::Cell::new(0u32);
    let resolution = open_representation(
        &source,
        &asset,
        Representation::Original,
        &held,
        &no_keys(),
        async || {
            resyncs.set(resyncs.get() + 1);
        },
    )
    .await;

    assert_eq!(resyncs.get(), 1);
    match resolution {
        FetchResolution::Fetched { bytes, .. } => assert_eq!(bytes, original),
        other => panic!("expected Fetched after re-sync, got {other:?}"),
    }
}

/// **awaiting-original semantics.** Fetching an original that has not landed yet
/// surfaces the transient pending state (distinct from `410`) and never removes the
/// asset — the badge shows at the best-held representation.
#[tokio::test]
async fn pending_upload_is_distinct_from_gone() {
    let asset = AssetBlobs {
        thumbnail: Some(RepBlob {
            hash: "t".to_string(),
            size: 64,
        }),
        preview: None,
        original: Some(RepBlob {
            hash: "o".to_string(),
            size: 4096,
        }),
        ..AssetBlobs::default()
    };
    let held = BTreeSet::from([Representation::Lqip, Representation::Thumbnail]);

    let pending = AlwaysStatus {
        status: 409,
        code: Some(capsule_i18n::error_codes::BLOB_PENDING_UPLOAD.to_string()),
    };
    let resolution = open_representation(
        &pending,
        &asset,
        Representation::Original,
        &held,
        &no_keys(),
        async || {},
    )
    .await;
    assert_eq!(
        resolution,
        FetchResolution::Pending {
            shown: Representation::Thumbnail,
        },
        "pending is a badge, never a failure or a removal"
    );

    // The same shape under `410` is a *degrade*, not pending — the two are distinct.
    let gone = AlwaysStatus {
        status: 410,
        code: None,
    };
    let resolution = open_representation(
        &gone,
        &asset,
        Representation::Original,
        &held,
        &no_keys(),
        async || {},
    )
    .await;
    assert!(matches!(
        resolution,
        FetchResolution::Degraded {
            reason: DegradeReason::PermanentlyGone,
            ..
        }
    ));
}

// ─── Adverse-network stall-cut-resume (S-D10) ────────────────────────────────

/// A mock monotonic clock the source advances; the fetch loop reads the same cell,
/// so the stall cut is deterministic with no wall-clock sleep.
#[derive(Clone)]
struct SharedClock(Arc<Mutex<u64>>);

impl MonotonicClock for SharedClock {
    fn now_millis(&self) -> u64 {
        *self.0.lock().unwrap()
    }
}

/// A no-op sleeper that records the backoffs it was asked to perform — proving the
/// loop backed off between stalled attempts without any real sleep.
#[derive(Clone)]
struct NoopSleeper(Arc<Mutex<Vec<Duration>>>);

impl Sleeper for NoopSleeper {
    async fn sleep(&self, dur: Duration) {
        self.0.lock().unwrap().push(dur);
    }
}

/// A source that serves `data` in bounded windows (honoring the client's `max_len`),
/// but **black-holes** the request that resumes at `blackhole_at` for the first
/// `blackholes` attempts — returning no bytes while the clock advances — before
/// finally serving. Records every window it actually serves so the test can prove
/// zero duplicate bytes.
struct BlackHoleThenResumes {
    data: Vec<u8>,
    clock: SharedClock,
    tick: u64,
    blackhole_at: u64,
    blackholes: Mutex<u32>,
    served: Mutex<Vec<(u64, usize)>>,
}

impl BlobSource for BlackHoleThenResumes {
    async fn get_range(&self, _hash: &str, start: u64, max_len: Option<u64>) -> RangeOutcome {
        // Every read costs time (the client's stall clock advances off this).
        {
            let mut now = self.clock.0.lock().unwrap();
            *now += self.tick;
        }
        // Black-hole the resume point until the budget is spent: no bytes, so the
        // client resumes from the same offset (which must not re-fetch held bytes).
        if start == self.blackhole_at {
            let mut remaining = self.blackholes.lock().unwrap();
            if *remaining > 0 {
                *remaining -= 1;
                return RangeOutcome::Partial { bytes: Vec::new() };
            }
        }
        let start_us = start as usize;
        if start_us >= self.data.len() {
            return RangeOutcome::Complete { bytes: Vec::new() };
        }
        // The client MUST bound the window under adverse; assert it did.
        let window = max_len.expect("adverse fetch requests a bounded window") as usize;
        assert!(window > 0);
        let end = (start_us + window).min(self.data.len());
        self.served.lock().unwrap().push((start, end - start_us));
        let chunk = self.data[start_us..end].to_vec();
        if end == self.data.len() {
            RangeOutcome::Complete { bytes: chunk }
        } else {
            RangeOutcome::Partial { bytes: chunk }
        }
    }
}

/// **Stall-cut-resume with zero duplicate bytes (networking Validation).** On an
/// `adverse` link the fetcher requests bounded windows; a black-holed window is cut
/// by the no-bytes-for-T stall detector within its bound, the loop backs off through
/// the shared retry engine and resumes from the persisted offset — and every byte is
/// transferred exactly once (zero duplicates). The stall surfaces a
/// [`TransferSignal::Stall`] that folds into `adverse` promotion.
#[tokio::test]
async fn adverse_stall_cut_resumes_with_zero_duplicate_bytes() {
    let data = bytes(2000);
    let hash = sha(&data);
    let window = 512u64; // an explicit small window; the source asserts one is set

    let clock = SharedClock(Arc::new(Mutex::new(0)));
    let backoffs = Arc::new(Mutex::new(Vec::new()));
    let source = BlackHoleThenResumes {
        data: data.clone(),
        clock: clock.clone(),
        tick: 100,            // each read advances the stall clock 100 ms
        blackhole_at: window, // black-hole the request resuming after window 0
        blackholes: Mutex::new(5),
        served: Mutex::new(Vec::new()),
    };

    // Stall bound 500 ms: after window 0 lands (t=100) the black-holes at
    // t=200..600 cross the bound at t=600 → a cut within the bound.
    let fetcher = RangedFetcher::from_parts(
        ConnectionClass::Adverse,
        StallConfig::new(Duration::from_millis(500)),
        Some(window),
        NoopSleeper(backoffs.clone()),
        clock,
    );

    let (fetched, signals) = fetcher
        .fetch_with_signals(&source, &hash, data.len() as u64)
        .await
        .expect("stall-cut fetch resumes and completes");

    // Byte-identical result.
    assert_eq!(fetched, data);

    // The stall detector cut at least once (no-bytes-for-T), surfacing a Stall
    // signal for adverse promotion; and it backed off through the shared engine.
    assert!(
        signals.contains(&TransferSignal::Stall),
        "a stall must surface for adverse promotion: {signals:?}"
    );
    assert!(
        !backoffs.lock().unwrap().is_empty(),
        "the loop backed off between stalled attempts"
    );

    // Zero duplicate bytes: the served windows tile [0, len) contiguously — each
    // byte delivered exactly once, the black-holed reads served nothing.
    let served = source.served.lock().unwrap().clone();
    let mut cursor = 0u64;
    let mut total = 0usize;
    for (start, len) in &served {
        assert_eq!(
            *start, cursor,
            "every window resumes from the persisted offset"
        );
        cursor += *len as u64;
        total += *len;
    }
    assert_eq!(total, data.len(), "exactly the blob's bytes, none twice");
    assert!(
        served.iter().all(|(_, len)| *len as u64 <= window),
        "every request stayed within the bounded adverse window"
    );
}

// ─── Opening a derivative (issue #448) ───────────────────────────────────────

/// A reader holding no album key.
fn no_keys() -> BTreeMap<AmkVersion, Amk> {
    BTreeMap::new()
}

const EPOCH: AmkVersion = AmkVersion(3);
const SOURCE_ASSET: uuid::Uuid = uuid::Uuid::from_u128(0x000A_55E7);
const PREFIX: [u8; NONCE_PREFIX_LEN] = [7, 6, 5, 4, 3, 2, 1];

fn album_key() -> Amk {
    Amk::from_bytes([0x5A; 32])
}

fn reader_keys() -> BTreeMap<AmkVersion, Amk> {
    BTreeMap::from([(EPOCH, album_key())])
}

/// Seal `plaintext` the way the write half does: under
/// `AMK(epoch).derive_file_key(source_asset_id, nonce_prefix)`.
fn seal(amk: &Amk, plaintext: &[u8]) -> Vec<u8> {
    let file_key = amk.derive_file_key(&SOURCE_ASSET, &PREFIX);
    capsule_core::crypto::encryption::encrypt_asset_vec_with_prefix(&file_key, PREFIX, plaintext).1
}

/// A signed derivative manifest for `role`, committing to `ciphertext` in `format`.
fn manifest(role: DerivativeRole, format: &str, ciphertext: &[u8]) -> DerivativeManifest {
    use capsule_core::crypto::keys::HybridSigningKey;
    use capsule_core::crypto::provenance::{DERIVATIVE_MANIFEST_VERSION, DerivativeCore};

    let device = HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32]);
    let write = HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32]);
    DerivativeCore {
        version: DERIVATIVE_MANIFEST_VERSION.into(),
        crypto_suite_id: capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
        protocol_version: Some(capsule_core::crypto::primitives::PROTOCOL_VERSION.into()),
        amk_version: Some(EPOCH),
        source_asset_id: SOURCE_ASSET,
        role,
        format: format.into(),
        ciphertext_hash: capsule_core::crypto::hash::hash_bytes(ciphertext),
        nonce_prefix: PREFIX,
        generated_by_device: uuid::Uuid::from_u128(0xD1),
        generated_by_client: "capsule-cli/0.1.0".into(),
        model_id: None,
        model_version: None,
        generated_at: "2026-10-17T00:00:00Z".into(),
        prior_provenance_hash: None,
    }
    .sign(&device, &write)
    .unwrap()
}

/// An asset whose thumbnail is `ciphertext`, described by `manifests`.
fn thumbnail_asset(ciphertext: &[u8], manifests: Vec<DerivativeManifest>) -> AssetBlobs {
    AssetBlobs {
        thumbnail: Some(RepBlob {
            hash: sha(ciphertext),
            size: ciphertext.len() as u64,
        }),
        derivative_manifests: manifests,
        ..AssetBlobs::default()
    }
}

/// A source that serves `data` whole and records every request.
fn serving(data: &[u8]) -> ChunkedSource {
    ChunkedSource {
        data: data.to_vec(),
        window: data.len().max(1),
        requested_starts: Mutex::new(Vec::new()),
    }
}

async fn open_thumbnail(
    source: &ChunkedSource,
    asset: &AssetBlobs,
    keys: &BTreeMap<AmkVersion, Amk>,
) -> FetchResolution {
    let held = BTreeSet::from([Representation::Lqip]);
    open_representation(
        source,
        asset,
        Representation::Thumbnail,
        &held,
        keys,
        async || {},
    )
    .await
}

/// **The read half.** A fetched thumbnail is decrypted with the key its manifest selects,
/// so the caller receives the plaintext still — never AEAD ciphertext.
#[tokio::test]
async fn fetched_thumbnail_is_decrypted_with_its_manifest_key() {
    let plaintext = b"\xff\x0aJXL thumbnail bytes".repeat(400);
    let ciphertext = seal(&album_key(), &plaintext);
    let asset = thumbnail_asset(
        &ciphertext,
        vec![manifest(
            DerivativeRole::Thumbnail,
            "image/jxl",
            &ciphertext,
        )],
    );
    let source = serving(&ciphertext);

    let resolution = open_thumbnail(&source, &asset, &reader_keys()).await;

    assert_eq!(
        resolution,
        FetchResolution::Opened {
            representation: Representation::Thumbnail,
            plaintext,
        }
    );
}

/// A preview opens through its own role's manifest, not the thumbnail's.
#[tokio::test]
async fn preview_opens_through_the_preview_manifest() {
    let thumb = seal(&album_key(), b"thumb");
    let preview_plain = b"preview".repeat(100);
    let preview = seal(&album_key(), &preview_plain);
    let asset = AssetBlobs {
        thumbnail: Some(RepBlob {
            hash: sha(&thumb),
            size: thumb.len() as u64,
        }),
        preview: Some(RepBlob {
            hash: sha(&preview),
            size: preview.len() as u64,
        }),
        original: None,
        derivative_manifests: vec![
            manifest(DerivativeRole::Thumbnail, "image/jxl", &thumb),
            manifest(DerivativeRole::Preview, "image/avif", &preview),
        ],
    };
    let source = serving(&preview);
    let held = BTreeSet::from([Representation::Lqip]);

    let resolution = open_representation(
        &source,
        &asset,
        Representation::Preview,
        &held,
        &reader_keys(),
        async || {},
    )
    .await;

    assert_eq!(
        resolution,
        FetchResolution::Opened {
            representation: Representation::Preview,
            plaintext: preview_plain,
        }
    );
}

/// **Corrupt blob.** Bytes that match their content address but fail AEAD authentication
/// under the manifest's key are discarded and surfaced as corrupt (regenerate from the
/// original) — fetched once, never handed out, never re-fetched into the same failure.
#[tokio::test]
async fn authentication_failure_is_a_corrupt_blob() {
    let foreign = Amk::from_bytes([0x11; 32]);
    let ciphertext = seal(&foreign, b"sealed under some other key");
    let asset = thumbnail_asset(
        &ciphertext,
        vec![manifest(
            DerivativeRole::Thumbnail,
            "image/jxl",
            &ciphertext,
        )],
    );
    let source = serving(&ciphertext);

    let resolution = open_thumbnail(&source, &asset, &reader_keys()).await;

    assert_eq!(
        resolution,
        FetchResolution::Degraded {
            shown: Representation::Lqip,
            reason: DegradeReason::Corrupt,
        }
    );
    assert_eq!(
        *source.requested_starts.lock().unwrap(),
        vec![0],
        "fetched exactly once"
    );
}

/// A blob that would not open after a `403` re-sync admitted the fetch is corrupt, not an
/// authorization change.
#[tokio::test]
async fn corrupt_after_resync_is_reported_as_corrupt() {
    let ciphertext = seal(&Amk::from_bytes([0x11; 32]), b"other key");
    let asset = thumbnail_asset(
        &ciphertext,
        vec![manifest(
            DerivativeRole::Thumbnail,
            "image/jxl",
            &ciphertext,
        )],
    );
    let source = ForbiddenThenServes {
        data: ciphertext.clone(),
        deny: Mutex::new(1),
    };
    let held = BTreeSet::from([Representation::Lqip]);

    let resolution = open_representation(
        &source,
        &asset,
        Representation::Thumbnail,
        &held,
        &reader_keys(),
        async || {},
    )
    .await;

    assert!(matches!(
        resolution,
        FetchResolution::Degraded {
            reason: DegradeReason::Corrupt,
            ..
        }
    ));
}

/// **The `original` sentinel is not a fetch.** A tier whose signed manifest carries
/// `format = "original"` is satisfied by the original, and no request is made.
#[tokio::test]
async fn original_sentinel_is_satisfied_without_a_fetch() {
    let original_ciphertext = seal(&album_key(), b"the original");
    let asset = AssetBlobs {
        derivative_manifests: vec![manifest(
            DerivativeRole::Thumbnail,
            "original",
            &original_ciphertext,
        )],
        ..AssetBlobs::default()
    };
    let source = serving(b"never served");

    let resolution = open_thumbnail(&source, &asset, &no_keys()).await;

    assert_eq!(
        resolution,
        FetchResolution::ReferencesOriginal {
            representation: Representation::Thumbnail,
        }
    );
    assert!(source.requested_starts.lock().unwrap().is_empty());
}

/// The sentinel is found even when the feed lists a blob for the tier and a same-role
/// manifest committing to other bytes comes first: the sentinel outranks an arbitrary
/// manifest of the role, so the tier references the original rather than degrading.
#[tokio::test]
async fn original_sentinel_outranks_a_manifest_for_other_bytes() {
    let listed = seal(&album_key(), b"listed thumbnail");
    let other = seal(&album_key(), b"some other thumbnail");
    let original_ciphertext = seal(&album_key(), b"the original");
    let asset = thumbnail_asset(
        &listed,
        vec![
            manifest(DerivativeRole::Thumbnail, "image/jxl", &other),
            manifest(DerivativeRole::Thumbnail, "original", &original_ciphertext),
        ],
    );
    let source = serving(&listed);

    let resolution = open_thumbnail(&source, &asset, &reader_keys()).await;

    assert_eq!(
        resolution,
        FetchResolution::ReferencesOriginal {
            representation: Representation::Thumbnail,
        }
    );
    assert!(source.requested_starts.lock().unwrap().is_empty());
}

/// **The receiver's closed-format check.** A manifest naming a still format outside the
/// tier table is a structural rejection: nothing is fetched.
#[tokio::test]
async fn unrecognised_still_format_is_rejected_before_fetching() {
    let ciphertext = seal(&album_key(), b"future codec");
    let asset = thumbnail_asset(
        &ciphertext,
        vec![manifest(
            DerivativeRole::Thumbnail,
            "image/future-codec",
            &ciphertext,
        )],
    );
    let source = serving(&ciphertext);

    let resolution = open_thumbnail(&source, &asset, &reader_keys()).await;

    assert_eq!(
        resolution,
        FetchResolution::Degraded {
            shown: Representation::Lqip,
            reason: DegradeReason::ManifestRejected,
        }
    );
    assert!(source.requested_starts.lock().unwrap().is_empty());
}

/// A manifest with no AMK epoch cannot select a key and is rejected without a fetch.
#[tokio::test]
async fn manifest_without_an_epoch_is_rejected() {
    let ciphertext = seal(&album_key(), b"no epoch");
    let mut unbound = manifest(DerivativeRole::Thumbnail, "image/jxl", &ciphertext);
    unbound.core.amk_version = None;
    let asset = thumbnail_asset(&ciphertext, vec![unbound]);
    let source = serving(&ciphertext);

    let resolution = open_thumbnail(&source, &asset, &reader_keys()).await;

    assert!(matches!(
        resolution,
        FetchResolution::Degraded {
            reason: DegradeReason::ManifestRejected,
            ..
        }
    ));
    assert!(source.requested_starts.lock().unwrap().is_empty());
}

/// A reader that does not hold the manifest's epoch learns so without spending a fetch.
#[tokio::test]
async fn unheld_epoch_degrades_without_fetching() {
    let ciphertext = seal(&album_key(), b"locked out");
    let asset = thumbnail_asset(
        &ciphertext,
        vec![manifest(
            DerivativeRole::Thumbnail,
            "image/jxl",
            &ciphertext,
        )],
    );
    let source = serving(&ciphertext);

    let resolution = open_thumbnail(&source, &asset, &no_keys()).await;

    assert!(matches!(
        resolution,
        FetchResolution::Degraded {
            reason: DegradeReason::KeyUnavailable,
            ..
        }
    ));
    assert!(source.requested_starts.lock().unwrap().is_empty());
}

/// A thumbnail blob that no manifest describes — none at all, or one committing to other
/// bytes — has no key, so it is not fetched.
#[tokio::test]
async fn blob_without_a_describing_manifest_is_not_fetched() {
    let ciphertext = seal(&album_key(), b"undescribed");
    let other = seal(&album_key(), b"some other thumbnail");

    for manifests in [
        Vec::new(),
        vec![manifest(DerivativeRole::Thumbnail, "image/jxl", &other)],
        vec![manifest(DerivativeRole::Preview, "image/jxl", &ciphertext)],
    ] {
        let asset = thumbnail_asset(&ciphertext, manifests);
        let source = serving(&ciphertext);

        let resolution = open_thumbnail(&source, &asset, &reader_keys()).await;

        assert!(
            matches!(
                resolution,
                FetchResolution::Degraded {
                    reason: DegradeReason::NoManifest,
                    ..
                }
            ),
            "got {resolution:?}"
        );
        assert!(source.requested_starts.lock().unwrap().is_empty());
    }
}

/// An absent derivative with no manifest is the absent derivative it always was: gone,
/// degrade — and a manifest-described derivative the server does not list is the same.
#[tokio::test]
async fn absent_derivative_still_degrades_as_gone() {
    let ciphertext = seal(&album_key(), b"unlisted");
    for manifests in [
        Vec::new(),
        vec![manifest(
            DerivativeRole::Thumbnail,
            "image/jxl",
            &ciphertext,
        )],
    ] {
        let asset = AssetBlobs {
            derivative_manifests: manifests,
            ..AssetBlobs::default()
        };
        let source = serving(&ciphertext);

        let resolution = open_thumbnail(&source, &asset, &reader_keys()).await;

        assert_eq!(
            resolution,
            FetchResolution::Degraded {
                shown: Representation::Lqip,
                reason: DegradeReason::PermanentlyGone,
            }
        );
    }
}

// ─── Admitting derivative manifests (#632) ───────────────────────────────────

mod admission {
    use capsule_core::crypto::authority::ReferenceAuthority;
    use capsule_core::crypto::hash::Hash32;
    use capsule_core::crypto::keys::{
        DeviceDirectory, DeviceEntry, DirectoryCore, HybridSigningKey,
    };
    use capsule_core::crypto::provenance::{DERIVATIVE_MANIFEST_VERSION, DerivativeCore};

    use super::*;

    fn device() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[1; 32], &[2; 32])
    }

    fn write_key() -> HybridSigningKey {
        HybridSigningKey::from_seed_bytes(&[3; 32], &[4; 32])
    }

    fn directory() -> DeviceDirectory {
        DirectoryCore {
            user_id: uuid::Uuid::from_u128(0x05E2),
            directory_version: 1,
            updated_at: "2026-10-01T00:00:00Z".into(),
            devices: vec![DeviceEntry {
                device_id: uuid::Uuid::from_u128(0xD1),
                dsk_public: device().verifying_key(),
                dek_public: None,
                added_at: "2026-10-01T00:00:00Z".into(),
                revoked_at: None,
            }],
        }
        .sign(&HybridSigningKey::from_seed_bytes(&[10; 32], &[11; 32]))
    }

    /// The album authority attesting [`EPOCH`] under [`write_key`], with its AMK held or not.
    fn authority(amk_present: bool) -> ReferenceAuthority {
        let admin = HybridSigningKey::from_seed_bytes(&[7; 32], &[8; 32]);
        ReferenceAuthority::new(uuid::Uuid::from_u128(0xA1), admin.verifying_key()).with_epoch(
            &admin,
            EPOCH,
            &write_key().verifying_key(),
            amk_present,
        )
    }

    /// The core the shared [`manifest`] fixture signs, for `role` chained to `prior`.
    fn core(role: DerivativeRole, prior: Option<Hash32>, ciphertext: &[u8]) -> DerivativeCore {
        DerivativeCore {
            version: DERIVATIVE_MANIFEST_VERSION.into(),
            crypto_suite_id: capsule_core::crypto::primitives::CRYPTO_SUITE_ID,
            protocol_version: Some(capsule_core::crypto::primitives::PROTOCOL_VERSION.into()),
            amk_version: Some(EPOCH),
            source_asset_id: SOURCE_ASSET,
            role,
            format: "image/jxl".into(),
            ciphertext_hash: capsule_core::crypto::hash::hash_bytes(ciphertext),
            nonce_prefix: PREFIX,
            generated_by_device: uuid::Uuid::from_u128(0xD1),
            generated_by_client: "capsule-cli/0.1.0".into(),
            model_id: None,
            model_version: None,
            generated_at: "2026-10-17T00:00:00Z".into(),
            prior_provenance_hash: prior,
        }
    }

    fn sign(core: DerivativeCore) -> DerivativeManifest {
        core.sign(&device(), &write_key()).unwrap()
    }

    /// The exact bytes a feed entry carries for `manifest`.
    fn wire(manifest: &DerivativeManifest) -> Vec<u8> {
        capsule_core::cbor::to_canonical_vec(manifest).unwrap()
    }

    fn admit(encoded: &[Vec<u8>], amk_present: bool) -> VerifiedDerivatives {
        verify_derivative_manifests(encoded, SOURCE_ASSET, &directory(), &authority(amk_present))
    }

    fn rejected(manifest: &DerivativeManifest, reason: RejectReason) -> QuarantinedDerivative {
        QuarantinedDerivative {
            address: sha(&wire(manifest)),
            reason: DerivativeQuarantine::Rejected(reason),
        }
    }

    /// A thumbnail chain of two (a first and its `derivative-replace`) plus a preview.
    fn chain() -> (DerivativeManifest, DerivativeManifest, DerivativeManifest) {
        let first = sign(core(DerivativeRole::Thumbnail, None, b"jxl"));
        let replace = sign(core(
            DerivativeRole::Thumbnail,
            Some(first.link_hash()),
            b"avif",
        ));
        let preview = sign(core(DerivativeRole::Preview, None, b"preview"));
        (first, replace, preview)
    }

    /// The feed serves manifests in content-address order, not chain order: the chain is
    /// rebuilt from its start whatever order the bytes arrive in.
    #[test]
    fn accepts_every_role_chain_whatever_order_it_is_served_in() {
        let (first, replace, preview) = chain();
        for encoded in [
            vec![wire(&first), wire(&replace), wire(&preview)],
            vec![wire(&preview), wire(&replace), wire(&first)],
        ] {
            assert_eq!(
                admit(&encoded, true),
                VerifiedDerivatives {
                    accepted: vec![first.clone(), replace.clone(), preview.clone()],
                    pending: Vec::new(),
                    quarantined: Vec::new(),
                }
            );
        }
    }

    /// **Derivative poisoning rejection**: a `derivative-replace` that does not chain to the
    /// role's head is quarantined, and the head it tried to displace is still accepted.
    #[test]
    fn a_poisoned_replace_is_quarantined_and_the_existing_derivative_preserved() {
        let (first, replace, _) = chain();
        let poison = sign(core(
            DerivativeRole::Thumbnail,
            Some(capsule_core::crypto::hash::hash_bytes(
                b"a link nobody holds",
            )),
            b"poison",
        ));
        let verified = admit(&[wire(&first), wire(&replace), wire(&poison)], true);
        assert_eq!(verified.accepted, vec![first, replace]);
        assert_eq!(
            verified.quarantined,
            vec![rejected(&poison, RejectReason::ForgedChain)]
        );
    }

    /// Two replaces naming the same predecessor: the receiver cannot tell which is real, so
    /// both are refused and the common prefix stands.
    #[test]
    fn a_fork_quarantines_both_branches_and_keeps_the_prefix() {
        let (first, replace, _) = chain();
        let rival = sign(core(
            DerivativeRole::Thumbnail,
            Some(first.link_hash()),
            b"rival",
        ));
        let verified = admit(&[wire(&first), wire(&replace), wire(&rival)], true);
        assert_eq!(verified.accepted, vec![first]);
        let mut quarantined = verified.quarantined;
        quarantined.sort_by(|a, b| a.address.cmp(&b.address));
        let mut expected = vec![
            rejected(&replace, RejectReason::ForgedChain),
            rejected(&rival, RejectReason::ForgedChain),
        ];
        expected.sort_by(|a, b| a.address.cmp(&b.address));
        assert_eq!(quarantined, expected);
    }

    /// A refused link stops its role's walk: it is quarantined with its own reason, and what
    /// chains behind it cannot be placed.
    #[test]
    fn a_refused_link_quarantines_itself_and_everything_behind_it() {
        let first = sign(core(DerivativeRole::Thumbnail, None, b"jxl"));
        let reader = HybridSigningKey::from_seed_bytes(&[77; 32], &[78; 32]);
        let reader_signed = core(DerivativeRole::Thumbnail, Some(first.link_hash()), b"avif")
            .sign(&device(), &reader)
            .unwrap();
        let behind = sign(core(
            DerivativeRole::Thumbnail,
            Some(reader_signed.link_hash()),
            b"behind",
        ));
        let verified = admit(&[wire(&behind), wire(&reader_signed), wire(&first)], true);
        assert_eq!(verified.accepted, vec![first]);
        assert_eq!(
            verified.quarantined,
            vec![
                rejected(&reader_signed, RejectReason::BadWriteSig),
                rejected(&behind, RejectReason::ForgedChain),
            ]
        );
    }

    #[test]
    fn undecodable_bytes_are_quarantined_by_address() {
        let (first, ..) = chain();
        let junk = b"not a derivative manifest".to_vec();
        let verified = admit(&[junk.clone(), wire(&first)], true);
        assert_eq!(verified.accepted, vec![first]);
        assert_eq!(
            verified.quarantined,
            vec![QuarantinedDerivative {
                address: sha(&junk),
                reason: DerivativeQuarantine::Undecodable,
            }]
        );
    }

    #[test]
    fn a_derivative_of_another_asset_is_quarantined() {
        let mut transplanted = core(DerivativeRole::Thumbnail, None, b"jxl");
        transplanted.source_asset_id = uuid::Uuid::from_u128(0xBEEF);
        let transplanted = sign(transplanted);
        let verified = admit(&[wire(&transplanted)], true);
        assert!(verified.accepted.is_empty());
        assert_eq!(
            verified.quarantined,
            vec![rejected(&transplanted, RejectReason::WrongAsset)]
        );
    }

    #[test]
    fn a_manifest_without_an_epoch_is_quarantined() {
        let mut epochless = core(DerivativeRole::Thumbnail, None, b"jxl");
        epochless.amk_version = None;
        let epochless = sign(epochless);
        let verified = admit(&[wire(&epochless)], true);
        assert!(verified.accepted.is_empty());
        assert_eq!(
            verified.quarantined,
            vec![rejected(&epochless, RejectReason::Structural)]
        );
    }

    /// A manifest whose epoch AMK has not arrived is held, not quarantined — and it still
    /// places what chains behind it.
    #[test]
    fn a_missing_amk_holds_the_chain_as_pending() {
        let (first, replace, _) = chain();
        let verified = admit(&[wire(&first), wire(&replace)], false);
        assert_eq!(
            verified,
            VerifiedDerivatives {
                accepted: Vec::new(),
                pending: vec![first, replace],
                quarantined: Vec::new(),
            }
        );
    }

    /// End to end: the feed's bytes, admitted, open the thumbnail they describe.
    #[tokio::test]
    async fn admitted_manifests_open_the_thumbnail_they_describe() {
        let plaintext = b"\xff\x0aJXL thumbnail bytes".repeat(40);
        let ciphertext = seal(&album_key(), &plaintext);
        let signed = sign(core(DerivativeRole::Thumbnail, None, &ciphertext));

        let verified = admit(&[wire(&signed)], true);
        assert!(verified.quarantined.is_empty());
        let asset = thumbnail_asset(&ciphertext, verified.accepted);

        assert_eq!(
            open_thumbnail(&serving(&ciphertext), &asset, &reader_keys()).await,
            FetchResolution::Opened {
                representation: Representation::Thumbnail,
                plaintext,
            }
        );
    }
}
