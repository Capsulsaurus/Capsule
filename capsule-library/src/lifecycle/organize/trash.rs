//! The trash state machine (owner: [Organization — Deletion State Machine]): delete, restore,
//! purge, and the trash listing, each judged against the `retention_until` signed into the
//! asset's latest `delete` record rather than against an index column or a caller's clock
//! arithmetic.
//!
//! Every transition is a signed provenance record, so it syncs and survives a
//! [`rebuild_index`](crate::library::rebuild_index): the index row is a projection of the
//! chain, never the place a deletion is decided.
//!
//! [Organization — Deletion State Machine]: https://docs/design/organization/#deletion-state-machine

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use capsule_core::crypto::hash::Hash32;
use capsule_core::crypto::provenance::{Action, ProvenanceChain, ProvenanceRecord};
use jiff::Timestamp;
use uuid::Uuid;

use super::super::{AssetState, LifecycleError, Result, Workspace, asset_is_deleted, media_dir};
use crate::library::paths::{
    ThumbnailSize, meta_cache_path, thumbnail_path, transcode_h264_path, transcode_live_path,
};

/// One asset in the trash, as its latest signed `delete` record describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashEntry {
    /// The trashed asset.
    pub asset_id: Uuid,
    /// When the `delete` record was signed.
    pub deleted_at: Timestamp,
    /// The signed retention floor: a restore is refused at or after it, and a purge before it
    /// must first re-issue a shorter one. `None` for a `delete` that carries no readable
    /// window, which never expires and never refuses a restore, because a guessed deadline
    /// could only ever lose data.
    pub retention_until: Option<Timestamp>,
}

impl TrashEntry {
    /// Whether a `trash-restore` is still accepted at `now`: strictly before `retention_until`.
    pub fn is_restorable_at(&self, now: Timestamp) -> bool {
        self.retention_until.is_none_or(|until| now < until)
    }

    /// Whether the retention window has closed at `now`, so a purge needs no re-issue.
    pub fn is_expired_at(&self, now: Timestamp) -> bool {
        self.retention_until.is_some_and(|until| now >= until)
    }
}

/// A sync that drained the feed to its head: the evidence a retention sweep needs before it
/// judges a deadline ([Peering — Sync]).
///
/// [Peering — Sync]: https://docs/design/peering/
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FeedDrain {
    /// The latest server `changed_at` the drain saw, or `None` for a drain that saw no entry.
    pub latest_changed_at: Option<Timestamp>,
    /// Every asset the drained feed lists (the assets the server has indexed), with the hash of
    /// the head record its latest entry carries, or `None` for an entry that carries none (a
    /// purged asset, or an older server). A record's hash is the SHA-256 of the entry's
    /// `manifest_cbor`, which is that record's canonical CBOR.
    ///
    /// It must cover the whole feed: a drain from the feed's start, or heads the caller kept
    /// across every cursored drain since one. A drain resumed from a cursor lists only what
    /// changed after it, so an asset whose `delete` an earlier sync indexed would be missing,
    /// judged held, and never expire.
    pub server_heads: BTreeMap<Uuid, Option<Hash32>>,
}

impl FeedDrain {
    /// The instant a deadline is judged at: the latest server `changed_at` the drain saw, and
    /// never this device's clock. A device clock running ahead would purge early, and its
    /// tombstone would then refuse a restore the server accepted in time; a late purge loses
    /// nothing. `None` for a drain that saw no entry: there is no reading of the server's clock
    /// to judge by, so nothing is judged expired.
    pub fn judged_at(&self) -> Option<Timestamp> {
        self.latest_changed_at
    }

    /// Whether the server has indexed the latest `delete` of `records` (an asset's chain, oldest
    /// first): the feed lists the asset, and the head it carries is that `delete` or a record
    /// after it. Anything else is a held delete, with no window running on the server: the
    /// asset is not listed, its head precedes the delete, the entry carries no record, or the
    /// head is one this chain does not hold.
    fn indexes_delete_of(&self, asset_id: &Uuid, records: &[ProvenanceRecord]) -> bool {
        let Some(delete) = records
            .iter()
            .rposition(|rec| rec.manifest.core.action == Action::Delete)
        else {
            return false;
        };
        let Some(Some(head)) = self.server_heads.get(asset_id) else {
            return false;
        };
        records
            .iter()
            .position(|rec| rec.record_hash() == *head)
            .is_some_and(|at| at >= delete)
    }
}

