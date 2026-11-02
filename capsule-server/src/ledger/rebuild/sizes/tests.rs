//! The size repair's contract: a dry run writes nothing, `--apply` fills in exactly the size-0
//! references whose blob the store holds, a missing or empty blob is reported and kept, nothing
//! else changes, and a second pass finds nothing.

use std::sync::Arc;

use capsule_core::crypto::hash::hash_bytes;
use jiff::Timestamp;

use super::*;
use crate::blob::{BlobStore as _, InMemoryBlobStore};
use crate::index::memory::InMemoryAssetIndex;
use crate::index::{
    AssetIndex as _, AssetRow, BlobRecord, LifecycleOp, OpAction, OpBlob, OpOutcome, PendingAsset,
};
use crate::ledger::InMemoryLedger;
use crate::ledger::replay::LedgerKeys;
use crate::quota::InMemoryQuota;
use crate::store::memory::ManualClock;
use crate::store::{AlbumId, OwnerId};

struct Harness {
    index: Arc<InMemoryAssetIndex>,
    blobs: Arc<InMemoryBlobStore>,
    context: RebuildContext,
}

impl Harness {
    fn new() -> Self {
        let index = Arc::new(InMemoryAssetIndex::new());
        let blobs = Arc::new(InMemoryBlobStore::new());
        let context = RebuildContext::new(
            index.clone(),
            blobs.clone(),
            Arc::new(InMemoryLedger::new()),
            Arc::new(InMemoryQuota::new()),
            LedgerKeys::new(Vec::new()),
            Arc::new(ManualClock::new(Timestamp::UNIX_EPOCH)),
        );
        Self {
            index,
            blobs,
            context,
        }
    }

    /// The address `bytes` have, stored or not.
    fn address(bytes: &[u8]) -> ContentAddress {
        ContentAddress::parse(&hash_bytes(bytes).to_hex()).expect("an address")
    }

    async fn store(&self, bytes: &[u8]) -> ContentAddress {
        let address = Self::address(bytes);
        self.blobs.put(&address, bytes).await.expect("stored");
        address
    }

    async fn record(&self, asset: &AssetId, role: BlobRole, bytes: &[u8]) {
        let address = self.store(bytes).await;
        self.index
            .record_blob(
                asset,
                BlobRecord {
                    manifest_sha256: (role == BlobRole::Provenance).then(|| hash_bytes(bytes)),
                    amk_version: None,
                    role,
                    address,
                    size: bytes.len() as u64,
                    finalized_at: Timestamp::UNIX_EPOCH,
                },
            )
            .await
            .expect("the index records");
    }

    /// Publish `name` with an original, then re-point its manifest and metadata through an op
    /// that recorded both at size 0 — the row a lifecycle op wrote before ops carried sizes.
    async fn legacy(&self, name: &str, manifest: &[u8], metadata: &[u8]) -> AssetId {
        let asset = AssetId::new(name);
        self.index
            .reserve(PendingAsset {
                asset_id: asset.clone(),
                owner_id: OwnerId::new("sizes-owner"),
                album_id: AlbumId::new("sizes-album"),
                protocol_version: "2026-01-01".to_owned(),
                crypto_suite_id: 1,
                created_at: Timestamp::UNIX_EPOCH,
            })
            .await
            .expect("reserved");
        let create = format!("{name}-create");
        self.record(&asset, BlobRole::Provenance, create.as_bytes())
            .await;
        self.record(
            &asset,
            BlobRole::Metadata,
            format!("{name}-meta-1").as_bytes(),
        )
        .await;
        self.record(
            &asset,
            BlobRole::Original,
            format!("{name}-original").as_bytes(),
        )
        .await;
        let outcome = self
            .index
            .apply_op(LifecycleOp {
                asset_id: asset.clone(),
                owner_id: OwnerId::new("sizes-owner"),
                album_id: AlbumId::new("sizes-album"),
                action: OpAction::MetadataUpdate,
                manifest_hash: hash_bytes(manifest),
                prior_provenance_hash: Some(hash_bytes(create.as_bytes())),
                amk_version: 1,
                provenance: OpBlob {
                    address: Self::address(manifest),
                    size: 0,
                },
                metadata: Some(OpBlob {
                    address: Self::address(metadata),
                    size: 0,
                }),
                original: None,
                retention_until: None,
                at: Timestamp::UNIX_EPOCH,
            })
            .await
            .expect("the op applies");
        assert!(
            matches!(outcome, OpOutcome::Applied { .. }),
            "the legacy op answered {outcome:?}"
        );
        asset
    }

