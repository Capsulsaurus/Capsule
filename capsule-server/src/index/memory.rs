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

use jiff::Timestamp;

use super::{
    AssetIndex, AssetRow, AssetState, BlobOutcome, BlobRecord, BlobRef, FeedEntry, IndexFuture,
    PendingAsset, Reservation, entry_for,
};
use crate::blob::ContentAddress;
use crate::store::{AlbumId, AssetId, BlobRole, OwnerId};

/// Take a lock, recovering rather than propagating a poisoned one — see
/// [`crate::store::memory`], which does the same for the same reason.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The roles an asset may hold exactly one of.
///
/// The manifest, the metadata blob and the original are each named by the signed manifest, so
/// a second address under one of these roles is a contradiction rather than an addition.
/// Derivatives and backups are plural by nature — an asset has a thumbnail *and* a preview.
fn is_singular(role: BlobRole) -> bool {
    matches!(
        role,
        BlobRole::Original | BlobRole::Metadata | BlobRole::Provenance
    )
}

/// Everything the double holds, behind one lock.
///
/// One lock rather than two maps with two locks, because the sequence counter and the row it
/// stamps must move together — that is the entire point (see [`super`]).
#[derive(Debug, Default)]
struct Inner {
    rows: BTreeMap<AssetId, AssetRow>,
    /// The highest sequence number each owner has minted. Absent means none.
    minted: BTreeMap<OwnerId, u64>,
}

impl Inner {
    /// Allocate `owner`'s next sequence number.
    ///
    /// Callable only with the lock held, which is what makes allocation order equal publication
    /// order. Starts at 1 so that `after = 0` means "I have seen nothing".
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
}