/// What a [`purge`](Workspace::purge) did.
#[derive(Debug, Clone)]
pub struct PurgeOutcome {
    /// The purged asset.
    pub asset_id: Uuid,
    /// The signed shorter-retention `delete` appended before an early purge, so the server may
    /// purge too. `None` when the window had already closed. It is the asset's final chain
    /// head and the record a client sends to the album's ops surface; a delivery that failed
    /// resends it from [`Workspace::purge_record`].
    pub reissue: Option<ProvenanceRecord>,
    /// The files removed, for the caller's log.
    pub removed_files: usize,
}

/// The trash entry `asset` is in, or `None` when its chain's lifecycle state is not deleted.
fn entry_of(asset: &AssetState) -> Option<TrashEntry> {
    if !asset_is_deleted(asset) {
        return None;
    }
    let record = asset
        .chain
        .records()
        .iter()
        .rev()
        .find(|rec| rec.manifest.core.action == Action::Delete)?;
    let core = &record.manifest.core;
    let retention_until = core
        .retention_until
        .as_deref()
        .and_then(|until| match until.parse::<Timestamp>() {
            Ok(until) => Some(until),
            Err(error) => {
                tracing::warn!(asset_id = %asset.asset_id, retention_until = %until, %error, "trash: unreadable retention_until; treating the window as open");
                None
            }
        });
    Some(TrashEntry {
        asset_id: asset.asset_id,
        deleted_at: core.timestamp.parse().unwrap_or(Timestamp::UNIX_EPOCH),
        retention_until,
    })
}

/// Remove `path` if it exists. A missing file is not an error: a purge that is retried after a
/// partial one finds some files already gone.
fn remove_if_present(path: &Path, removed: &mut usize) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            *removed += 1;
            tracing::trace!(path = %path.display(), "purge: removed");
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(LifecycleError::Io(format!(
            "purge: removing {}: {error}",
            path.display()
        ))),
    }
}

impl Workspace {
    /// Soft-delete: emit a `delete` record carrying a signed retention window.
    pub fn soft_delete(&mut self, asset_id: &Uuid, retain_days: i64) -> Result<()> {
        // Timestamp arithmetic is absolute, so a retention "day" is exactly 24 h — the
        // correct semantic for a UTC retention window.
        let until = (Timestamp::now()
            + jiff::SignedDuration::from_hours(retain_days.saturating_mul(24)))
        .to_string();
        self.append_lifecycle(asset_id, Action::Delete, Some(until), |_, _| {})
    }

    /// Restore a soft-deleted asset by this device's clock; see [`restore_at`](Self::restore_at).
    pub fn restore(&mut self, asset_id: &Uuid) -> Result<()> {
        self.restore_at(asset_id, Timestamp::now())
    }

    /// Restore a soft-deleted asset: emit a `trash-restore` record, judged at `now`.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::NotFound`] for an unknown asset, [`LifecycleError::NotInTrash`] for one
    /// that is not in the trash, and [`LifecycleError::RetentionElapsed`] at or after its signed
    /// `retention_until`. A refusal signs nothing.
    #[tracing::instrument(skip(self), fields(asset_id = %asset_id, now = %now))]
    pub fn restore_at(&mut self, asset_id: &Uuid, now: Timestamp) -> Result<()> {
        let entry = self.require_trashed(asset_id)?;
        if !entry.is_restorable_at(now) {
            let until = entry.retention_until.unwrap_or_default().to_string();
            tracing::warn!(retention_until = %until, "trash: restore refused, the retention window has closed");
            return Err(LifecycleError::RetentionElapsed {
                asset_id: *asset_id,
                retention_until: until,
            });
        }
        self.append_lifecycle(asset_id, Action::TrashRestore, None, |_, _| {})?;
        tracing::info!("trash: restored");
        Ok(())
    }

    /// The trash entry `asset_id` is in, or `None` when it is unknown or not in the trash.
    pub fn trash_entry(&self, asset_id: &Uuid) -> Option<TrashEntry> {
        self.assets.get(asset_id).and_then(entry_of)
    }

    /// Every asset in the trash, most recently deleted first (ties by id, for determinism).
    pub fn trash(&self) -> Vec<TrashEntry> {
        let mut entries: Vec<TrashEntry> = self.assets.values().filter_map(entry_of).collect();
        entries.sort_by(|a, b| {
            b.deleted_at
                .cmp(&a.deleted_at)
                .then(a.asset_id.cmp(&b.asset_id))
        });
        entries
    }

    /// The trash entries whose signed retention window has closed at `now`: the retention
    /// sweep's input. Judged by `retention_until` alone, never by how long ago the delete was.
    pub fn expired_trash(&self, now: Timestamp) -> Vec<TrashEntry> {
        self.trash()
            .into_iter()
            .filter(|entry| entry.is_expired_at(now))
            .collect()
    }

