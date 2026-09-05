//! A deterministic in-memory [`AssetIndex`] — a **test double**, never a deployment mode.
//!
//! The production adapter is PostgreSQL, and the property this double exists to reproduce is
//! the one the port's whole design rests on: **the sequence number is allocated inside the same
//! critical section that makes the row readable**. Postgres gets that from a row lock held to
//! commit; this gets it from holding one mutex across both writes. Two implementations, one
//! guarantee, and [`super::conformance`] is where that stops being a claim.
//!
//! What it deliberately does *not* reproduce is scale. Every lookup here is a scan, which is
//! the right trade for a double that is never asked about more rows than a test wrote, and the
//! wrong one for anything else.

use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use capsule_core::crypto::hash::Hash32;
use jiff::Timestamp;

use super::{
    AssetIndex, AssetRow, AssetState, BlobOutcome, BlobRecord, BlobRef, FeedEntry, HoldOutcome,
    IndexFuture, LifecycleOp, OpAction, OpOutcome, PendingAsset, Reservation, ServingHold,
    entry_for, is_singular, set_singular,
};
use crate::blob::ContentAddress;
use crate::store::{AlbumId, AssetId, BlobRole, OwnerId};

/// Take a lock, recovering rather than propagating a poisoned one — see
/// [`crate::store::memory`], which does the same for the same reason.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Everything the double holds, behind one lock.
///
/// One lock rather than two maps with two locks, because the sequence counter and the row it
/// stamps must move together — that is the entire point (see [`super`]).
#[derive(Debug, Default)]
struct Inner {
    rows: BTreeMap<AssetId, AssetRow>,
    /// The sequence number each already-applied lifecycle manifest minted, by its content hash.
    ///
    /// The whole idempotency store: a replay needs the number the first application minted, and
    /// everything else in the response is derivable from the manifest itself.
    applied: BTreeMap<Hash32, u64>,
    /// The highest sequence number each owner has minted. Absent means none.
    minted: BTreeMap<OwnerId, u64>,
}

impl Inner {
    /// Allocate `owner`'s next sequence number.
    ///
    /// Callable only with the lock held, which is what makes allocation order equal publication
    /// order. Starts at 1 so that `after = 0` means "I have seen nothing".
    /// The highest album-key epoch any row of `album` has been accepted under.
    ///
    /// Derived rather than stored, so it cannot disagree with the rows it summarizes.
    fn album_epoch(&self, album: &AlbumId) -> u64 {
        self.rows
            .values()
            .filter(|row| &row.album_id == album)
            .map(|row| row.amk_version)
            .max()
            .unwrap_or(0)
    }

    fn mint(&mut self, owner: &OwnerId) -> u64 {
        let next = self
            .minted
            .get(owner)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.minted.insert(owner.clone(), next);
        next
    }
}

/// The deterministic asset index.
#[derive(Debug, Default)]
pub struct InMemoryAssetIndex {
    inner: Mutex<Inner>,
}

impl InMemoryAssetIndex {
    /// An empty index.
    pub fn new() -> Self {
        Self::default()
    }
}