    async fn rows(&self) -> Vec<AssetRow> {
        self.index.rows(None, 1000).await.expect("rows")
    }
}

fn reference(asset: &AssetId, role: BlobRole, bytes: &[u8]) -> ZeroSized {
    ZeroSized {
        asset: asset.clone(),
        role,
        address: Harness::address(bytes),
    }
}

fn zero_sizes(rows: &[AssetRow]) -> usize {
    rows.iter()
        .flat_map(|row| &row.blobs)
        .filter(|blob| blob.size == 0)
        .count()
}

#[tokio::test]
async fn a_dry_run_reports_the_repairs_and_writes_nothing() {
    let h = Harness::new();
    let asset = h.legacy("dry", b"dry-manifest-2", b"dry-meta-2").await;
    h.store(b"dry-manifest-2").await;
    h.store(b"dry-meta-2").await;
    let before = h.rows().await;

    let report = repair_sizes(&h.context, Mode::DryRun)
        .await
        .expect("a pass");

    assert!(!report.applied);
    let mut repaired = report.repaired.clone();
    repaired.sort();
    let mut expected = vec![
        Resized {
            reference: reference(&asset, BlobRole::Metadata, b"dry-meta-2"),
            size: 10,
        },
        Resized {
            reference: reference(&asset, BlobRole::Provenance, b"dry-manifest-2"),
            size: 14,
        },
    ];
    expected.sort();
    assert_eq!(repaired, expected);
    assert!(!report.has_findings());
    assert_eq!(h.rows().await, before, "a dry run wrote");
}

#[tokio::test]
async fn apply_repairs_only_zero_sizes_and_a_rerun_finds_nothing() {
    let h = Harness::new();
    let asset = h.legacy("fix", b"fix-manifest-2", b"fix-meta-2").await;
    h.store(b"fix-manifest-2").await;
    h.store(b"fix-meta-2").await;
    let before = h.rows().await;
    assert_eq!(zero_sizes(&before), 2);

    let report = repair_sizes(&h.context, Mode::Apply).await.expect("a pass");
    assert!(report.applied);
    assert_eq!(report.repaired.len(), 2);
    assert!(report.moved.is_empty());
    assert!(!report.has_findings());

    let after = h.rows().await;
    assert_eq!(zero_sizes(&after), 0, "{after:?}");
    let row = &after[0];
    let size = |role| {
        row.blobs
            .iter()
            .find(|blob| blob.role == role)
            .map(|blob| blob.size)
    };
    assert_eq!(size(BlobRole::Provenance), Some(14));
    assert_eq!(size(BlobRole::Metadata), Some(10));
    assert_eq!(
        size(BlobRole::Original),
        Some(b"fix-original".len() as u64),
        "a non-zero size is left as it was"
    );
    let shape = |rows: &[AssetRow]| {
        rows.iter()
            .map(|row| AssetRow {
                blobs: row
                    .blobs
                    .iter()
                    .map(|blob| crate::index::BlobRef {
                        size: 0,
                        ..blob.clone()
                    })
                    .collect(),
                sync_seq: None,
                ..row.clone()
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        shape(&after),
        shape(&before),
        "nothing but sizes and the feed position changed: no address, role or state moved"
    );
    assert!(
        row.sync_seq > before[0].sync_seq,
        "a repaired row moves to the feed's head so synced devices see its sizes"
    );

    let again = repair_sizes(&h.context, Mode::Apply)
        .await
        .expect("a second pass");
    assert_eq!(
        again,
        SizeRepairReport {
            applied: true,
            ..SizeRepairReport::default()
        }
    );
    assert_eq!(h.rows().await, after, "a rerun changed something");
    let _ = asset;
}

#[tokio::test]
async fn a_missing_or_empty_blob_is_reported_and_its_reference_kept() {
    let h = Harness::new();
    // The manifest was never stored, and the metadata blob is stored empty.
    let asset = h.legacy("gone", b"gone-manifest-2", b"").await;
    h.store(b"").await;
    let before = h.rows().await;

    let report = repair_sizes(&h.context, Mode::Apply).await.expect("a pass");

    assert_eq!(
        report.missing,
        vec![reference(&asset, BlobRole::Provenance, b"gone-manifest-2")]
    );
    assert_eq!(
        report.empty,
        vec![reference(&asset, BlobRole::Metadata, b"")]
    );
    assert!(report.repaired.is_empty());
    assert!(report.has_findings());
    assert_eq!(
        h.rows().await,
        before,
        "an unrepairable reference is kept exactly as it was"
    );
}
