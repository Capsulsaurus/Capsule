//! The keyless **retention purge worker** (slice `S-C11`).
//!
//! A soft delete is a state change, not a file operation: the client signs a `delete`
//! manifest carrying `retention_until` in its **server-visible envelope**, and the server
//! records `assets.deleted_at` plus a `Deleted` feed entry. The trash retention window is
//! therefore **cryptographic**, not server-configured — the purge worker reads
//! `retention_until` straight from the signed manifest and compares it against its own
//! [trusted clock][clk], with **no decryption key**. This is what stops a hostile server from
//! accelerating a purge: there is no local-config path to bring a purge forward, and a delete
//! whose signed floor has not elapsed is refused ([Organization — Retention Window][ret]).
//!
//! When (and only when) the signed floor has elapsed, [`RetentionPurgeWorker::purge_expired`]
//! hard-purges the asset: it deletes the `assets` row (releasing the original blob's
//! reference) and releases the asset's auxiliary blobs from the [quota ledger][led]. The
//! now-unreferenced bytes are reclaimed later by the refcount [mark-and-sweep](super::worker)
//! — the two workers compose but stay decoupled.
//!
//! A delete manifest that carries **no** parseable `retention_until` is never purged (bias
//! toward keeping bytes); it is surfaced for an operator instead.
//!
//! [clk]: ../../../../capsule-docs/src/content/docs/design/filesystem/server.md
//! [ret]: ../../../../capsule-docs/src/content/docs/design/organization.md
//! [led]: entity::quota_ledger

use std::collections::BTreeSet;

use ::entity::{asset, sync_entry};
use jiff::Timestamp;
use sea_orm::{
    ColumnTrait, DatabaseConnection, DbErr, EntityTrait, QueryFilter, QueryOrder, TransactionTrait,
};
use serde::Deserialize;
use tracing::{info, instrument, warn};

use super::{Clock, SystemClock};
use crate::quota;
use crate::sync::FeedBlobManifest;

/// The `Deleted` change-kind discriminant on the sync feed (mirrors
/// [`sync::ChangeKind::Deleted`](crate::sync::ChangeKind)); a delete manifest lands as this
/// kind and carries `retention_until`.
const DELETED_KIND: i16 = 3;

/// A minimal, key-free view of the signed manifest **envelope** — only the two server-visible
/// fields the purge worker needs. The full envelope is owned by the upload crate; the worker
/// deserializes just this from the stored canonical CBOR (unknown fields skipped), so no key
/// and no cross-crate coupling is required.
#[derive(Debug, Deserialize)]
struct RetentionView {
    /// The signed retention floor (RFC 3339), the cryptographic floor the worker enforces.
    #[serde(default)]
    retention_until: Option<String>,
    /// The committed metadata-blob content address, released from the quota ledger on purge.
    #[serde(default)]
    metadata_blob_hash: Option<String>,
}

/// A structured summary of one retention purge pass — the roll-up behind the per-asset logs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RetentionReport {
    /// Asset ids hard-purged (or, in a dry run, that *would* be purged).
    pub purged: Vec<String>,
    /// Asset ids refused because their signed `retention_until` has not elapsed — the
    /// hostile-server / early-purge defense.
    pub refused_in_window: Vec<String>,
    /// Asset ids skipped because their delete manifest carried no parseable retention floor
    /// (bytes kept, surfaced for an operator).
    pub skipped_no_floor: Vec<String>,
    /// Whether this was a non-mutating dry run.
    pub dry_run: bool,
}

/// The operator-invokable retention purge worker. Construct with [`RetentionPurgeWorker::new`]
/// for production, or [`RetentionPurgeWorker::with_clock`] to inject a deterministic
/// [`Clock`] in tests. A binary crons [`RetentionPurgeWorker::purge_expired`].
pub struct RetentionPurgeWorker<K: Clock = SystemClock> {
    clock: K,
}

impl Default for RetentionPurgeWorker<SystemClock> {
    fn default() -> Self {
        Self::new()
    }
}

impl RetentionPurgeWorker<SystemClock> {
    /// A retention worker on the system clock.
    #[must_use]
    pub fn new() -> Self {
        Self { clock: SystemClock }
    }
}

impl<K: Clock> RetentionPurgeWorker<K> {
    /// A retention worker with an injected [`Clock`] — the seam that proves the window refusal
    /// / proceed behaviour without waiting real time.
    pub fn with_clock(clock: K) -> Self {
        Self { clock }
    }