impl AssetIndex for InMemoryAssetIndex {
    fn reserve(&self, asset: PendingAsset) -> IndexFuture<'_, Reservation> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            if let Some(existing) = inner.rows.get(&asset.asset_id) {
                let agrees = existing.owner_id == asset.owner_id
                    && existing.album_id == asset.album_id
                    && existing.protocol_version == asset.protocol_version
                    && existing.crypto_suite_id == asset.crypto_suite_id;
                return Ok(if agrees {
                    Reservation::Joined(Box::new(existing.clone()))
                } else {
                    Reservation::Conflict
                });
            }

            let row = AssetRow {
                asset_id: asset.asset_id.clone(),
                owner_id: asset.owner_id,
                album_id: asset.album_id,
                protocol_version: asset.protocol_version,
                crypto_suite_id: asset.crypto_suite_id,
                state: AssetState::Pending,
                blobs: Vec::new(),
                first_seq: None,
                sync_seq: None,
                superseded: Vec::new(),
                chain_head: None,
                amk_version: 0,
                // A newly reserved asset is under no moderation hold. A hold is placed by an
                // admin action and never by a write path, which is what keeps `S-C17` from
                // becoming something an upload can accidentally set.
                hold: None,
                retention_until: None,
                created_at: asset.created_at,
                updated_at: asset.created_at,
            };
            inner.rows.insert(asset.asset_id, row.clone());
            Ok(Reservation::Created(Box::new(row)))
        })
    }

    fn read<'a>(&'a self, asset: &'a AssetId) -> IndexFuture<'a, Option<AssetRow>> {
        Box::pin(async move { Ok(lock(&self.inner).rows.get(asset).cloned()) })
    }

    fn record_blob<'a>(
        &'a self,
        asset: &'a AssetId,
        blob: BlobRecord,
    ) -> IndexFuture<'a, BlobOutcome> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(row) = inner.rows.get(asset).cloned() else {
                return Ok(BlobOutcome::NotFound);
            };

            if row
                .blobs
                .iter()
                .any(|held| held.role == blob.role && held.address == blob.address)
            {
                return Ok(BlobOutcome::AlreadyHeld(Box::new(row)));
            }
            if is_singular(blob.role) && row.blobs.iter().any(|held| held.role == blob.role) {
                return Ok(BlobOutcome::Conflict);
            }

            let mut row = row;
            row.blobs.push(BlobRef {
                role: blob.role,
                address: blob.address,
                size: blob.size,
            });
            // Ordered so two adapters that accepted the same blobs hold the same row — the
            // port contracts role-then-address order and a `Vec` will not sort itself.
            row.blobs.sort();
            row.updated_at = blob.finalized_at;

            // A create's provenance blob is the asset's first accepted manifest, so it is also
            // the chain the first lifecycle op must name (invariant 17). Set here rather than
            // declared by the route because this is where the manifest becomes durable, and a
            // head recorded before the bytes landed would point at a manifest nobody holds.
            //
            // The value is the record's own `manifest_sha256` and never the content address
            // (`S-C31`): the two are equal today and are not the same identifier, so deriving
            // one from the other would be correct by coincidence and would break silently the
            // day a suite picks a different content-address digest.
            if blob.role == BlobRole::Provenance
                && row.chain_head.is_none()
                && let Some(manifest_sha256) = blob.manifest_sha256
            {
                row.chain_head = Some(manifest_sha256);
            }

            // The publishable check runs over the *bundle*, so a blob completing the index tier
            // and a blob arriving after it are the same code path — which is what keeps
            // "visible" from depending on arrival order.
            let minted = if row.state == AssetState::Tombstoned {
                // A late blob for a deleted asset is stored (the bytes exist and GC must see the
                // reference) but publishes nothing: the tombstone is the asset's final word.
                None
            } else if row.is_publishable() {
                let owner = row.owner_id.clone();
                let seq = inner.mint(&owner);
                row.state = AssetState::Visible;
                row.sync_seq = Some(seq);
                row.first_seq = Some(row.first_seq.unwrap_or(seq));
                Some(seq)
            } else {
                None
            };

            inner.rows.insert(asset.clone(), row.clone());
            Ok(BlobOutcome::Recorded {
                row: Box::new(row),
                minted,
            })
        })
    }

    fn tombstone<'a>(
        &'a self,
        asset: &'a AssetId,
        at: Timestamp,
    ) -> IndexFuture<'a, Option<AssetRow>> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(mut row) = inner.rows.get(asset).cloned() else {
                return Ok(None);
            };
            if row.state == AssetState::Tombstoned {
                return Ok(Some(row));
            }

            // A row nobody could see needs no retraction, so it takes no sequence number. It
            // still becomes terminal, so its id cannot be reserved back into life.
            let was_published = row.sync_seq.is_some();
            row.state = AssetState::Tombstoned;
            row.updated_at = at;
            if was_published {
                let owner = row.owner_id.clone();
                row.sync_seq = Some(inner.mint(&owner));
            }
            inner.rows.insert(asset.clone(), row.clone());
            Ok(Some(row))
        })
    }

    fn find_by_address<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Option<AssetId>> {
        Box::pin(async move {
            Ok(lock(&self.inner)
                .rows
                .values()
                .find(|row| {
                    &row.owner_id == owner
                        && &row.album_id == album
                        && row.state != AssetState::Tombstoned
                        && row.blobs.iter().any(|blob| &blob.address == address)
                })
                .map(|row| row.asset_id.clone()))
        })
    }

    fn find_reference<'a>(
        &'a self,
        address: &'a ContentAddress,
    ) -> IndexFuture<'a, Option<super::BlobReference>> {
        Box::pin(async move {
            let inner = lock(&self.inner);
            // Two passes rather than a sort: a live reference outranks a tombstoned one, and
            // pending rows are not references at all.
            let holds = |row: &&AssetRow| row.blobs.iter().any(|blob| &blob.address == address);
            let reference = |row: &AssetRow| super::BlobReference {
                asset_id: row.asset_id.clone(),
                album_id: row.album_id.clone(),
                owner_id: row.owner_id.clone(),
                role: row
                    .blobs
                    .iter()
                    .find(|blob| &blob.address == address)
                    .map_or(BlobRole::Original, |blob| blob.role),
                state: row.state,
                original_held: row.original_held(),
                hold: row.hold,
            };
            let live = inner
                .rows
                .values()
                .filter(|row| row.state == AssetState::Visible)
                .find(holds);
            if let Some(row) = live {
                return Ok(Some(reference(row)));
            }
            Ok(inner
                .rows
                .values()
                .filter(|row| row.state == AssetState::Tombstoned)
                .find(holds)
                .map(reference))
        })
    }

    fn apply_op(&self, op: LifecycleOp) -> IndexFuture<'_, OpOutcome> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);

            // Idempotency first, before any invariant: a byte-identical resubmission of an op
            // that has already been applied is not a stale chain, it is the same op arriving
            // twice. Checking 17 first would answer `409` to a client whose only fault was
            // losing an acknowledgement.
            if let Some(sync_seq) = inner.applied.get(&op.manifest_hash).copied() {
                tracing::info!(
                    asset = %op.asset_id,
                    action = op.action.as_str(),
                    sync_seq,
                    "a lifecycle write was replayed; nothing was written"
                );
                return Ok(OpOutcome::Replayed { sync_seq });
            }

            let Some(row) = inner.rows.get(&op.asset_id).cloned() else {
                return Ok(OpOutcome::NotFound);
            };
            // Not this caller's asset, or not in the album the op was addressed to. Both are
            // the same answer, and neither is distinguishable from an asset that never existed.
            if row.owner_id != op.owner_id || row.album_id != op.album_id {
                tracing::info!(
                    asset = %op.asset_id,
                    "a lifecycle write was refused: the asset is not this caller's"
                );
                return Ok(OpOutcome::NotFound);
            }
            // A row nothing can see yet has no chain to extend. Treated as absent rather than
            // as a stale chain: an op against a half-finished upload is a client bug about
            // *which* asset, not about which manifest.
            if row.state == AssetState::Pending {
                return Ok(OpOutcome::NotFound);
            }

            // Invariant 17.
            if op.prior_provenance_hash != row.chain_head {
                tracing::info!(
                    asset = %op.asset_id,
                    action = op.action.as_str(),
                    "a lifecycle write was refused: it does not chain onto the stored head"
                );
                return Ok(OpOutcome::StaleChain {
                    head: row.chain_head,
                });
            }

            // Invariant 18, over the album's high-water mark rather than this row's: an epoch
            // is an album-wide fact, so an op on a stale asset must not be able to re-admit an
            // epoch the album has already moved past.
            let stored = inner.album_epoch(&op.album_id);
            if op.amk_version < stored {
                tracing::info!(
                    asset = %op.asset_id,
                    stored,
                    submitted = op.amk_version,
                    "a lifecycle write was refused: the album epoch regresses"
                );
                return Ok(OpOutcome::AmkRegressed { stored });
            }

            let mut row = row;
            row.state = match op.action {
                OpAction::Delete => AssetState::Tombstoned,
                // A restore returns the asset to the live set. Every other action leaves the
                // state alone — a metadata update to a tombstoned asset is still a tombstone,
                // and so is a replace: re-uploading the bytes of something you deleted does not
                // undelete it, because undeleting is what a `trash-restore` is for.
                OpAction::TrashRestore => AssetState::Visible,
                OpAction::MetadataUpdate | OpAction::Derivative | OpAction::Replace => row.state,
            };
            // The provenance blob is re-pointed on every op, which is the one place a singular
            // role legitimately moves: the chain *is* a succession of manifests, so the newest
            // one is what the feed must serve.
            set_singular(&mut row, BlobRole::Provenance, &op.provenance);
            if let Some(metadata) = &op.metadata {
                set_singular(&mut row, BlobRole::Metadata, metadata);
            }
            // A replace's whole point (`S-C43`). It re-points the one role `record_blob`
            // refuses to move, and the refusal keeps meaning what it means: an *upload* that
            // swapped the original would swap bytes under a signature that still verifies
            // against the old ones, while this arrives with a manifest that chains onto the one
            // it supersedes — which is the difference between an authorized replacement and the
            // defect `BlobOutcome::Conflict` was written to stop.
            if let Some(original) = &op.original {
                set_singular(&mut row, BlobRole::Original, original);
            }
            row.chain_head = Some(op.manifest_hash);
            row.amk_version = op.amk_version;
            row.retention_until = match op.action {
                OpAction::Delete => op.retention_until,
                // Back in the live set: there is no window left to run out.
                OpAction::TrashRestore => None,
                OpAction::MetadataUpdate | OpAction::Derivative | OpAction::Replace => {
                    row.retention_until
                }
            };
            row.updated_at = op.at;

            let owner = row.owner_id.clone();
            let sync_seq = inner.mint(&owner);
            row.sync_seq = Some(sync_seq);
            inner.applied.insert(op.manifest_hash, sync_seq);
            inner.rows.insert(op.asset_id.clone(), row.clone());
            tracing::info!(
                asset = %op.asset_id,
                action = op.action.as_str(),
                sync_seq,
                "a lifecycle write was applied"
            );
            Ok(OpOutcome::Applied {
                row: Box::new(row),
                sync_seq,
            })
        })
    }

    fn set_hold<'a>(
        &'a self,
        asset: &'a AssetId,
        hold: Option<ServingHold>,
    ) -> IndexFuture<'a, HoldOutcome> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(row) = inner.rows.get_mut(asset) else {
                return Ok(HoldOutcome::NotFound);
            };
            if row.hold == hold {
                return Ok(HoldOutcome::Unchanged);
            }
            row.hold = hold;
            if let Some(hold) = hold {
                tracing::info!(
                    %asset,
                    hold = hold.as_str(),
                    "an asset was placed under a serving hold; its bytes are untouched"
                );
            } else {
                tracing::info!(%asset, "an asset's serving hold was lifted");
            }
            Ok(HoldOutcome::Applied)
        })
    }

    fn reference_count<'a>(&'a self, address: &'a ContentAddress) -> IndexFuture<'a, u64> {
        Box::pin(async move {
            Ok(lock(&self.inner)
                .rows
                .values()
                .filter(|row| {
                    row.blobs.iter().any(|blob| &blob.address == address)
                        // A manifest the chain has moved past is still referenced (`S-C52`).
                        // Without this the collector reclaims the server's own evidence.
                        || row.superseded.contains(address)
                })
                .count() as u64)
        })
    }

    fn rows<'a>(
        &'a self,
        after: Option<&'a AssetId>,
        limit: usize,
    ) -> IndexFuture<'a, Vec<AssetRow>> {
        Box::pin(async move {
            let inner = lock(&self.inner);
            // The map is keyed by asset id, so its own order *is* the contract's order and the
            // cursor is a range bound rather than a scan-and-filter.
            let range = match after {
                Some(after) => inner
                    .rows
                    .range((
                        std::ops::Bound::Excluded(after.clone()),
                        std::ops::Bound::Unbounded,
                    ))
                    .take(limit)
                    .map(|(_, row)| row.clone())
                    .collect(),
                None => inner.rows.values().take(limit).cloned().collect(),
            };
            Ok(range)
        })
    }

    fn tombstoned(&self, limit: usize) -> IndexFuture<'_, Vec<AssetRow>> {
        Box::pin(async move {
            let inner = lock(&self.inner);
            let mut rows: Vec<AssetRow> = inner
                .rows
                .values()
                .filter(|row| row.state == AssetState::Tombstoned)
                .cloned()
                .collect();
            // Oldest change first, so a bounded pass makes progress on the oldest deletions
            // rather than revisiting the same page.
            rows.sort_by(|a, b| {
                (a.updated_at, a.asset_id.as_str()).cmp(&(b.updated_at, b.asset_id.as_str()))
            });
            rows.truncate(limit);
            Ok(rows)
        })
    }

    fn purge<'a>(&'a self, asset: &'a AssetId) -> IndexFuture<'a, Option<AssetRow>> {
        Box::pin(async move {
            let mut inner = lock(&self.inner);
            let Some(row) = inner.rows.get_mut(asset) else {
                return Ok(None);
            };
            row.blobs.clear();
            // The chain goes with the bytes it describes. A purge is the end of the retention
            // window the user's own signed delete fixed, so there is nothing left to rebut with
            // and nothing left to rebut about (`S-C52`).
            row.superseded.clear();
            let row = row.clone();
            tracing::info!(%asset, "purged a tombstoned asset's blob references");
            Ok(Some(row))
        })
    }

    fn feed_page<'a>(
        &'a self,
        owner: &'a OwnerId,
        after: u64,
        limit: usize,
    ) -> IndexFuture<'a, Vec<FeedEntry>> {
        Box::pin(async move {
            let inner = lock(&self.inner);
            let mut page: Vec<FeedEntry> = inner
                .rows
                .values()
                .filter(|row| &row.owner_id == owner)
                .filter(|row| row.sync_seq.is_some_and(|seq| seq > after))
                .filter_map(|row| entry_for(row, after))
                .collect();
            page.sort_by_key(|entry| entry.sync_seq);
            page.truncate(limit);
            Ok(page)
        })
    }

    fn head_seq<'a>(&'a self, owner: &'a OwnerId) -> IndexFuture<'a, u64> {
        Box::pin(async move { Ok(lock(&self.inner).minted.get(owner).copied().unwrap_or(0)) })
    }

    fn album_feed_page<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
        after: u64,
        limit: usize,
    ) -> IndexFuture<'a, Vec<FeedEntry>> {
        Box::pin(async move {
            let inner = lock(&self.inner);
            let mut page: Vec<FeedEntry> = inner
                .rows
                .values()
                .filter(|row| &row.owner_id == owner && &row.album_id == album)
                .filter(|row| row.sync_seq.is_some_and(|seq| seq > after))
                .filter_map(|row| entry_for(row, after))
                .collect();
            page.sort_by_key(|entry| entry.sync_seq);
            page.truncate(limit);
            Ok(page)
        })
    }

    fn album_head_seq<'a>(
        &'a self,
        owner: &'a OwnerId,
        album: &'a AlbumId,
    ) -> IndexFuture<'a, u64> {
        Box::pin(async move {
            Ok(lock(&self.inner)
                .rows
                .values()
                .filter(|row| &row.owner_id == owner && &row.album_id == album)
                .filter_map(|row| row.sync_seq)
                .max()
                .unwrap_or(0))
        })
    }
}