    /// The retention sweep's input after a sync: [`expired_trash`](Self::expired_trash) judged
    /// at the instant [`FeedDrain::judged_at`] gives, less every held delete. Nothing at all
    /// when no sync drained the feed to its head (`drain` is `None`) or the drain saw no
    /// entry, which leaves no reading of the server's clock to judge by.
    ///
    /// Waiting for the drain means a restore on a later feed page lands before any deadline is
    /// judged, so the sweep never purges an asset that page restored. Judging by the server's
    /// clock alone means a device clock running ahead never purges early. A held delete (one
    /// the server has not indexed, [`FeedDrain::server_heads`]) never expires: the server has
    /// no window running for it, so only the user's own [`purge`](Self::purge) removes it.
    pub fn expired_trash_after_drain(&self, drain: Option<&FeedDrain>) -> Vec<TrashEntry> {
        let Some(drain) = drain else {
            tracing::debug!("trash: no drained feed; the retention sweep purges nothing");
            return Vec::new();
        };
        let Some(now) = drain.judged_at() else {
            tracing::debug!(
                "trash: the drain saw no server clock reading; the retention sweep purges nothing"
            );
            return Vec::new();
        };
        let mut held = 0usize;
        let expired: Vec<TrashEntry> = self
            .expired_trash(now)
            .into_iter()
            .filter(|entry| {
                let indexed = self.assets.get(&entry.asset_id).is_some_and(|asset| {
                    drain.indexes_delete_of(&entry.asset_id, asset.chain.records())
                });
                if !indexed {
                    held += 1;
                    tracing::debug!(asset_id = %entry.asset_id, retention_until = ?entry.retention_until, "trash: the server has not indexed this delete; held, so it never expires");
                }
                indexed
            })
            .collect();
        tracing::debug!(%now, expired = expired.len(), held, "trash: retention sweep judged after a drain");
        expired
    }

    /// Every asset this workspace has purged: the peering tombstones a backup import or a
    /// peer ingest must never resurrect.
    pub fn tombstones(&self) -> BTreeSet<Uuid> {
        self.purged.keys().copied().collect()
    }

    /// The head of a purged asset's kept provenance chain: the `delete` that ended it. After
    /// an early purge this is the shorter-retention re-issue, so a delivery that failed can be
    /// retried from it after [`purge`](Self::purge) returned, and after a reopen. `None` for
    /// an asset this workspace has not purged.
    pub fn purge_record(&self, asset_id: &Uuid) -> Option<&ProvenanceRecord> {
        self.purged
            .get(asset_id)
            .and_then(|chain| chain.records().last())
    }

    /// A purged asset's whole kept provenance chain, oldest first, headed by
    /// [`purge_record`](Self::purge_record). A held delivery reads the stretch after the
    /// server's head from it, so a `delete` the server never saw (and the shorter-retention
    /// re-issue an early purge appended) is sent from the tombstone once another device's
    /// upload of the create is indexed. `None` for an asset this workspace has not purged.
    pub fn purged_chain(&self, asset_id: &Uuid) -> Option<&[ProvenanceRecord]> {
        self.purged.get(asset_id).map(ProvenanceChain::records)
    }

    /// Purge a trashed asset by this device's clock; see [`purge_at`](Self::purge_at).
    pub fn purge(&mut self, asset_id: &Uuid) -> Result<PurgeOutcome> {
        self.purge_at(asset_id, Timestamp::now())
    }

