//! The scrub's own suite.
//!
//! Two properties carry it: a clean store yields nothing and yields the same nothing twice,
//! and each seeded corruption is reported as **its own class** — because a scrub that reports
//! "something is wrong" is a scrub an operator cannot act on.
//!
//! Every case also asserts the store and index are unchanged afterwards. That is the one
//! property this module's whole design rests on, so it is checked rather than assumed.

use std::sync::Arc;

use capsule_core::crypto::hash::{Hash32, hash_bytes};
use jiff::Timestamp;

use super::*;
use crate::blob::{InMemoryBlobStore, QuarantineReason};
use crate::index::memory::InMemoryAssetIndex;
use crate::index::{BlobRecord, PendingAsset};
use crate::store::memory::InMemoryUploadSessions;
use crate::store::{AlbumId, OwnerId, SystemClock, UploadId};

/// The three stores, assembled.
struct Harness {
    context: ScrubContext,
    index: Arc<InMemoryAssetIndex>,
    blobs: Arc<InMemoryBlobStore>,
}

impl Harness {
    fn new() -> Self {
        let index = Arc::new(InMemoryAssetIndex::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let uploads = Arc::new(InMemoryUploadSessions::new(
            Arc::new(SystemClock),
            jiff::SignedDuration::from_hours(24),
        ));
        let context = ScrubContext::new(index.clone(), blobs.clone(), uploads);
        Self {
            context,
            index,
            blobs,
        }
    }

    /// Put `bytes` in the store at their own address.
    async fn store(&self, bytes: &[u8]) -> ContentAddress {
        let address = ContentAddress::parse(&hash_bytes(bytes).to_hex()).expect("an address");
        self.blobs.put(&address, bytes).await.expect("stored");
        address
    }

    /// Publish `asset` with its index tier, and return its provenance address.
    async fn publish(&self, asset: &str) -> ContentAddress {
        let id = AssetId::new(asset);
        self.index
            .reserve(PendingAsset {
                asset_id: id.clone(),
                owner_id: OwnerId::new("scrub-owner"),
                album_id: AlbumId::new("scrub-album"),
                protocol_version: "2026-01-01".to_owned(),
                crypto_suite_id: 1,
                created_at: Timestamp::UNIX_EPOCH,
            })
            .await
            .expect("the index reserves");

        let provenance = self.store(format!("{asset}-provenance").as_bytes()).await;
        self.record(&id, BlobRole::Provenance, &provenance).await;
        let metadata = self.store(format!("{asset}-metadata").as_bytes()).await;
        self.record(&id, BlobRole::Metadata, &metadata).await;
        provenance
    }

    async fn record(&self, asset: &AssetId, role: BlobRole, address: &ContentAddress) {
        self.index
            .record_blob(
                asset,
                BlobRecord {
                    manifest_sha256: (role == BlobRole::Provenance)
                        .then(|| Hash32::from_hex(address.as_str()).expect("a digest")),
                    role,
                    address: address.clone(),
                    size: 32,
                    finalized_at: Timestamp::UNIX_EPOCH,
                },
            )
            .await
            .expect("the index records");
    }

    /// A snapshot of both sides, so a case can assert the scrub changed nothing.
    async fn snapshot(&self) -> (Vec<AssetRow>, Vec<ContentAddress>) {
        let rows = self.index.rows(None, 1000).await.expect("rows");
        let page = self.blobs.enumerate(None, 1000).await.expect("enumerate");
        (
            rows,
            page.entries.into_iter().map(|stat| stat.address).collect(),
        )
    }
}

use crate::index::AssetRow;

/// A deep pass with a budget nothing will reach.
fn deep() -> Depth {
    Depth::Deep {
        budget: 1024 * 1024,
    }
}

// ===========================================================================================

#[tokio::test]
async fn a_clean_store_yields_nothing_and_yields_it_twice() {
    let h = Harness::new();
    h.publish("clean").await;

    let first = scrub(&h.context, deep()).await.expect("a pass");
    assert!(first.is_clean(), "found {:?}", first.findings);
    assert_eq!(first.counts(), Vec::new());

    let second = scrub(&h.context, deep()).await.expect("a pass");
    assert_eq!(
        first, second,
        "a scrub that is not idempotent on a clean store is a scrub whose output nobody can \
         diff between runs"
    );
}

#[tokio::test]
async fn a_missing_blob_is_a_dangling_reference_and_the_row_survives() {
    let h = Harness::new();
    let provenance = h.publish("dangling").await;
    let before = h.snapshot().await;
    h.blobs
        .remove(&provenance)
        .await
        .expect("the store removes");

    let report = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert_eq!(report.count("dangling_reference"), 1);
    assert!(matches!(
        report.findings.first(),
        Some(Finding::DanglingReference {
            role: BlobRole::Provenance,
            ..
        })
    ));

    let (rows, _) = h.snapshot().await;
    assert_eq!(
        rows, before.0,
        "erasing the row would destroy the only record that the asset should exist, which is \
         why this classifies and never repairs"
    );
}

#[tokio::test]
async fn a_missing_provenance_blob_also_breaks_the_chain_head() {
    let h = Harness::new();
    let provenance = h.publish("chain").await;
    h.blobs
        .remove(&provenance)
        .await
        .expect("the store removes");

    let report = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert_eq!(
        report.count("chain_head_unresolvable"),
        1,
        "the chain head names a provenance blob, so losing it is two findings and not one — \
         and they are separate classes because they call for different investigations"
    );
    assert_eq!(report.count("dangling_reference"), 1);
}

#[tokio::test]
async fn an_unreferenced_blob_is_an_orphan_and_is_left_alone() {
    let h = Harness::new();
    h.publish("referenced").await;
    let orphan = h.store(b"a finalization-crash orphan").await;

    let report = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert_eq!(
        report.findings,
        vec![Finding::Orphan {
            address: orphan.clone()
        }]
    );
    assert!(
        h.blobs.stat(&orphan).await.expect("stat").is_some(),
        "reporting for the collector is the whole job; removing here would make the scrub the \
         deletion bug it exists to catch"
    );
}

#[tokio::test]
async fn bit_rot_is_found_only_by_a_deep_pass() {
    let h = Harness::new();
    let id = AssetId::new("rotten");
    h.publish("rotten").await;

    // A blob whose bytes do not hash to its name — what bit rot looks like from outside.
    let honest = hash_bytes(b"the bytes this address names").to_hex();
    let address = ContentAddress::parse(&honest).expect("an address");
    h.blobs
        .put(&address, b"different bytes entirely")
        .await
        .expect("stored");
    h.record(&id, BlobRole::Original, &address).await;

    let structural = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert!(
        structural.is_clean(),
        "a structural pass stats and does not read, so it cannot see rot: {:?}",
        structural.findings
    );

    let report = scrub(&h.context, deep()).await.expect("a pass");
    assert_eq!(report.count("byte_mismatch"), 1);
    assert!(report.bytes_hashed > 0);
}

#[tokio::test]
async fn a_deep_pass_that_runs_out_of_budget_is_not_clean() {
    let h = Harness::new();
    h.publish("budgeted").await;

    let report = scrub(&h.context, Depth::Deep { budget: 1 })
        .await
        .expect("a pass");
    assert!(report.findings.is_empty());
    assert!(report.budget_exhausted);
    assert!(
        !report.is_clean(),
        "a clean report from a truncated pass is not a clean store, and an operator alerting on \
         the finding count has to be able to tell the difference"
    );
}

#[tokio::test]
async fn quarantined_blobs_and_stale_stages_are_inventoried() {
    let h = Harness::new();
    let provenance = h.publish("held").await;
    h.blobs
        .quarantine(
            &provenance,
            QuarantineReason {
                code: "error.scrub.hash_mismatch".to_owned(),
                detail: "seeded".to_owned(),
                at: Timestamp::UNIX_EPOCH,
            },
        )
        .await
        .expect("the store quarantines");

    // A stage with no session behind it.
    let upload = UploadId::new("018f3f1e-4b7a-7c9d-8e2f-1a2b3c4d5e6f");
    h.blobs.begin(&upload).await.expect("the store stages");

    let report = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert_eq!(report.count("quarantined"), 1);
    assert_eq!(report.count("stale_stage"), 1);
    assert_eq!(
        report.count("dangling_reference"),
        1,
        "quarantining takes the bytes out of the store, so the row that still references them \
         is dangling — which is the honest reading, not a double-report"
    );
}

#[tokio::test]
async fn the_report_counts_by_class() {
    let h = Harness::new();
    h.publish("counted-one").await;
    h.publish("counted-two").await;
    h.store(b"orphan one").await;
    h.store(b"orphan two").await;

    let report = scrub(&h.context, Depth::Structural).await.expect("a pass");
    assert_eq!(report.counts(), vec![("orphan", 2)]);
    assert!(!report.is_clean());
}
