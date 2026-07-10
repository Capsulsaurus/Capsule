//! Mocked tests for the download-sync doc's client fetch Validation bullets
//! (slice `S-D2`): tiered fetch correctness, cross-asset dedup, resume-after-
//! interrupt (zero duplicate bytes), above-tier permanent unavailability with the
//! degrade ladder and automatic re-fetch, the `403` authorization-change path, the
//! `awaiting-original` pending state, and content-hash self-verification. All
//! deterministic — driven through the [`BlobSource`] seam, never a live socket.

#![allow(clippy::unwrap_used)]

use std::collections::{BTreeSet, HashSet};
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use super::*;

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
    async fn get_range(&self, _hash: &str, start: u64) -> RangeOutcome {
        self.requested_starts.lock().unwrap().push(start);
        let start = start as usize;
        if start >= self.data.len() {
            return RangeOutcome::Complete { bytes: Vec::new() };
        }
        let end = (start + self.window).min(self.data.len());
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
    async fn get_range(&self, _hash: &str, _start: u64) -> RangeOutcome {
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
    async fn get_range(&self, _hash: &str, start: u64) -> RangeOutcome {
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
    async fn get_range(&self, _hash: &str, start: u64) -> RangeOutcome {
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
    async fn get_range(&self, _hash: &str, _start: u64) -> RangeOutcome {
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
    let resolution =
        open_representation(&gone, &asset, Representation::Original, &held, async || {}).await;
    assert!(matches!(
        resolution,
        FetchResolution::Degraded {
            reason: DegradeReason::PermanentlyGone,
            ..
        }
    ));
}