    /// Scan soft-deleted assets and hard-purge exactly those whose signed retention floor has
    /// elapsed. Refuses any purge inside the signed window (the early-purge / hostile-server
    /// defense) and skips any delete with no parseable floor. With `dry_run`, decisions are
    /// logged and reported but **no row is deleted and no quota released**.
    #[instrument(skip(self, db), fields(dry_run))]
    pub async fn purge_expired(
        &self,
        db: &DatabaseConnection,
        dry_run: bool,
    ) -> Result<RetentionReport, DbErr> {
        let now = self.clock.now();
        let mut report = RetentionReport {
            dry_run,
            ..Default::default()
        };
        info!(%now, dry_run, "retention: purge pass starting");

        let soft_deleted = asset::Entity::find()
            .filter(asset::Column::DeletedAt.is_not_null())
            .all(db)
            .await?;

        for asset in soft_deleted {
            match self.retention_floor(db, &asset.id).await? {
                Some(until) if now < until => {
                    report.refused_in_window.push(asset.id.clone());
                    info!(
                        asset_id = %asset.id,
                        retention_until = %until,
                        %now,
                        "retention: purge REFUSED — inside the signed retention window"
                    );
                }
                Some(until) => {
                    if dry_run {
                        info!(asset_id = %asset.id, retention_until = %until, %now, "retention: [dry-run] would hard-purge past the signed window");
                    } else {
                        self.purge_one(db, &asset.id).await?;
                        info!(asset_id = %asset.id, retention_until = %until, %now, "retention: hard-purged past the signed window");
                    }
                    report.purged.push(asset.id.clone());
                }
                None => {
                    report.skipped_no_floor.push(asset.id.clone());
                    warn!(
                        asset_id = %asset.id,
                        "retention: SKIPPED — delete manifest carried no parseable retention_until; keeping bytes"
                    );
                }
            }
        }

        info!(
            purged = report.purged.len(),
            refused = report.refused_in_window.len(),
            skipped = report.skipped_no_floor.len(),
            dry_run,
            "retention: purge pass complete"
        );
        Ok(report)
    }

    /// The signed retention floor for a soft-deleted asset: `retention_until` read (no key)
    /// from the newest `Deleted` feed entry's envelope. `None` when there is no delete entry,
    /// the CBOR is undecodable, or the field is absent/unparseable — every such case is a
    /// "keep the bytes" outcome.
    async fn retention_floor(
        &self,
        db: &DatabaseConnection,
        asset_id: &str,
    ) -> Result<Option<Timestamp>, DbErr> {
        let entry = sync_entry::Entity::find()
            .filter(sync_entry::Column::AssetId.eq(asset_id))
            .filter(sync_entry::Column::Kind.eq(DELETED_KIND))
            .order_by_desc(sync_entry::Column::FeedSeq)
            .one(db)
            .await?;
        let Some(entry) = entry else {
            return Ok(None);
        };
        let view: RetentionView = match capsule_core::cbor::from_slice(&entry.manifest_cbor) {
            Ok(v) => v,
            Err(e) => {
                warn!(%asset_id, "retention: delete manifest CBOR undecodable ({e}); keeping bytes");
                return Ok(None);
            }
        };
        match view.retention_until {
            Some(s) => match s.parse::<Timestamp>() {
                Ok(ts) => Ok(Some(ts)),
                Err(e) => {
                    warn!(%asset_id, value = %s, "retention: retention_until unparseable ({e}); keeping bytes");
                    Ok(None)
                }
            },
            None => Ok(None),
        }
    }

    /// Hard-purge one asset in a single transaction: release its auxiliary blob references
    /// (derivatives + metadata) from the quota ledger, then delete the `assets` row (which
    /// drops the original blob's reference). The bytes are reclaimed later by the refcount
    /// mark-and-sweep.
    async fn purge_one(&self, db: &DatabaseConnection, asset_id: &str) -> Result<(), DbErr> {
        let aux = self.aux_hashes(db, asset_id).await?;
        let txn = db.begin().await?;
        for hash in &aux {
            // Originals are accounted from the assets index (released by the row delete below),
            // not the ledger; releasing a non-ledgered hash is a harmless no-op.
            let outcome = quota::Mutation::release(&txn, hash).await?;
            info!(asset_id, %hash, ?outcome, "retention: released auxiliary blob reference");
        }
        asset::Entity::delete_by_id(asset_id.to_string())
            .exec(&txn)
            .await?;
        txn.commit().await?;
        Ok(())
    }

    /// The set of auxiliary content addresses an asset holds across its whole provenance
    /// history: derivative blobs (from each feed entry's `blobs` manifest) and metadata blobs
    /// (the committed `metadata_blob_hash` in each entry's envelope). The original blob is
    /// excluded — it is released by the row delete, not the ledger.
    async fn aux_hashes(
        &self,
        db: &DatabaseConnection,
        asset_id: &str,
    ) -> Result<BTreeSet<String>, DbErr> {
        let entries = sync_entry::Entity::find()
            .filter(sync_entry::Column::AssetId.eq(asset_id))
            .all(db)
            .await?;
        let mut set = BTreeSet::new();
        for entry in entries {
            if let Ok(manifest) = serde_json::from_value::<FeedBlobManifest>(entry.blobs.clone()) {
                for derivative in manifest.derivatives {
                    set.insert(derivative.ciphertext_hash);
                }
            }
            if let Ok(view) = capsule_core::cbor::from_slice::<RetentionView>(&entry.manifest_cbor)
                && let Some(hash) = view.metadata_blob_hash
            {
                set.insert(hash);
            }
        }
        Ok(set)
    }
}
