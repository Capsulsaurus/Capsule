//! A deterministic in-memory blob store — a **test double**, never a deployment mode.
//!
//! design/filesystem/server.md makes the blob store a required service and names one backend:
//! the filesystem. What this type is for is the same thing [`crate::store::memory`] is for —
//! letting the surfaces above the port be tested without standing up a tree on disk — and it
//! earns that only by passing the same [`super::conformance`] suite the filesystem adapter does.
//!
//! Two properties make it a legitimate stand-in rather than merely a fast one:
//!
//! - **Every listing is sorted**, because the port's order is the contract. A `BTreeMap` keyed by
//!   [`ContentAddress`] gives content-address order for free, which is exactly the order the
//!   sharded walk produces — so a case that passes here and fails there is a real difference,
//!   not iteration noise.
//! - **Debris exists.** A store that could not hold anything but blobs would make
//!   [`super::conformance::enumeration_reports_what_is_not_a_blob_as_debris`] vacuous here and
//!   meaningful only on disk. [`InMemoryBlobStore::plant_debris`] is the seam the suite drives.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::sync::{Mutex, MutexGuard, PoisonError};

use super::address::ContentAddress;
use super::{
    BlobError, BlobFuture, BlobPage, BlobStat, BlobStore, Placement, QuarantineReason,
    QuarantinedBlob, check_upload_id, window,
};
use crate::store::UploadId;

/// Take a lock, recovering rather than propagating a poisoned one.
///
/// A test double must not turn one failed assertion inside a lock into a cascade of unrelated
/// failures, and `unwrap()` is denied workspace-wide besides.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Everything the double holds.
#[derive(Debug, Default)]
struct State {
    /// Open stages, keyed like `incoming/{upload_id}.bin`.
    staged: BTreeMap<UploadId, Vec<u8>>,
    /// Finalized blobs, keyed and therefore ordered by content address.
    blobs: BTreeMap<ContentAddress, Vec<u8>>,
    /// Blobs pulled for an operator, and why.
    held: BTreeMap<ContentAddress, QuarantineReason>,
    /// Names that are not blobs, as [`InMemoryBlobStore::plant_debris`] planted them.
    debris: BTreeSet<String>,
}

/// In-memory [`BlobStore`].
#[derive(Debug, Default)]
pub struct InMemoryBlobStore {
    state: Mutex<State>,
}

impl InMemoryBlobStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Plant an entry that is not a blob, named as the finalized store would see it.
    ///
    /// The double's answer to a crashed temp file or a stray directory on disk. Test-only in
    /// spirit and in fact: nothing in the server calls it.
    pub fn plant_debris(&self, relative: &str) {
        lock(&self.state).debris.insert(relative.to_owned());
    }

    /// The bytes staged for `upload`, for a test that wants to look without committing.
    pub fn staged_bytes(&self, upload: &UploadId) -> Option<Vec<u8>> {
        lock(&self.state).staged.get(upload).cloned()
    }
}

impl BlobStore for InMemoryBlobStore {
    fn begin<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, ()> {
        Box::pin(async move {
            check_upload_id(upload)?;
            lock(&self.state).staged.insert(upload.clone(), Vec::new());
            tracing::debug!(%upload, "staged an upload");
            Ok(())
        })
    }