    /// Hard-delete a trashed asset, judged at `now`.
    ///
    /// Before `retention_until` (an early purge, which the user asked for), a `delete` whose
    /// `retention_until` is `now` is signed and appended first: the server refuses a purge
    /// before the signed floor, so without the re-issue the server would keep the asset for the
    /// rest of the old window.
    ///
    /// Then the original, every derivative, every local cache entry, the signed sidecar and the
    /// sealed metadata blob are removed, and the asset leaves the index and this workspace. The
    /// provenance chain stays on disk: it is the record that the asset was deleted, and without
    /// it the purge could not be told from a lost file. The original goes first, so a failure
    /// part-way leaves the asset in the trash for a retry rather than its bytes behind a
    /// vanished row.
    ///
    /// # Errors
    ///
    /// [`LifecycleError::NotFound`] for an unknown asset, [`LifecycleError::NotInTrash`] for one
    /// that is not in the trash (a purge never skips the trash), [`LifecycleError::Io`] when a
    /// file cannot be removed, and [`LifecycleError::Db`] when the index cannot be updated.
    #[tracing::instrument(skip(self), fields(asset_id = %asset_id, now = %now))]
    pub fn purge_at(&mut self, asset_id: &Uuid, now: Timestamp) -> Result<PurgeOutcome> {
        let entry = self.require_trashed(asset_id)?;
        let reissue = if entry.is_expired_at(now) {
            None
        } else {
            self.append_lifecycle(asset_id, Action::Delete, Some(now.to_string()), |_, _| {})?;
            tracing::info!(previous = ?entry.retention_until, "trash: early purge; signed a shorter-retention delete");
            self.assets[asset_id].chain.records().last().cloned()
        };

        let removed_files = self.remove_asset_files(asset_id)?;
        let id = asset_id.to_string();
        let db = &self.library.db;
        db.purge_asset(&id)
            .and_then(|()| db.delete_embeddings_for(&id))
            .map_err(|e| LifecycleError::Db(e.to_string()))?;
        if let Some(asset) = self.assets.remove(asset_id) {
            // The peering tombstone: the kept chain, headed by its `delete`, which a reopen
            // reads back from the provenance file.
            self.purged.insert(*asset_id, asset.chain);
        }
        tracing::info!(
            removed_files,
            reissued = reissue.is_some(),
            "trash: purged; provenance chain kept"
        );
        Ok(PurgeOutcome {
            asset_id: *asset_id,
            reissue,
            removed_files,
        })
    }

    /// The asset's trash entry, or why it has none.
    fn require_trashed(&self, asset_id: &Uuid) -> Result<TrashEntry> {
        let asset = self
            .assets
            .get(asset_id)
            .ok_or_else(|| LifecycleError::NotFound(format!("asset {asset_id}")))?;
        entry_of(asset).ok_or(LifecycleError::NotInTrash(*asset_id))
    }

    /// Remove every file of `asset_id` but its provenance, receipt and attestation logs.
    fn remove_asset_files(&self, asset_id: &Uuid) -> Result<usize> {
        let asset = &self.assets[asset_id];
        let mut removed = 0;
        remove_if_present(&self.media_path(asset), &mut removed)?;

        let stem = format!("{}.", asset_id.simple());
        let derivatives = media_dir(&self.root, asset.capture_utc).join("derivatives");
        match fs::read_dir(&derivatives) {
            Ok(entries) => {
                for entry in entries.filter_map(std::result::Result::ok) {
                    if entry.file_name().to_string_lossy().starts_with(&stem) {
                        remove_if_present(&entry.path(), &mut removed)?;
                    }
                }
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(LifecycleError::Io(format!(
                    "purge: listing {}: {error}",
                    derivatives.display()
                )));
            }
        }

        for path in self.cache_paths(asset_id)? {
            remove_if_present(&path, &mut removed)?;
        }

        remove_if_present(&self.sidecar_path(asset), &mut removed)?;
        remove_if_present(&self.metadata_blob_path(asset), &mut removed)?;
        remove_if_present(&self.create_metadata_blob_path_of(asset), &mut removed)?;
        Ok(removed)
    }

    /// Every local cache path `asset_id` may occupy: the `index/` tree's fixed layout and every
    /// path the cache table recorded for it. A recorded path outside this library is skipped
    /// rather than trusted, so a corrupt row cannot point a purge at someone else's file.
    fn cache_paths(&self, asset_id: &Uuid) -> Result<Vec<PathBuf>> {
        let mut paths = Vec::new();
        for size in [
            ThumbnailSize::Xs,
            ThumbnailSize::S,
            ThumbnailSize::M,
            ThumbnailSize::L,
            ThumbnailSize::Xl,
            ThumbnailSize::O,
        ] {
            let base = thumbnail_path(&self.root, asset_id, size);
            for ext in ["jxl", "webp"] {
                paths.push(base.with_extension(ext));
            }
        }
        paths.push(meta_cache_path(&self.root, asset_id));
        paths.push(transcode_h264_path(&self.root, asset_id));
        paths.push(transcode_live_path(&self.root, asset_id));

        let rows = self
            .library
            .db
            .representations_for(&asset_id.to_string())
            .map_err(|e| LifecycleError::Db(e.to_string()))?;
        for row in rows {
            let recorded = Path::new(&row.path);
            let path = self.root.join(recorded);
            let climbs = recorded
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir));
            if path.starts_with(&self.root) && !climbs {
                paths.push(path);
            } else {
                tracing::warn!(path = %row.path, tier = %row.tier, "purge: cache row names a path outside the library; left alone");
            }
        }
        Ok(paths)
    }
}

#[cfg(test)]
mod tests;