    fn append<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
        bytes: &'a [u8],
    ) -> BlobFuture<'a, u64> {
        Box::pin(async move {
            check_upload_id(upload)?;
            let mut state = lock(&self.state);
            let staged = state
                .staged
                .get_mut(upload)
                .ok_or_else(|| BlobError::NotStaged {
                    upload: upload.clone(),
                })?;

            let actual = staged.len() as u64;
            if offset != actual {
                return Err(BlobError::OffsetMismatch {
                    upload: upload.clone(),
                    offset,
                    actual,
                });
            }

            staged.extend_from_slice(bytes);
            let length = staged.len() as u64;
            tracing::trace!(%upload, offset, appended = bytes.len(), length, "appended to a stage");
            Ok(length)
        })
    }

    fn truncate_staged<'a>(
        &'a self,
        upload: &'a UploadId,
        len: u64,
    ) -> BlobFuture<'a, Option<u64>> {
        Box::pin(async move {
            check_upload_id(upload)?;
            let mut state = lock(&self.state);
            let Some(staged) = state.staged.get_mut(upload) else {
                return Ok(None);
            };
            // Shrinks only; `truncate` on a shorter vector is already a no-op.
            staged.truncate(usize::try_from(len).unwrap_or(usize::MAX));
            Ok(Some(staged.len() as u64))
        })
    }

    fn staged_len<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, Option<u64>> {
        Box::pin(async move {
            check_upload_id(upload)?;
            Ok(lock(&self.state)
                .staged
                .get(upload)
                .map(|staged| staged.len() as u64))
        })
    }

    fn read_staged_at<'a>(
        &'a self,
        upload: &'a UploadId,
        offset: u64,
        len: usize,
    ) -> BlobFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            check_upload_id(upload)?;
            let state = lock(&self.state);
            let Some(bytes) = state.staged.get(upload) else {
                return Ok(None);
            };
            let (start, taken) = window(bytes.len() as u64, offset, len);
            Ok(Some(bytes[start..start + taken].to_vec()))
        })
    }

    fn abandon<'a>(&'a self, upload: &'a UploadId) -> BlobFuture<'a, bool> {
        Box::pin(async move {
            check_upload_id(upload)?;
            let removed = lock(&self.state).staged.remove(upload).is_some();
            if removed {
                tracing::debug!(%upload, "abandoned a stage");
            }
            Ok(removed)
        })
    }

    fn staged(&self) -> BlobFuture<'_, Vec<UploadId>> {
        Box::pin(async move { Ok(lock(&self.state).staged.keys().cloned().collect()) })
    }

    fn commit<'a>(
        &'a self,
        upload: &'a UploadId,
        address: &'a ContentAddress,
    ) -> BlobFuture<'a, Placement> {
        Box::pin(async move {
            check_upload_id(upload)?;
            let mut state = lock(&self.state);
            let bytes = state
                .staged
                .remove(upload)
                .ok_or_else(|| BlobError::NotStaged {
                    upload: upload.clone(),
                })?;

            if state.blobs.contains_key(address) {
                tracing::info!(%upload, %address, "committed onto an address already present");
                return Ok(Placement::AlreadyPresent);
            }

            let size = bytes.len();
            state.blobs.insert(address.clone(), bytes);
            tracing::info!(%upload, %address, size, "committed a blob");
            Ok(Placement::Stored)
        })
    }

    fn put<'a>(
        &'a self,
        address: &'a ContentAddress,
        bytes: &'a [u8],
    ) -> BlobFuture<'a, Placement> {
        Box::pin(async move {
            let mut state = lock(&self.state);
            if state.blobs.contains_key(address) {
                return Ok(Placement::AlreadyPresent);
            }
            state.blobs.insert(address.clone(), bytes.to_vec());
            tracing::info!(%address, size = bytes.len(), "stored a blob");
            Ok(Placement::Stored)
        })
    }

    fn stat<'a>(&'a self, address: &'a ContentAddress) -> BlobFuture<'a, Option<BlobStat>> {
        Box::pin(async move {
            Ok(lock(&self.state).blobs.get(address).map(|bytes| BlobStat {
                address: address.clone(),
                size: bytes.len() as u64,
            }))
        })
    }

    fn read_at<'a>(
        &'a self,
        address: &'a ContentAddress,
        offset: u64,
        len: usize,
    ) -> BlobFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            let state = lock(&self.state);
            let Some(bytes) = state.blobs.get(address) else {
                return Ok(None);
            };
            let (start, taken) = window(bytes.len() as u64, offset, len);
            Ok(Some(bytes[start..start + taken].to_vec()))
        })
    }

    fn enumerate<'a>(
        &'a self,
        after: Option<&'a ContentAddress>,
        limit: usize,
    ) -> BlobFuture<'a, BlobPage> {
        Box::pin(async move {
            let state = lock(&self.state);
            let lower = after.map_or(Bound::Unbounded, Bound::Excluded);
            let entries: Vec<BlobStat> = state
                .blobs
                .range((lower, Bound::Unbounded))
                .take(limit)
                .map(|(address, bytes)| BlobStat {
                    address: address.clone(),
                    size: bytes.len() as u64,
                })
                .collect();

            let next = if entries.len() == limit && limit > 0 {
                entries.last().map(|entry| entry.address.clone())
            } else {
                None
            };

            // Debris rides the page that ends the walk. The port asks only that a complete
            // enumeration report each entry at least once; where a backend encounters one is its
            // own business, and this one has no tree to encounter it in.
            let debris = if next.is_none() {
                state.debris.iter().cloned().collect()
            } else {
                Vec::new()
            };

            Ok(BlobPage {
                entries,
                debris,
                next,
            })
        })
    }

    fn remove<'a>(&'a self, address: &'a ContentAddress) -> BlobFuture<'a, bool> {
        Box::pin(async move {
            let removed = lock(&self.state).blobs.remove(address).is_some();
            if removed {
                tracing::info!(%address, "removed a blob");
            }
            Ok(removed)
        })
    }

    fn quarantine<'a>(
        &'a self,
        address: &'a ContentAddress,
        reason: QuarantineReason,
    ) -> BlobFuture<'a, bool> {
        Box::pin(async move {
            let mut state = lock(&self.state);
            if state.blobs.remove(address).is_none() {
                return Ok(false);
            }
            tracing::warn!(%address, code = %reason.code, detail = %reason.detail, "quarantined a blob");
            state.held.insert(address.clone(), reason);
            Ok(true)
        })
    }

    fn quarantined(&self) -> BlobFuture<'_, Vec<QuarantinedBlob>> {
        Box::pin(async move {
            Ok(lock(&self.state)
                .held
                .iter()
                .map(|(address, reason)| QuarantinedBlob {
                    address: address.clone(),
                    reason: reason.clone(),
                })
                .collect())
        })
    }
}

impl super::conformance::Harness for InMemoryBlobStore {
    fn store(&self) -> &dyn BlobStore {
        self
    }

    fn plant_debris(&self, relative: &str) -> BlobFuture<'_, ()> {
        let relative = relative.to_owned();
        Box::pin(async move {
            lock(&self.state).debris.insert(relative);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests;
